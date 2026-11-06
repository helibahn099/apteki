//! Модуль Chat: переписка пользователя с аптекой в рамках поиска.
//!
//! * чат привязан к поисковому заказу и торговой точке аптеки;
//! * активен CHAT_ACTIVE_HOURS (48 ч) от момента успешной оплаты;
//! * после — только чтение, перемещается в архив;
//! * номер телефона пользователя аптеке НЕ передаётся (аптека видит только
//!   обезличенный псевдоним «Клиент #XXXX»);
//! * новые сообщения получаются опросом `?after=<время>` без перезапуска приложения.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::modules::auth::AuthUser;
use crate::modules::notifications::Notification;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatState {
    Active,
    Archived,
}

/// Состояние чата определяется временем, а не только флагом в БД:
/// даже если фоновая задача ещё не отработала, писать после срока нельзя.
pub fn chat_state(active_until: DateTime<Utc>, now: DateTime<Utc>) -> ChatState {
    if now < active_until {
        ChatState::Active
    } else {
        ChatState::Archived
    }
}

/// Обезличенный псевдоним клиента для аптеки (стабилен в пределах чата).
pub fn client_alias(chat_id: Uuid) -> String {
    let s = chat_id.simple().to_string();
    format!("Клиент #{}", s[..4].to_uppercase())
}

/// Создаёт чат (если ещё нет) и возвращает его ID.
pub async fn ensure_chat(
    conn: &mut PgConnection,
    order_id: Uuid,
    user_id: Uuid,
    pharmacy_id: Uuid,
    location_id: Uuid,
    response_id: Uuid,
    active_until: DateTime<Utc>,
) -> AppResult<Uuid> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO chats (search_order_id, pharmacy_id, location_id, pharmacy_response_id, user_id, active_until)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (search_order_id, location_id) DO NOTHING RETURNING id",
    )
    .bind(order_id)
    .bind(pharmacy_id)
    .bind(location_id)
    .bind(response_id)
    .bind(user_id)
    .bind(active_until)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(id) = inserted {
        sqlx::query("INSERT INTO messages (chat_id, sender_type, body) VALUES ($1, 'system', $2)")
            .bind(id)
            .bind("Аптека подтвердила наличие. Чат активен 48 часов с момента оплаты.")
            .execute(&mut *conn)
            .await?;
        return Ok(id);
    }
    Ok(
        sqlx::query_scalar("SELECT id FROM chats WHERE search_order_id = $1 AND location_id = $2")
            .bind(order_id)
            .bind(location_id)
            .fetch_one(&mut *conn)
            .await?,
    )
}

/// Перевод просроченных чатов в архив. Возвращает число заархивированных.
pub async fn archive_expired(db: &PgPool) -> AppResult<u64> {
    Ok(sqlx::query(
        "UPDATE chats SET status = 'archived', archived_at = now() WHERE status = 'active' AND active_until <= now()",
    )
    .execute(db)
    .await?
    .rows_affected())
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ChatRow {
    id: Uuid,
    search_order_id: Uuid,
    pharmacy_id: Uuid,
    location_id: Uuid,
    user_id: Uuid,
    active_until: DateTime<Utc>,
    created_at: DateTime<Utc>,
    pharmacy_name: String,
    address: String,
    phone: Option<String>,
    lat: f64,
    lon: f64,
    last_message_at: Option<DateTime<Utc>>,
}

const CHAT_SELECT: &str = "SELECT c.id, c.search_order_id, c.pharmacy_id, c.location_id, c.user_id, c.active_until,
        c.created_at, p.name AS pharmacy_name, l.address, COALESCE(l.phone, p.phone) AS phone,
        ST_Y(l.location::geometry) AS lat, ST_X(l.location::geometry) AS lon,
        (SELECT max(m.created_at) FROM messages m WHERE m.chat_id = c.id) AS last_message_at
    FROM chats c
    JOIN pharmacies p ON p.id = c.pharmacy_id
    JOIN pharmacy_locations l ON l.id = c.location_id";

/// Карточка чата для пользователя (закреплённая карточка аптеки).
#[derive(Debug, Serialize)]
pub struct ChatView {
    pub id: Uuid,
    pub search_id: Uuid,
    pub state: ChatState,
    pub can_send: bool,
    pub active_until: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub last_message_at: Option<DateTime<Utc>>,
    pub pharmacy: PharmacyCard,
}

#[derive(Debug, Serialize)]
pub struct PharmacyCard {
    pub pharmacy_id: Uuid,
    pub location_id: Uuid,
    pub name: String,
    pub address: String,
    pub phone: Option<String>,
    pub lat: f64,
    pub lon: f64,
}

/// Чат с точки зрения аптеки: без каких-либо данных пользователя.
#[derive(Debug, Serialize)]
pub struct PharmacyChatView {
    pub id: Uuid,
    pub search_id: Uuid,
    pub client_alias: String,
    pub location_id: Uuid,
    pub address: String,
    pub state: ChatState,
    pub can_send: bool,
    pub active_until: DateTime<Utc>,
    pub last_message_at: Option<DateTime<Utc>>,
}

impl ChatRow {
    fn state(&self) -> ChatState {
        chat_state(self.active_until, Utc::now())
    }

    fn user_view(&self) -> ChatView {
        let state = self.state();
        ChatView {
            id: self.id,
            search_id: self.search_order_id,
            state,
            can_send: state == ChatState::Active,
            active_until: self.active_until,
            created_at: self.created_at,
            last_message_at: self.last_message_at,
            pharmacy: PharmacyCard {
                pharmacy_id: self.pharmacy_id,
                location_id: self.location_id,
                name: self.pharmacy_name.clone(),
                address: self.address.clone(),
                phone: self.phone.clone(),
                lat: self.lat,
                lon: self.lon,
            },
        }
    }

    fn pharmacy_view(&self) -> PharmacyChatView {
        let state = self.state();
        PharmacyChatView {
            id: self.id,
            search_id: self.search_order_id,
            client_alias: client_alias(self.id),
            location_id: self.location_id,
            address: self.address.clone(),
            state,
            can_send: state == ChatState::Active,
            active_until: self.active_until,
            last_message_at: self.last_message_at,
        }
    }
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct MessageView {
    pub id: Uuid,
    pub sender_type: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ListChatsQuery {
    /// `true` — только архив, `false` — только активные, не задано — все.
    pub archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct MessagesQuery {
    /// Вернуть сообщения строго после этого момента (для опроса новых).
    pub after: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct SendMessage {
    pub body: String,
}

fn filter_by_archived<T>(items: Vec<(ChatState, T)>, archived: Option<bool>) -> Vec<T> {
    items
        .into_iter()
        .filter(|(s, _)| match archived {
            None => true,
            Some(true) => *s == ChatState::Archived,
            Some(false) => *s == ChatState::Active,
        })
        .map(|(_, v)| v)
        .collect()
}

async fn load_messages(db: &PgPool, chat_id: Uuid, q: &MessagesQuery) -> AppResult<Vec<MessageView>> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    Ok(sqlx::query_as::<_, MessageView>(
        "SELECT id, sender_type, body, created_at FROM messages
         WHERE chat_id = $1 AND ($2::timestamptz IS NULL OR created_at > $2)
         ORDER BY created_at ASC LIMIT $3",
    )
    .bind(chat_id)
    .bind(q.after)
    .bind(limit)
    .fetch_all(db)
    .await?)
}

fn validate_body(body: &str, max: usize) -> AppResult<String> {
    let b = body.trim();
    if b.is_empty() {
        return Err(AppError::Validation("сообщение не может быть пустым".into()));
    }
    if b.chars().count() > max {
        return Err(AppError::Validation(format!("сообщение не длиннее {max} символов")));
    }
    Ok(b.to_string())
}

async fn insert_message(
    db: &PgPool,
    chat: &ChatRow,
    sender_type: &str,
    sender_user_id: Uuid,
    body: &str,
) -> AppResult<MessageView> {
    if chat.state() != ChatState::Active {
        return Err(AppError::Forbidden("чат в архиве: доступен только для чтения".into()));
    }
    let msg = sqlx::query_as::<_, MessageView>(
        "INSERT INTO messages (chat_id, sender_type, sender_user_id, body) VALUES ($1, $2, $3, $4)
         RETURNING id, sender_type, body, created_at",
    )
    .bind(chat.id)
    .bind(sender_type)
    .bind(sender_user_id)
    .bind(body)
    .fetch_one(db)
    .await?;
    // Текст сообщения в лог не пишется (может содержать данные о здоровье).
    tracing::info!(chat_id = %chat.id, sender_type, "новое сообщение в чате");
    Ok(msg)
}

// --------------------------- Пользователь ----------------------------------

async fn user_chat(db: &PgPool, chat_id: Uuid, user_id: Uuid) -> AppResult<ChatRow> {
    sqlx::query_as::<_, ChatRow>(&format!("{CHAT_SELECT} WHERE c.id = $1 AND c.user_id = $2"))
        .bind(chat_id)
        .bind(user_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound("чат не найден".into()))
}

/// GET /api/v1/chats — чаты пользователя.
async fn list_user_chats(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ListChatsQuery>,
) -> AppResult<Json<Vec<ChatView>>> {
    let rows = sqlx::query_as::<_, ChatRow>(&format!(
        "{CHAT_SELECT} WHERE c.user_id = $1 ORDER BY c.created_at DESC"
    ))
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(filter_by_archived(
        rows.iter().map(|r| (r.state(), r.user_view())).collect(),
        q.archived,
    )))
}

async fn get_user_chat(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<ChatView>> {
    Ok(Json(user_chat(&state.db, id, user.user_id).await?.user_view()))
}

async fn list_user_messages(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<MessagesQuery>,
) -> AppResult<Json<Vec<MessageView>>> {
    user_chat(&state.db, id, user.user_id).await?;
    Ok(Json(load_messages(&state.db, id, &q).await?))
}

async fn send_user_message(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<SendMessage>,
) -> AppResult<(StatusCode, Json<MessageView>)> {
    let chat = user_chat(&state.db, id, user.user_id).await?;
    let body = validate_body(&req.body, state.config.chat_message_max_len)?;
    let msg = insert_message(&state.db, &chat, "user", user.user_id, &body).await?;
    state
        .notifier
        .notify(Notification::NewMessageForPharmacy {
            pharmacy_id: chat.pharmacy_id,
            chat_id: chat.id,
        })
        .await;
    Ok((StatusCode::CREATED, Json(msg)))
}

// ----------------------------- Аптека --------------------------------------

async fn pharmacy_chat(db: &PgPool, chat_id: Uuid, pharmacy_id: Uuid) -> AppResult<ChatRow> {
    sqlx::query_as::<_, ChatRow>(&format!("{CHAT_SELECT} WHERE c.id = $1 AND c.pharmacy_id = $2"))
        .bind(chat_id)
        .bind(pharmacy_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound("чат не найден".into()))
}

/// GET /api/v1/pharmacy-panel/chats — чаты аптеки сотрудника.
async fn list_pharmacy_chats(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ListChatsQuery>,
) -> AppResult<Json<Vec<PharmacyChatView>>> {
    let pharmacy_id = user.require_pharmacy_member()?;
    let rows = sqlx::query_as::<_, ChatRow>(&format!(
        "{CHAT_SELECT} WHERE c.pharmacy_id = $1 ORDER BY c.created_at DESC"
    ))
    .bind(pharmacy_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(filter_by_archived(
        rows.iter().map(|r| (r.state(), r.pharmacy_view())).collect(),
        q.archived,
    )))
}

async fn list_pharmacy_messages(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<MessagesQuery>,
) -> AppResult<Json<Vec<MessageView>>> {
    let pharmacy_id = user.require_pharmacy_member()?;
    pharmacy_chat(&state.db, id, pharmacy_id).await?;
    Ok(Json(load_messages(&state.db, id, &q).await?))
}

async fn send_pharmacy_message(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<SendMessage>,
) -> AppResult<(StatusCode, Json<MessageView>)> {
    let pharmacy_id = user.require_pharmacy_member()?;
    let chat = pharmacy_chat(&state.db, id, pharmacy_id).await?;
    let body = validate_body(&req.body, state.config.chat_message_max_len)?;
    let msg = insert_message(&state.db, &chat, "pharmacy", user.user_id, &body).await?;
    state
        .notifier
        .notify(Notification::NewMessageForUser {
            user_id: chat.user_id,
            chat_id: chat.id,
        })
        .await;
    Ok((StatusCode::CREATED, Json(msg)))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/chats", get(list_user_chats))
        .route("/chats/{id}", get(get_user_chat))
        .route("/chats/{id}/messages", get(list_user_messages).post(send_user_message))
        .route("/pharmacy-panel/chats", get(list_pharmacy_chats))
        .route(
            "/pharmacy-panel/chats/{id}/messages",
            get(list_pharmacy_messages).post(send_pharmacy_message),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn chat_active_for_48_hours_after_payment() {
        let paid = Utc::now();
        let until = paid + Duration::hours(48);
        assert_eq!(chat_state(until, paid), ChatState::Active);
        assert_eq!(chat_state(until, paid + Duration::hours(47)), ChatState::Active);
        assert_eq!(chat_state(until, paid + Duration::hours(48)), ChatState::Archived);
        assert_eq!(chat_state(until, paid + Duration::hours(72)), ChatState::Archived);
    }

    #[test]
    fn alias_hides_identity_and_is_stable() {
        let id = Uuid::new_v4();
        let a = client_alias(id);
        assert_eq!(a, client_alias(id));
        assert!(a.starts_with("Клиент #"));
        assert!(!a.contains('+'));
    }

    #[test]
    fn message_validation() {
        assert!(validate_body("   ", 10).is_err());
        assert!(validate_body("12345678901", 10).is_err());
        assert_eq!(validate_body("  привет ", 10).unwrap(), "привет");
    }

    #[test]
    fn archived_filter() {
        let items = vec![(ChatState::Active, 1), (ChatState::Archived, 2)];
        assert_eq!(filter_by_archived(items.clone(), None), vec![1, 2]);
        assert_eq!(filter_by_archived(items.clone(), Some(true)), vec![2]);
        assert_eq!(filter_by_archived(items, Some(false)), vec![1]);
    }
}
