//! Модуль Search: создание поисковых заказов, предварительная проверка,
//! поисковый движок (волны, радиус, rate limiting) и выдача результатов.

pub mod engine;
pub mod maintenance;
pub mod orders;
pub mod radius;
pub mod rate_limiter;
pub mod responses;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::modules::auth::AuthUser;
use crate::modules::geo::{self, GeoPoint};
use crate::modules::medication::matching::MatchStatus;
use crate::modules::medication::{self, MedicationInput, MedicationRow};
use crate::modules::payments::pricing::{format_kgs, search_price_minor, SearchType};
use crate::state::AppState;
use orders::{load_user_order, OrderView, SupportInfo};
use responses::ResponseItemView;

/// Радиус предварительной проверки: максимум базового поиска или весь город.
fn precheck_radius(state: &AppState, t: SearchType) -> Option<u32> {
    match t {
        SearchType::Basic => Some(state.config.search.max_radius_basic_m),
        SearchType::Extended => None,
    }
}

async fn ensure_in_city(state: &AppState, p: GeoPoint) -> AppResult<()> {
    if !p.is_valid() {
        return Err(AppError::Validation("некорректные координаты".into()));
    }
    if !geo::is_inside_city(&state.db, &state.config.search.city_name, p).await? {
        return Err(AppError::Validation(format!(
            "сервис пока работает только в городе {}",
            state.config.search.city_name
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Предварительная проверка
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PrecheckRequest {
    pub search_type: SearchType,
    pub location: GeoPoint,
    #[serde(default)]
    pub medications_count: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct PrecheckResponse {
    /// Количество аптек (торговых точек) в зоне поиска.
    pub pharmacy_count: i64,
    /// Радиус проверки, м (`null` — весь город).
    pub radius_m: Option<u32>,
    pub price_minor: Option<i64>,
    pub price_kgs: Option<String>,
    pub currency: &'static str,
}

/// POST /api/v1/search/precheck — только подсчёт аптек в радиусе (до оплаты).
async fn precheck(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<PrecheckRequest>,
) -> AppResult<Json<PrecheckResponse>> {
    ensure_in_city(&state, req.location).await?;
    let radius = precheck_radius(&state, req.search_type);
    let count = geo::count_locations(&state.db, &state.config.search.city_name, req.location, radius).await?;
    let price = req
        .medications_count
        .filter(|n| *n > 0)
        .map(|n| search_price_minor(&state.config.pricing, req.search_type, n));
    Ok(Json(PrecheckResponse {
        pharmacy_count: count,
        radius_m: radius,
        price_minor: price,
        price_kgs: price.map(format_kgs),
        currency: "KGS",
    }))
}

// ---------------------------------------------------------------------------
// Заказы
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateOrderRequest {
    pub search_type: SearchType,
    pub location: GeoPoint,
    pub medications: Vec<MedicationInput>,
}

#[derive(Debug, Serialize)]
pub struct OrderDetails {
    #[serde(flatten)]
    pub order: OrderView,
    pub medications: Vec<MedicationRow>,
}

async fn order_details(state: &AppState, order: &orders::SearchOrderRow) -> AppResult<OrderDetails> {
    Ok(OrderDetails {
        order: OrderView::from_row(order, &state.config.support_telegram, &state.config.support_url()),
        medications: medication::load_order_medications(&state.db, order.id).await?,
    })
}

/// Общая логика создания заказа (используется и при повторе поиска).
async fn create_order_inner(
    state: &AppState,
    user_id: Uuid,
    search_type: SearchType,
    location: GeoPoint,
    items: &[MedicationInput],
    source_order_id: Option<Uuid>,
) -> AppResult<orders::SearchOrderRow> {
    ensure_in_city(state, location).await?;
    let items = medication::validate_items(items, state.config.search.max_medications)?;
    let blacklist = medication::load_blacklist(&state.db).await?;
    for item in &items {
        if let Some(reason) = medication::is_blacklisted(item, &blacklist) {
            return Err(AppError::Validation(format!("«{}»: {reason}", item.name)));
        }
    }
    let count = geo::count_locations(
        &state.db,
        &state.config.search.city_name,
        location,
        precheck_radius(state, search_type),
    )
    .await?;
    let price = search_price_minor(&state.config.pricing, search_type, items.len());

    let mut tx = state.db.begin().await?;
    let order_id: Uuid = sqlx::query_scalar(
        "INSERT INTO search_orders (user_id, search_type, location, precheck_pharmacy_count, price_minor, source_order_id)
         VALUES ($1, $2, ST_SetSRID(ST_MakePoint($3, $4), 4326)::geography, $5, $6, $7) RETURNING id",
    )
    .bind(user_id)
    .bind(search_type.as_db())
    .bind(location.lon)
    .bind(location.lat)
    .bind(count as i32)
    .bind(price)
    .bind(source_order_id)
    .fetch_one(&mut *tx)
    .await?;
    for (pos, m) in items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO medications (search_order_id, catalog_id, position, name, active_substance, dosage,
                                      concentration, form, manufacturer, quantity, unit)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(order_id)
        .bind(m.catalog_id)
        .bind(pos as i32)
        .bind(&m.name)
        .bind(&m.active_substance)
        .bind(&m.dosage)
        .bind(&m.concentration)
        .bind(&m.form)
        .bind(&m.manufacturer)
        .bind(m.quantity as i32)
        .bind(m.unit.as_deref().unwrap_or(medication::matching::DEFAULT_UNIT))
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "INSERT INTO order_history (search_order_id, from_status, to_status, note) VALUES ($1, NULL, 'created', $2)",
    )
    .bind(order_id)
    .bind(source_order_id.map(|s| format!("повтор поиска {s}")))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(search_id = %order_id, user_id = %user_id, items = items.len(), pharmacies = count, "создан поисковый заказ");
    orders::load_order(&state.db, order_id).await
}

/// POST /api/v1/search/orders — создание поискового заказа.
async fn create_order(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateOrderRequest>,
) -> AppResult<(StatusCode, Json<OrderDetails>)> {
    let order = create_order_inner(
        &state,
        user.user_id,
        req.search_type,
        req.location,
        &req.medications,
        None,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(order_details(&state, &order).await?)))
}

/// GET /api/v1/search/orders/{id} — заказ пользователя.
async fn get_order(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<OrderDetails>> {
    let order = load_user_order(&state.db, id, user.user_id).await?;
    Ok(Json(order_details(&state, &order).await?))
}

#[derive(Debug, Deserialize, Default)]
pub struct RepeatRequest {
    pub search_type: Option<SearchType>,
    pub location: Option<GeoPoint>,
}

/// POST /api/v1/search/orders/{id}/repeat — новый поиск на основе старого.
async fn repeat_order(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<RepeatRequest>>,
) -> AppResult<(StatusCode, Json<OrderDetails>)> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let source = load_user_order(&state.db, id, user.user_id).await?;
    let items: Vec<MedicationInput> = medication::load_order_medications(&state.db, id)
        .await?
        .into_iter()
        .map(|m| MedicationInput {
            catalog_id: m.catalog_id,
            name: m.name,
            active_substance: m.active_substance,
            dosage: m.dosage,
            concentration: m.concentration,
            form: m.form,
            manufacturer: m.manufacturer,
            quantity: m.quantity.max(1) as u32,
            unit: Some(m.unit),
        })
        .collect();
    let order = create_order_inner(
        &state,
        user.user_id,
        req.search_type.unwrap_or(source.search_type()),
        req.location.unwrap_or(source.location()),
        &items,
        Some(source.id),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(order_details(&state, &order).await?)))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct HistoryEntry {
    pub from_status: Option<String>,
    pub to_status: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// GET /api/v1/search/orders/{id}/history — история статусов заказа.
async fn order_history(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<HistoryEntry>>> {
    load_user_order(&state.db, id, user.user_id).await?;
    let rows = sqlx::query_as::<_, HistoryEntry>(
        "SELECT from_status, to_status, note, created_at FROM order_history WHERE search_order_id = $1 ORDER BY created_at",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// Результаты
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PharmacyResult {
    pub response_id: Uuid,
    pub pharmacy_id: Uuid,
    pub location_id: Uuid,
    pub pharmacy_name: String,
    pub address: String,
    pub phone: Option<String>,
    pub working_hours: Option<String>,
    pub location: GeoPoint,
    pub distance_m: f64,
    pub match_status: MatchStatus,
    pub items: Vec<ResponseItemView>,
    /// Общая стоимость доступного набора (тыйын).
    pub total_price_minor: i64,
    pub total_price_kgs: String,
    /// Время последнего подтверждения наличия.
    pub confirmed_at: Option<DateTime<Utc>>,
    pub chat_id: Option<Uuid>,
}

/// Сортировка: полные совпадения выше частичных, затем ближе, затем дешевле.
/// Отсекается всё сверх `max` и все `NoMatch`.
pub fn rank_results(mut results: Vec<PharmacyResult>, max: usize) -> Vec<PharmacyResult> {
    results.retain(|r| r.match_status != MatchStatus::NoMatch);
    results.sort_by(|a, b| {
        a.match_status
            .cmp(&b.match_status)
            .then(a.distance_m.total_cmp(&b.distance_m))
            .then(a.total_price_minor.cmp(&b.total_price_minor))
    });
    results.truncate(max);
    results
}

#[derive(sqlx::FromRow)]
struct ResultRow {
    response_id: Uuid,
    pharmacy_id: Uuid,
    location_id: Uuid,
    pharmacy_name: String,
    address: String,
    phone: Option<String>,
    working_hours: Option<String>,
    lat: f64,
    lon: f64,
    distance_m: f64,
    match_status: String,
    items: serde_json::Value,
    total_price_minor: Option<i64>,
    answered_at: Option<DateTime<Utc>>,
    chat_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct ResultsResponse {
    pub search_id: Uuid,
    pub status: crate::modules::payments::status::OrderStatus,
    pub results: Vec<PharmacyResult>,
    pub full_match_count: usize,
    pub partial_match_count: usize,
    pub queried_pharmacies: i64,
    pub pending_responses: i64,
    /// Набран ли жёсткий минимум (иначе — «ничего не найдено»).
    pub hard_min_reached: bool,
    /// Набран ли мягкий минимум.
    pub soft_min_reached: bool,
    pub info_message: Option<String>,
    pub support: Option<SupportInfo>,
}

/// GET /api/v1/search/orders/{id}/results — результаты поиска.
async fn get_results(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<ResultsResponse>> {
    let order = load_user_order(&state.db, id, user.user_id).await?;
    let status = order.status();
    if !status.is_paid_or_later() && status != crate::modules::payments::status::OrderStatus::ProcessingError {
        return Err(AppError::Conflict("поиск запускается только после оплаты".into()));
    }
    let rows = sqlx::query_as::<_, ResultRow>(
        "SELECT r.id AS response_id, r.pharmacy_id, r.location_id, p.name AS pharmacy_name, l.address,
                COALESCE(l.phone, p.phone) AS phone, l.working_hours,
                ST_Y(l.location::geometry) AS lat, ST_X(l.location::geometry) AS lon,
                r.distance_m, r.match_status, r.items, r.total_price_minor, r.answered_at, c.id AS chat_id
         FROM pharmacy_responses r
         JOIN pharmacies p ON p.id = r.pharmacy_id
         JOIN pharmacy_locations l ON l.id = r.location_id
         LEFT JOIN chats c ON c.search_order_id = r.search_order_id AND c.location_id = r.location_id
         WHERE r.search_order_id = $1 AND r.status = 'answered'
           AND r.match_status IN ('full_match', 'partial_match')",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let results: Vec<PharmacyResult> = rows
        .into_iter()
        .map(|r| {
            let total = r.total_price_minor.unwrap_or(0);
            PharmacyResult {
                response_id: r.response_id,
                pharmacy_id: r.pharmacy_id,
                location_id: r.location_id,
                pharmacy_name: r.pharmacy_name,
                address: r.address,
                phone: r.phone,
                working_hours: r.working_hours,
                location: GeoPoint { lat: r.lat, lon: r.lon },
                distance_m: r.distance_m.round(),
                match_status: MatchStatus::from_db(&r.match_status).unwrap_or(MatchStatus::NoMatch),
                items: serde_json::from_value(r.items).unwrap_or_default(),
                total_price_minor: total,
                total_price_kgs: format_kgs(total),
                confirmed_at: r.answered_at,
                chat_id: r.chat_id,
            }
        })
        .collect();
    let ranked = rank_results(results, state.config.search.max_results);
    let (queried, pending): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE status = 'pending') FROM pharmacy_responses WHERE search_order_id = $1",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    let full = ranked
        .iter()
        .filter(|r| r.match_status == MatchStatus::FullMatch)
        .count();
    let view = OrderView::from_row(&order, &state.config.support_telegram, &state.config.support_url());
    Ok(Json(ResultsResponse {
        search_id: id,
        status,
        full_match_count: full,
        partial_match_count: ranked.len() - full,
        hard_min_reached: ranked.len() >= state.config.search.hard_min_results,
        soft_min_reached: ranked.len() >= state.config.search.soft_min_results,
        results: ranked,
        queried_pharmacies: queried,
        pending_responses: pending,
        info_message: order.info_message.clone(),
        support: view.support,
    }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/search/precheck", post(precheck))
        .route("/search/orders", post(create_order))
        .route("/search/orders/{id}", get(get_order))
        .route("/search/orders/{id}/repeat", post(repeat_order))
        .route("/search/orders/{id}/history", get(order_history))
        .route("/search/orders/{id}/results", get(get_results))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(status: MatchStatus, distance: f64, price: i64) -> PharmacyResult {
        PharmacyResult {
            response_id: Uuid::new_v4(),
            pharmacy_id: Uuid::new_v4(),
            location_id: Uuid::new_v4(),
            pharmacy_name: "А".into(),
            address: "Б".into(),
            phone: None,
            working_hours: None,
            location: GeoPoint { lat: 0.0, lon: 0.0 },
            distance_m: distance,
            match_status: status,
            items: vec![],
            total_price_minor: price,
            total_price_kgs: format_kgs(price),
            confirmed_at: None,
            chat_id: None,
        }
    }

    #[test]
    fn full_matches_first_then_distance() {
        let r = rank_results(
            vec![
                result(MatchStatus::PartialMatch, 100.0, 10),
                result(MatchStatus::FullMatch, 900.0, 10),
                result(MatchStatus::FullMatch, 300.0, 10),
                result(MatchStatus::NoMatch, 50.0, 10),
            ],
            15,
        );
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].match_status, MatchStatus::FullMatch);
        assert_eq!(r[0].distance_m, 300.0);
        assert_eq!(r[1].distance_m, 900.0);
        assert_eq!(r[2].match_status, MatchStatus::PartialMatch);
    }

    #[test]
    fn truncates_to_max_results() {
        let many: Vec<_> = (0..30).map(|i| result(MatchStatus::FullMatch, i as f64, 1)).collect();
        assert_eq!(rank_results(many, 15).len(), 15);
    }

    #[test]
    fn equal_distance_sorted_by_price() {
        let r = rank_results(
            vec![
                result(MatchStatus::FullMatch, 100.0, 900),
                result(MatchStatus::FullMatch, 100.0, 300),
            ],
            15,
        );
        assert_eq!(r[0].total_price_minor, 300);
    }
}
