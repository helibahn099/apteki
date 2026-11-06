//! Профиль пользователя и история его поисков.

use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::modules::auth::AuthUser;
use crate::modules::search::orders::{OrderView, SearchOrderRow, ORDER_COLUMNS};
use crate::state::AppState;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Profile {
    pub id: Uuid,
    pub phone: String,
    pub display_name: Option<String>,
    pub role: String,
    pub pharmacy_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProfile {
    pub display_name: Option<String>,
}

/// Проверка отображаемого имени: до 100 символов, без управляющих символов.
pub fn validate_display_name(raw: &str) -> Result<Option<String>, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > 100 {
        return Err("имя не длиннее 100 символов".into());
    }
    if name.chars().any(char::is_control) {
        return Err("имя содержит недопустимые символы".into());
    }
    Ok(Some(name.to_string()))
}

async fn load_profile(state: &AppState, user_id: Uuid) -> AppResult<Profile> {
    sqlx::query_as("SELECT id, phone, display_name, role, pharmacy_id, created_at FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("пользователь не найден".into()))
}

/// GET /users/me
async fn me(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Profile>> {
    Ok(Json(load_profile(&state, user.user_id).await?))
}

/// PATCH /users/me
async fn update_me(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<UpdateProfile>,
) -> AppResult<Json<Profile>> {
    let name = match req.display_name.as_deref() {
        Some(raw) => validate_display_name(raw).map_err(AppError::Validation)?,
        None => None,
    };
    sqlx::query("UPDATE users SET display_name = $2, updated_at = now() WHERE id = $1")
        .bind(user.user_id)
        .bind(name)
        .execute(&state.db)
        .await?;
    Ok(Json(load_profile(&state, user.user_id).await?))
}

#[derive(Debug, Deserialize)]
pub struct OrdersQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// GET /users/me/orders — история поисков (только свои заказы).
async fn my_orders(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<OrdersQuery>,
) -> AppResult<Json<Vec<OrderView>>> {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let offset = q.offset.unwrap_or(0).max(0);
    let sql = format!(
        "SELECT {ORDER_COLUMNS} FROM search_orders WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
    );
    let rows: Vec<SearchOrderRow> = sqlx::query_as(&sql)
        .bind(user.user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?;
    let url = state.config.support_url();
    Ok(Json(
        rows.iter()
            .map(|r| OrderView::from_row(r, &state.config.support_telegram, &url))
            .collect(),
    ))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/users/me", get(me).patch(update_me))
        .route("/users/me/orders", get(my_orders))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_validation() {
        assert_eq!(validate_display_name("  Айбек ").unwrap(), Some("Айбек".into()));
        assert_eq!(validate_display_name("   ").unwrap(), None);
        assert!(validate_display_name(&"а".repeat(101)).is_err());
        assert!(validate_display_name("a\u{0007}b").is_err());
    }
}
