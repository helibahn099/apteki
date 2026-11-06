//! Аптеки: публичный список, API веб-панели аптеки и администрирование.
//!
//! Панель (отдельное Next.js-приложение) использует:
//! - сотрудник: входящие запросы и ответ «есть / частично / нет» + количество + цена;
//! - администратор аптеки: статистика активности и количество клиентов.
//!
//! Аптека НИКОГДА не получает номер телефона или имя пользователя.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::modules::auth::{normalize_phone, AuthUser, Role};
use crate::modules::geo::GeoPoint;
use crate::modules::integrations::PharmacyAnswer;
use crate::modules::medication::{load_order_medications, MedicationRow};
use crate::modules::search::responses::{apply_answer, ApplyOutcome};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Публичный список аптек
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PharmacyListItem {
    pub id: Uuid,
    pub name: String,
    pub chain_name: Option<String>,
    pub locations_count: i64,
}

/// GET /pharmacies — активные аптеки и количество точек.
async fn list_pharmacies(State(state): State<AppState>) -> AppResult<Json<Vec<PharmacyListItem>>> {
    let rows = sqlx::query_as(
        "SELECT p.id, p.name, p.chain_name, COUNT(l.id) AS locations_count
         FROM pharmacies p LEFT JOIN pharmacy_locations l ON l.pharmacy_id = p.id AND l.is_active
         WHERE p.is_active GROUP BY p.id ORDER BY p.name",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// Панель аптеки: входящие запросы
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RequestsQuery {
    /// pending | answered | expired | failed (по умолчанию pending)
    pub status: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PanelRequest {
    pub id: Uuid,
    pub search_id: Uuid,
    pub location_id: Uuid,
    pub address: String,
    pub status: String,
    pub availability: Option<String>,
    pub match_status: Option<String>,
    pub total_price_minor: Option<i64>,
    pub items_count: i64,
    pub requested_at: DateTime<Utc>,
    pub answered_at: Option<DateTime<Utc>>,
    /// До какого времени можно ответить.
    pub expires_at: DateTime<Utc>,
}

/// Позиция запроса, видимая аптеке (только данные препарата).
#[derive(Debug, Serialize)]
pub struct PanelMedication {
    pub medication_id: Uuid,
    pub position: i32,
    pub name: String,
    pub active_substance: Option<String>,
    pub dosage: Option<String>,
    pub concentration: Option<String>,
    pub form: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: i32,
    pub unit: String,
}

impl From<MedicationRow> for PanelMedication {
    fn from(m: MedicationRow) -> Self {
        Self {
            medication_id: m.id,
            position: m.position,
            name: m.name,
            active_substance: m.active_substance,
            dosage: m.dosage,
            concentration: m.concentration,
            form: m.form,
            manufacturer: m.manufacturer,
            quantity: m.quantity,
            unit: m.unit,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PanelRequestDetail {
    #[serde(flatten)]
    pub request: PanelRequest,
    pub medications: Vec<PanelMedication>,
    pub answer_items: serde_json::Value,
}

const PANEL_REQUEST_SELECT: &str = "SELECT r.id, r.search_order_id AS search_id, r.location_id, l.address, r.status,
        r.availability, r.match_status, r.total_price_minor,
        (SELECT COUNT(*) FROM medications m WHERE m.search_order_id = r.search_order_id) AS items_count,
        r.requested_at, r.answered_at, r.requested_at + make_interval(secs => $2) AS expires_at
     FROM pharmacy_responses r JOIN pharmacy_locations l ON l.id = r.location_id";

const REQUEST_STATUSES: &[&str] = &["pending", "answered", "expired", "failed"];

/// GET /pharmacy-panel/requests
async fn list_requests(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<RequestsQuery>,
) -> AppResult<Json<Vec<PanelRequest>>> {
    let pharmacy_id = user.require_pharmacy_member()?;
    let status = q.status.unwrap_or_else(|| "pending".into());
    if !REQUEST_STATUSES.contains(&status.as_str()) {
        return Err(AppError::Validation("недопустимый статус".into()));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let sql = format!(
        "{PANEL_REQUEST_SELECT} WHERE r.pharmacy_id = $1 AND r.status = $3 ORDER BY r.requested_at DESC LIMIT $4"
    );
    let rows = sqlx::query_as(&sql)
        .bind(pharmacy_id)
        .bind(state.config.pharmacy_response_ttl.as_secs() as f64)
        .bind(status)
        .bind(limit)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(rows))
}

async fn load_panel_request(state: &AppState, pharmacy_id: Uuid, id: Uuid) -> AppResult<PanelRequest> {
    let sql = format!("{PANEL_REQUEST_SELECT} WHERE r.id = $1 AND r.pharmacy_id = $3");
    sqlx::query_as(&sql)
        .bind(id)
        .bind(state.config.pharmacy_response_ttl.as_secs() as f64)
        .bind(pharmacy_id)
        .fetch_optional(&state.db)
        .await?
        // Чужой запрос неотличим от несуществующего.
        .ok_or_else(|| AppError::NotFound("запрос не найден".into()))
}

/// GET /pharmacy-panel/requests/{id}
async fn get_request(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<PanelRequestDetail>> {
    let pharmacy_id = user.require_pharmacy_member()?;
    let request = load_panel_request(&state, pharmacy_id, id).await?;
    let medications = load_order_medications(&state.db, request.search_id)
        .await?
        .into_iter()
        .map(PanelMedication::from)
        .collect();
    let (answer_items,): (serde_json::Value,) = sqlx::query_as("SELECT items FROM pharmacy_responses WHERE id = $1")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(PanelRequestDetail {
        request,
        medications,
        answer_items,
    }))
}

/// POST /pharmacy-panel/requests/{id}/answer — ручное подтверждение сотрудником.
async fn answer_request(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(answer): Json<PharmacyAnswer>,
) -> AppResult<Json<ApplyOutcome>> {
    let pharmacy_id = user.require_pharmacy_member()?;
    let req = load_panel_request(&state, pharmacy_id, id).await?;
    if req.status == "pending" && req.expires_at < Utc::now() {
        return Err(AppError::Conflict("время ответа на запрос истекло".into()));
    }
    let outcome = apply_answer(&state, id, &answer, Some(user.user_id)).await?;
    Ok(Json(outcome))
}

// ---------------------------------------------------------------------------
// Панель аптеки: статистика (только администратор аптеки)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StatsQuery {
    /// Период в днях (1..365, по умолчанию 30).
    pub days: Option<i32>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PharmacyStats {
    pub period_days: i32,
    pub requests_total: i64,
    pub requests_answered: i64,
    pub requests_expired: i64,
    pub requests_pending: i64,
    pub full_matches: i64,
    pub partial_matches: i64,
    pub no_matches: i64,
    /// Доля ответов, %.
    pub answer_rate_percent: f64,
    /// Среднее время ответа, сек.
    pub avg_answer_secs: Option<f64>,
    /// Уникальные клиенты (по числу различных пользователей, без раскрытия данных).
    pub unique_clients: i64,
    pub chats_total: i64,
    pub chats_active: i64,
    pub staff_count: i64,
}

/// Доля ответов в процентах с округлением до 0.1.
pub fn answer_rate(answered: i64, total: i64) -> f64 {
    if total <= 0 {
        return 0.0;
    }
    ((answered as f64 / total as f64) * 1000.0).round() / 10.0
}

/// GET /pharmacy-panel/stats
async fn stats(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<StatsQuery>,
) -> AppResult<Json<PharmacyStats>> {
    let pharmacy_id = user.require_pharmacy_admin()?;
    let days = q.days.unwrap_or(30).clamp(1, 365);
    let mut s: PharmacyStats = sqlx::query_as(
        "WITH r AS (
            SELECT pr.*, so.user_id FROM pharmacy_responses pr
            JOIN search_orders so ON so.id = pr.search_order_id
            WHERE pr.pharmacy_id = $1 AND pr.requested_at > now() - make_interval(days => $2)
         )
         SELECT $2 AS period_days,
            (SELECT COUNT(*) FROM r) AS requests_total,
            (SELECT COUNT(*) FROM r WHERE status = 'answered') AS requests_answered,
            (SELECT COUNT(*) FROM r WHERE status = 'expired') AS requests_expired,
            (SELECT COUNT(*) FROM r WHERE status = 'pending') AS requests_pending,
            (SELECT COUNT(*) FROM r WHERE match_status = 'full_match') AS full_matches,
            (SELECT COUNT(*) FROM r WHERE match_status = 'partial_match') AS partial_matches,
            (SELECT COUNT(*) FROM r WHERE match_status = 'no_match') AS no_matches,
            0::float8 AS answer_rate_percent,
            (SELECT AVG(EXTRACT(EPOCH FROM answered_at - requested_at))::float8 FROM r WHERE answered_at IS NOT NULL)
                AS avg_answer_secs,
            (SELECT COUNT(DISTINCT user_id) FROM r) AS unique_clients,
            (SELECT COUNT(*) FROM chats c WHERE c.pharmacy_id = $1
                AND c.created_at > now() - make_interval(days => $2)) AS chats_total,
            (SELECT COUNT(*) FROM chats c WHERE c.pharmacy_id = $1 AND c.status = 'active'
                AND c.active_until > now()) AS chats_active,
            (SELECT COUNT(*) FROM users u WHERE u.pharmacy_id = $1
                AND u.role IN ('pharmacy_staff', 'pharmacy_admin')) AS staff_count",
    )
    .bind(pharmacy_id)
    .bind(days)
    .fetch_one(&state.db)
    .await?;
    s.answer_rate_percent = answer_rate(s.requests_answered, s.requests_total);
    Ok(Json(s))
}

// ---------------------------------------------------------------------------
// Администрирование (роль admin)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreatePharmacy {
    pub name: String,
    pub chain_name: Option<String>,
    pub phone: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreatedId {
    pub id: Uuid,
}

fn validate_name(name: &str, field: &str, max: usize) -> AppResult<String> {
    let v = name.trim();
    if v.is_empty() || v.chars().count() > max {
        return Err(AppError::Validation(format!("поле «{field}»: от 1 до {max} символов")));
    }
    Ok(v.to_string())
}

/// POST /admin/pharmacies
async fn admin_create_pharmacy(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreatePharmacy>,
) -> AppResult<(StatusCode, Json<CreatedId>)> {
    user.require_admin()?;
    let name = validate_name(&req.name, "name", 200)?;
    let (id,): (Uuid,) =
        sqlx::query_as("INSERT INTO pharmacies (name, chain_name, phone) VALUES ($1, $2, $3) RETURNING id")
            .bind(name)
            .bind(req.chain_name.as_deref().map(str::trim).filter(|s| !s.is_empty()))
            .bind(req.phone.as_deref().map(str::trim).filter(|s| !s.is_empty()))
            .fetch_one(&state.db)
            .await?;
    tracing::info!(pharmacy_id = %id, "аптека создана");
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[derive(Debug, Deserialize)]
pub struct CreateLocation {
    pub address: String,
    pub lat: f64,
    pub lon: f64,
    pub working_hours: Option<String>,
    pub phone: Option<String>,
    pub external_id: Option<String>,
}

async fn ensure_pharmacy(state: &AppState, id: Uuid) -> AppResult<()> {
    let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM pharmacies WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    exists
        .map(|_| ())
        .ok_or_else(|| AppError::NotFound("аптека не найдена".into()))
}

/// POST /admin/pharmacies/{id}/locations
async fn admin_create_location(
    State(state): State<AppState>,
    user: AuthUser,
    Path(pharmacy_id): Path<Uuid>,
    Json(req): Json<CreateLocation>,
) -> AppResult<(StatusCode, Json<CreatedId>)> {
    user.require_admin()?;
    ensure_pharmacy(&state, pharmacy_id).await?;
    let address = validate_name(&req.address, "address", 300)?;
    let point = GeoPoint {
        lat: req.lat,
        lon: req.lon,
    };
    if !point.is_valid() {
        return Err(AppError::Validation("некорректные координаты".into()));
    }
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO pharmacy_locations (pharmacy_id, address, city, location, working_hours, phone, external_id)
         VALUES ($1, $2, $3, ST_SetSRID(ST_MakePoint($4, $5), 4326)::geography, $6, $7, $8) RETURNING id",
    )
    .bind(pharmacy_id)
    .bind(address)
    .bind(&state.config.search.city_name)
    .bind(point.lon)
    .bind(point.lat)
    .bind(req.working_hours)
    .bind(req.phone)
    .bind(req.external_id)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[derive(Debug, Deserialize)]
pub struct SetIntegration {
    /// Тип адаптера: mock, manual или новый зарегистрированный.
    pub kind: String,
    #[serde(default)]
    pub config: serde_json::Value,
    pub is_enabled: Option<bool>,
}

/// PUT /admin/pharmacies/{id}/integration
async fn admin_set_integration(
    State(state): State<AppState>,
    user: AuthUser,
    Path(pharmacy_id): Path<Uuid>,
    Json(req): Json<SetIntegration>,
) -> AppResult<StatusCode> {
    user.require_admin()?;
    ensure_pharmacy(&state, pharmacy_id).await?;
    if state.adapters.resolve(Some(&req.kind)).is_none() {
        return Err(AppError::Validation(format!(
            "неизвестный тип интеграции; доступны: {}",
            state.adapters.kinds().join(", ")
        )));
    }
    let config = if req.config.is_null() {
        serde_json::json!({})
    } else {
        req.config
    };
    if !config.is_object() {
        return Err(AppError::Validation("config должен быть JSON-объектом".into()));
    }
    sqlx::query(
        "INSERT INTO integrations (pharmacy_id, kind, config, is_enabled) VALUES ($1, $2, $3, $4)
         ON CONFLICT (pharmacy_id) DO UPDATE SET kind = EXCLUDED.kind, config = EXCLUDED.config,
            is_enabled = EXCLUDED.is_enabled, updated_at = now()",
    )
    .bind(pharmacy_id)
    .bind(&req.kind)
    .bind(config)
    .bind(req.is_enabled.unwrap_or(true))
    .execute(&state.db)
    .await?;
    tracing::info!(pharmacy_id = %pharmacy_id, kind = %req.kind, "интеграция обновлена");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct AssignStaff {
    pub phone: String,
    /// pharmacy_staff | pharmacy_admin
    pub role: String,
}

/// POST /admin/pharmacies/{id}/staff — назначить сотрудника по номеру телефона.
async fn admin_assign_staff(
    State(state): State<AppState>,
    user: AuthUser,
    Path(pharmacy_id): Path<Uuid>,
    Json(req): Json<AssignStaff>,
) -> AppResult<(StatusCode, Json<CreatedId>)> {
    user.require_admin()?;
    ensure_pharmacy(&state, pharmacy_id).await?;
    let role = Role::parse(&req.role)
        .filter(|r| matches!(r, Role::PharmacyStaff | Role::PharmacyAdmin))
        .ok_or_else(|| AppError::Validation("роль: pharmacy_staff или pharmacy_admin".into()))?;
    let phone = normalize_phone(&req.phone).ok_or_else(|| AppError::Validation("некорректный номер".into()))?;
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO users (phone, role, pharmacy_id) VALUES ($1, $2, $3)
         ON CONFLICT (phone) DO UPDATE SET role = EXCLUDED.role, pharmacy_id = EXCLUDED.pharmacy_id, updated_at = now()
         RETURNING id",
    )
    .bind(phone)
    .bind(role.as_str())
    .bind(pharmacy_id)
    .fetch_one(&state.db)
    .await?;
    tracing::info!(pharmacy_id = %pharmacy_id, user_id = %id, role = role.as_str(), "назначен сотрудник аптеки");
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/pharmacies", get(list_pharmacies))
        .route("/pharmacy-panel/requests", get(list_requests))
        .route("/pharmacy-panel/requests/{id}", get(get_request))
        .route("/pharmacy-panel/requests/{id}/answer", post(answer_request))
        .route("/pharmacy-panel/stats", get(stats))
        .route("/admin/pharmacies", post(admin_create_pharmacy))
        .route("/admin/pharmacies/{id}/locations", post(admin_create_location))
        .route("/admin/pharmacies/{id}/integration", put(admin_set_integration))
        .route("/admin/pharmacies/{id}/staff", post(admin_assign_staff))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_rate_calculation() {
        assert_eq!(answer_rate(0, 0), 0.0);
        assert_eq!(answer_rate(1, 3), 33.3);
        assert_eq!(answer_rate(5, 5), 100.0);
    }

    #[test]
    fn name_validation() {
        assert!(validate_name("  ", "name", 10).is_err());
        assert!(validate_name(&"x".repeat(11), "name", 10).is_err());
        assert_eq!(validate_name(" Неман ", "name", 10).unwrap(), "Неман");
    }

    #[test]
    fn panel_medication_has_no_user_fields() {
        let m = MedicationRow {
            id: Uuid::nil(),
            position: 0,
            catalog_id: None,
            name: "Парацетамол".into(),
            active_substance: None,
            dosage: Some("500 мг".into()),
            concentration: None,
            form: None,
            manufacturer: None,
            quantity: 2,
            unit: "шт".into(),
        };
        let json = serde_json::to_string(&PanelMedication::from(m)).unwrap();
        assert!(!json.contains("phone") && !json.contains("user"));
    }
}
