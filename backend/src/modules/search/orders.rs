//! Поисковые заказы: модель, загрузка с проверкой владельца, смена статусов.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::modules::geo::GeoPoint;
use crate::modules::payments::pricing::SearchType;
use crate::modules::payments::status::OrderStatus;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SearchOrderRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub search_type: String,
    pub status: String,
    pub lat: f64,
    pub lon: f64,
    pub current_radius_m: Option<i32>,
    pub precheck_pharmacy_count: i32,
    pub price_minor: i64,
    pub currency: String,
    pub info_message: Option<String>,
    pub error_message: Option<String>,
    pub source_order_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub paid_at: Option<DateTime<Utc>>,
    pub search_started_at: Option<DateTime<Utc>>,
    pub search_completed_at: Option<DateTime<Utc>>,
}

pub const ORDER_COLUMNS: &str = "id, user_id, search_type, status,
    ST_Y(location::geometry) AS lat, ST_X(location::geometry) AS lon,
    current_radius_m, precheck_pharmacy_count, price_minor, currency, info_message,
    error_message, source_order_id, created_at, updated_at, paid_at, search_started_at,
    search_completed_at";

impl SearchOrderRow {
    pub fn status(&self) -> OrderStatus {
        OrderStatus::from_db(&self.status).unwrap_or(OrderStatus::ProcessingError)
    }

    pub fn search_type(&self) -> SearchType {
        SearchType::from_db(&self.search_type).unwrap_or(SearchType::Basic)
    }

    pub fn location(&self) -> GeoPoint {
        GeoPoint {
            lat: self.lat,
            lon: self.lon,
        }
    }
}

pub async fn load_order(db: &PgPool, order_id: Uuid) -> AppResult<SearchOrderRow> {
    sqlx::query_as::<_, SearchOrderRow>(&format!("SELECT {ORDER_COLUMNS} FROM search_orders WHERE id = $1"))
        .bind(order_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound("заказ не найден".into()))
}

/// Загружает заказ, только если он принадлежит пользователю.
/// Чужой заказ неотличим от несуществующего (404), чтобы не раскрывать ID.
pub async fn load_user_order(db: &PgPool, order_id: Uuid, user_id: Uuid) -> AppResult<SearchOrderRow> {
    sqlx::query_as::<_, SearchOrderRow>(&format!(
        "SELECT {ORDER_COLUMNS} FROM search_orders WHERE id = $1 AND user_id = $2"
    ))
    .bind(order_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("заказ не найден".into()))
}

/// Смена статуса с проверкой допустимости перехода и записью в `order_history`.
/// Выполняется в переданной транзакции/соединении; строка заказа блокируется.
pub async fn transition(
    conn: &mut PgConnection,
    order_id: Uuid,
    to: OrderStatus,
    note: Option<&str>,
) -> AppResult<OrderStatus> {
    let current: String = sqlx::query_scalar("SELECT status FROM search_orders WHERE id = $1 FOR UPDATE")
        .bind(order_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::NotFound("заказ не найден".into()))?;
    let from = OrderStatus::from_db(&current).ok_or_else(|| anyhow::anyhow!("неизвестный статус в БД"))?;
    if !from.can_transition_to(to) {
        return Err(AppError::Conflict(format!(
            "недопустимая смена статуса: {} → {}",
            from.as_db(),
            to.as_db()
        )));
    }
    let extra = match to {
        OrderStatus::Paid => ", paid_at = now()",
        OrderStatus::SearchInProgress => ", search_started_at = now()",
        OrderStatus::SearchCompleted => ", search_completed_at = now()",
        _ => "",
    };
    sqlx::query(&format!(
        "UPDATE search_orders SET status = $2, updated_at = now(){extra} WHERE id = $1"
    ))
    .bind(order_id)
    .bind(to.as_db())
    .execute(&mut *conn)
    .await?;
    sqlx::query("INSERT INTO order_history (search_order_id, from_status, to_status, note) VALUES ($1, $2, $3, $4)")
        .bind(order_id)
        .bind(from.as_db())
        .bind(to.as_db())
        .bind(note)
        .execute(&mut *conn)
        .await?;
    tracing::info!(search_id = %order_id, from = from.as_db(), to = to.as_db(), "смена статуса заказа");
    Ok(from)
}

/// Публичное представление заказа.
#[derive(Debug, Serialize)]
pub struct OrderView {
    pub id: Uuid,
    pub search_type: SearchType,
    pub status: OrderStatus,
    pub location: GeoPoint,
    pub current_radius_m: Option<i32>,
    pub precheck_pharmacy_count: i32,
    pub price_minor: i64,
    pub price_kgs: String,
    pub currency: String,
    pub info_message: Option<String>,
    pub source_order_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub paid_at: Option<DateTime<Utc>>,
    pub search_started_at: Option<DateTime<Utc>>,
    pub search_completed_at: Option<DateTime<Utc>>,
    /// Заполняется при статусе processing_error.
    pub support: Option<SupportInfo>,
}

#[derive(Debug, Serialize, Clone)]
pub struct SupportInfo {
    pub search_id: Uuid,
    pub telegram: String,
    pub url: String,
    pub message: String,
}

impl OrderView {
    pub fn from_row(r: &SearchOrderRow, support_telegram: &str, support_url: &str) -> Self {
        let status = r.status();
        OrderView {
            id: r.id,
            search_type: r.search_type(),
            status,
            location: r.location(),
            current_radius_m: r.current_radius_m,
            precheck_pharmacy_count: r.precheck_pharmacy_count,
            price_minor: r.price_minor,
            price_kgs: crate::modules::payments::pricing::format_kgs(r.price_minor),
            currency: r.currency.clone(),
            info_message: r.info_message.clone(),
            source_order_id: r.source_order_id,
            created_at: r.created_at,
            paid_at: r.paid_at,
            search_started_at: r.search_started_at,
            search_completed_at: r.search_completed_at,
            support: (status == OrderStatus::ProcessingError).then(|| SupportInfo {
                search_id: r.id,
                telegram: support_telegram.to_string(),
                url: support_url.to_string(),
                message: "Произошла ошибка при поиске. Напишите в поддержку и укажите номер поиска.".into(),
            }),
        }
    }
}
