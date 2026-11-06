//! Модуль Auth: регистрация и вход по номеру телефона (OTP), JWT-сессии,
//! обновление и отзыв токенов, проверка прав доступа.

pub mod jwt;
pub mod otp;

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use otp::OtpChannel;

/// Роли пользователей.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Пользователь мобильного приложения.
    User,
    /// Сотрудник аптеки: отвечает на запросы и в чатах.
    PharmacyStaff,
    /// Администратор аптеки: + статистика.
    PharmacyAdmin,
    /// Администратор системы.
    Admin,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::User => "user",
            Role::PharmacyStaff => "pharmacy_staff",
            Role::PharmacyAdmin => "pharmacy_admin",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Role::User),
            "pharmacy_staff" => Some(Role::PharmacyStaff),
            "pharmacy_admin" => Some(Role::PharmacyAdmin),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

/// Авторизованный пользователь (извлекается из `Authorization: Bearer <JWT>`).
/// Сессия проверяется в БД, поэтому отозванные токены перестают работать сразу.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub session_id: Uuid,
    pub role: Role,
    pub pharmacy_id: Option<Uuid>,
}

impl AuthUser {
    pub fn require_admin(&self) -> AppResult<()> {
        match self.role {
            Role::Admin => Ok(()),
            _ => Err(AppError::Forbidden("требуются права администратора".into())),
        }
    }

    /// Сотрудник или администратор аптеки → ID аптеки.
    pub fn require_pharmacy_member(&self) -> AppResult<Uuid> {
        match (self.role, self.pharmacy_id) {
            (Role::PharmacyStaff | Role::PharmacyAdmin, Some(pid)) => Ok(pid),
            _ => Err(AppError::Forbidden("доступно только сотрудникам аптеки".into())),
        }
    }

    pub fn require_pharmacy_admin(&self) -> AppResult<Uuid> {
        match (self.role, self.pharmacy_id) {
            (Role::PharmacyAdmin, Some(pid)) => Ok(pid),
            _ => Err(AppError::Forbidden("доступно только администратору аптеки".into())),
        }
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = bearer_token(&parts.headers).ok_or(AppError::Unauthorized)?;
        let claims = jwt::verify_access_token(&state.config.jwt_secret, token).ok_or(AppError::Unauthorized)?;
        let row: Option<(String, Option<Uuid>, bool)> = sqlx::query_as(
            "SELECT u.role, u.pharmacy_id, u.is_blocked
             FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.id = $1 AND s.user_id = $2 AND s.revoked_at IS NULL AND s.expires_at > now()",
        )
        .bind(claims.sid)
        .bind(claims.sub)
        .fetch_optional(&state.db)
        .await?;
        let (role, pharmacy_id, blocked) = row.ok_or(AppError::Unauthorized)?;
        if blocked {
            return Err(AppError::Forbidden("аккаунт заблокирован".into()));
        }
        let role = Role::parse(&role).ok_or(AppError::Unauthorized)?;
        tracing::Span::current().record("user_id", tracing::field::display(claims.sub));
        Ok(AuthUser {
            user_id: claims.sub,
            session_id: claims.sid,
            role,
            pharmacy_id,
        })
    }
}

/// Нормализация номера телефона в формат E.164.
/// Поддерживается местный формат Кыргызстана: `0555 123 456` → `+996555123456`.
pub fn normalize_phone(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let has_plus = trimmed.starts_with('+');
    let digits: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
    let allowed = trimmed
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '(' | ')'));
    if !allowed {
        return None;
    }
    let e164 = if has_plus {
        digits
    } else if digits.len() == 10 && digits.starts_with('0') {
        format!("996{}", &digits[1..])
    } else if digits.len() == 12 && digits.starts_with("996") {
        digits
    } else {
        return None;
    };
    if !(8..=15).contains(&e164.len()) || e164.starts_with('0') {
        return None;
    }
    Some(format!("+{e164}"))
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ChannelInfo {
    pub channel: OtpChannel,
    pub available: bool,
    /// Фактический способ доставки (debug_log, telegram_test_chat, ...).
    pub delivery: Option<&'static str>,
}

/// GET /api/v1/auth/channels — доступные каналы получения кода.
async fn channels(State(state): State<AppState>) -> Json<Vec<ChannelInfo>> {
    Json(
        OtpChannel::ALL
            .iter()
            .map(|&ch| ChannelInfo {
                channel: ch,
                available: state.otp_sender.supports(ch),
                delivery: state.otp_sender.delivery_mode(ch),
            })
            .collect(),
    )
}

#[derive(Debug, Deserialize)]
pub struct OtpRequest {
    pub phone: String,
    pub channel: OtpChannel,
}

#[derive(Debug, Serialize)]
pub struct OtpRequestResponse {
    pub channel: OtpChannel,
    pub delivery: &'static str,
    pub expires_in_secs: u64,
    pub resend_after_secs: u64,
}

/// Максимум кодов на один номер в час (защита от массовой рассылки).
const OTP_MAX_PER_HOUR: i64 = 5;

/// POST /api/v1/auth/otp/request — запрос одноразового кода.
async fn request_otp(
    State(state): State<AppState>,
    Json(req): Json<OtpRequest>,
) -> AppResult<Json<OtpRequestResponse>> {
    let cfg = &state.config;
    let phone =
        normalize_phone(&req.phone).ok_or_else(|| AppError::Validation("некорректный номер телефона".into()))?;
    let delivery = state
        .otp_sender
        .delivery_mode(req.channel)
        .ok_or_else(|| AppError::ServiceUnavailable(format!("канал {} пока недоступен", req.channel.as_str())))?;

    let (last_created, per_hour): (Option<chrono::DateTime<Utc>>, i64) = sqlx::query_as(
        "SELECT max(created_at), count(*) FILTER (WHERE created_at > now() - interval '1 hour')
         FROM otp_codes WHERE phone = $1",
    )
    .bind(&phone)
    .fetch_one(&state.db)
    .await?;
    if let Some(last) = last_created {
        let resend = ChronoDuration::from_std(cfg.otp_resend_interval).unwrap_or_default();
        let elapsed = Utc::now() - last;
        if elapsed < resend {
            let retry = (resend - elapsed).num_seconds().max(1) as u64;
            return Err(AppError::TooManyRequests {
                retry_after_secs: retry,
            });
        }
    }
    if per_hour >= OTP_MAX_PER_HOUR {
        return Err(AppError::TooManyRequests { retry_after_secs: 3600 });
    }

    let code = jwt::generate_otp(cfg.otp_length);
    let code_hash = jwt::hash_otp(&cfg.jwt_secret, &phone, &code);
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE otp_codes SET consumed_at = now() WHERE phone = $1 AND consumed_at IS NULL")
        .bind(&phone)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO otp_codes (phone, code_hash, channel, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(secs => $4))",
    )
    .bind(&phone)
    .bind(&code_hash)
    .bind(req.channel.as_str())
    .bind(cfg.otp_ttl.as_secs() as f64)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    if let Err(e) = state.otp_sender.send(&phone, &code, req.channel).await {
        tracing::error!(error = %e, channel = req.channel.as_str(), "не удалось отправить OTP");
        return Err(AppError::ServiceUnavailable(
            "не удалось отправить код, попробуйте позже".into(),
        ));
    }
    tracing::info!(channel = req.channel.as_str(), delivery, "OTP выдан");
    Ok(Json(OtpRequestResponse {
        channel: req.channel,
        delivery,
        expires_in_secs: cfg.otp_ttl.as_secs(),
        resend_after_secs: cfg.otp_resend_interval.as_secs(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct OtpVerify {
    pub phone: String,
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct AuthUserInfo {
    pub id: Uuid,
    pub role: Role,
    pub pharmacy_id: Option<Uuid>,
    pub is_new: bool,
}

#[derive(Debug, Serialize)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str,
    pub expires_in: u64,
    pub user: AuthUserInfo,
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(200).collect())
}

/// Создаёт сессию и пару токенов.
async fn create_session(
    state: &AppState,
    user_id: Uuid,
    role: Role,
    pharmacy_id: Option<Uuid>,
    is_new: bool,
    headers: &HeaderMap,
) -> AppResult<TokenPair> {
    let cfg = &state.config;
    let refresh = jwt::generate_refresh_token();
    let session_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sessions (user_id, refresh_token_hash, user_agent, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(secs => $4)) RETURNING id",
    )
    .bind(user_id)
    .bind(jwt::hash_token(&refresh))
    .bind(user_agent(headers))
    .bind(cfg.jwt_refresh_ttl.as_secs() as f64)
    .fetch_one(&state.db)
    .await?;
    let access = jwt::issue_access_token(
        &cfg.jwt_secret,
        user_id,
        session_id,
        role.as_str(),
        cfg.jwt_access_ttl.as_secs() as i64,
    )?;
    Ok(TokenPair {
        access_token: access,
        refresh_token: refresh,
        token_type: "Bearer",
        expires_in: cfg.jwt_access_ttl.as_secs(),
        user: AuthUserInfo {
            id: user_id,
            role,
            pharmacy_id,
            is_new,
        },
    })
}

fn invalid_code() -> AppError {
    AppError::Validation("неверный или просроченный код".into())
}

/// POST /api/v1/auth/otp/verify — проверка кода; создаёт пользователя при первом входе.
async fn verify_otp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<OtpVerify>,
) -> AppResult<Json<TokenPair>> {
    let cfg = &state.config;
    let phone =
        normalize_phone(&req.phone).ok_or_else(|| AppError::Validation("некорректный номер телефона".into()))?;
    let code = req.code.trim();
    if code.len() != cfg.otp_length || !code.chars().all(|c| c.is_ascii_digit()) {
        return Err(invalid_code());
    }

    let mut tx = state.db.begin().await?;
    let otp: Option<(Uuid, String, i32)> = sqlx::query_as(
        "SELECT id, code_hash, attempts FROM otp_codes
         WHERE phone = $1 AND consumed_at IS NULL AND expires_at > now()
         ORDER BY created_at DESC LIMIT 1 FOR UPDATE",
    )
    .bind(&phone)
    .fetch_optional(&mut *tx)
    .await?;
    let (otp_id, stored_hash, attempts) = otp.ok_or_else(invalid_code)?;
    if attempts >= cfg.otp_max_attempts {
        sqlx::query("UPDATE otp_codes SET consumed_at = now() WHERE id = $1")
            .bind(otp_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Err(AppError::TooManyRequests { retry_after_secs: 60 });
    }
    if !jwt::constant_time_eq(&stored_hash, &jwt::hash_otp(&cfg.jwt_secret, &phone, code)) {
        let new_attempts = attempts + 1;
        let consume = new_attempts >= cfg.otp_max_attempts;
        sqlx::query(
            "UPDATE otp_codes SET attempts = $2, consumed_at = CASE WHEN $3 THEN now() ELSE consumed_at END WHERE id = $1",
        )
        .bind(otp_id)
        .bind(new_attempts)
        .bind(consume)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        tracing::warn!(attempts = new_attempts, "неверный OTP");
        return Err(invalid_code());
    }
    sqlx::query("UPDATE otp_codes SET consumed_at = now() WHERE id = $1")
        .bind(otp_id)
        .execute(&mut *tx)
        .await?;

    let is_admin_phone = cfg
        .admin_phones
        .iter()
        .any(|p| normalize_phone(p).as_deref() == Some(phone.as_str()));
    let (user_id, role, pharmacy_id, blocked, is_new): (Uuid, String, Option<Uuid>, bool, bool) = sqlx::query_as(
        "INSERT INTO users (phone, role) VALUES ($1, CASE WHEN $2 THEN 'admin' ELSE 'user' END)
         ON CONFLICT (phone) DO UPDATE SET
            updated_at = now(),
            role = CASE WHEN $2 AND users.role = 'user' THEN 'admin' ELSE users.role END
         RETURNING id, role, pharmacy_id, is_blocked, (xmax = 0) AS is_new",
    )
    .bind(&phone)
    .bind(is_admin_phone)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    if blocked {
        return Err(AppError::Forbidden("аккаунт заблокирован".into()));
    }
    let role = Role::parse(&role).ok_or_else(|| anyhow::anyhow!("неизвестная роль в БД"))?;
    let pair = create_session(&state, user_id, role, pharmacy_id, is_new, &headers).await?;
    tracing::info!(user_id = %user_id, is_new, role = role.as_str(), "пользователь вошёл");
    Ok(Json(pair))
}

#[derive(Debug, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// POST /api/v1/auth/refresh — ротация refresh-токена.
async fn refresh(State(state): State<AppState>, Json(req): Json<RefreshRequest>) -> AppResult<Json<TokenPair>> {
    let cfg = &state.config;
    let mut tx = state.db.begin().await?;
    let row: Option<(Uuid, Uuid, String, Option<Uuid>, bool)> = sqlx::query_as(
        "SELECT s.id, u.id, u.role, u.pharmacy_id, u.is_blocked
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.refresh_token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now()
         FOR UPDATE OF s",
    )
    .bind(jwt::hash_token(req.refresh_token.trim()))
    .fetch_optional(&mut *tx)
    .await?;
    let (session_id, user_id, role, pharmacy_id, blocked) = row.ok_or(AppError::Unauthorized)?;
    if blocked {
        return Err(AppError::Forbidden("аккаунт заблокирован".into()));
    }
    let new_refresh = jwt::generate_refresh_token();
    sqlx::query(
        "UPDATE sessions SET refresh_token_hash = $2, last_used_at = now(),
                expires_at = now() + make_interval(secs => $3) WHERE id = $1",
    )
    .bind(session_id)
    .bind(jwt::hash_token(&new_refresh))
    .bind(cfg.jwt_refresh_ttl.as_secs() as f64)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let role = Role::parse(&role).ok_or(AppError::Unauthorized)?;
    let access = jwt::issue_access_token(
        &cfg.jwt_secret,
        user_id,
        session_id,
        role.as_str(),
        cfg.jwt_access_ttl.as_secs() as i64,
    )?;
    Ok(Json(TokenPair {
        access_token: access,
        refresh_token: new_refresh,
        token_type: "Bearer",
        expires_in: cfg.jwt_access_ttl.as_secs(),
        user: AuthUserInfo {
            id: user_id,
            role,
            pharmacy_id,
            is_new: false,
        },
    }))
}

/// POST /api/v1/auth/logout — завершение текущей сессии.
async fn logout(State(state): State<AppState>, user: AuthUser) -> AppResult<StatusCode> {
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
        .bind(user.session_id)
        .execute(&state.db)
        .await?;
    tracing::info!(user_id = %user.user_id, "выход из аккаунта");
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/v1/auth/logout-all — завершение всех сессий (восстановление доступа).
async fn logout_all(State(state): State<AppState>, user: AuthUser) -> AppResult<StatusCode> {
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL")
        .bind(user.user_id)
        .execute(&state.db)
        .await?;
    tracing::info!(user_id = %user.user_id, "завершены все сессии");
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/channels", get(channels))
        .route("/auth/otp/request", post(request_otp))
        .route("/auth/otp/verify", post(verify_otp))
        .route("/auth/refresh", post(refresh))
        .route("/auth/logout", post(logout))
        .route("/auth/logout-all", post(logout_all))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_kyrgyz_numbers() {
        assert_eq!(normalize_phone("+996 555 123 456").as_deref(), Some("+996555123456"));
        assert_eq!(normalize_phone("0555 123-456").as_deref(), Some("+996555123456"));
        assert_eq!(normalize_phone("996555123456").as_deref(), Some("+996555123456"));
        assert_eq!(normalize_phone("+7 (912) 345-67-89").as_deref(), Some("+79123456789"));
    }

    #[test]
    fn rejects_invalid_numbers() {
        assert!(normalize_phone("").is_none());
        assert!(normalize_phone("abc").is_none());
        assert!(normalize_phone("12345").is_none());
        assert!(normalize_phone("+0555123456").is_none());
        assert!(normalize_phone("+996555123456; DROP").is_none());
    }

    #[test]
    fn role_roundtrip() {
        for r in [Role::User, Role::PharmacyStaff, Role::PharmacyAdmin, Role::Admin] {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        assert_eq!(Role::parse("root"), None);
    }

    #[test]
    fn role_guards() {
        let pid = Uuid::new_v4();
        let staff = AuthUser {
            user_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            role: Role::PharmacyStaff,
            pharmacy_id: Some(pid),
        };
        assert_eq!(staff.require_pharmacy_member().unwrap(), pid);
        assert!(staff.require_pharmacy_admin().is_err());
        assert!(staff.require_admin().is_err());
        let user = AuthUser {
            role: Role::User,
            pharmacy_id: None,
            ..staff.clone()
        };
        assert!(user.require_pharmacy_member().is_err());
    }
}
