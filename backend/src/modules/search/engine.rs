//! Поисковый движок.
//!
//! Запускается ТОЛЬКО после подтверждения оплаты (из обработчика webhook).
//! Алгоритм:
//! 1. `Paid → SearchInProgress` (повторный запуск невозможен — переход запрещён);
//! 2. волны по плану радиусов (100 → 4000 м, шаг 500 м; для расширенного поиска —
//!    финальная волна по всему Бишкеку); каждая волна опрашивает только ещё
//!    не опрошенные точки;
//! 3. перед запросом к аптеке — глобальный rate limit (PHARMACY_REQUEST_INTERVAL_SECS);
//! 4. ответы сопоставляются алгоритмом `medication::matching` (без ИИ);
//! 5. расширение останавливается, когда набран мягкий минимум результатов;
//! 6. ожидание ответов (SEARCH_RESPONSE_WAIT_SECS), затем `SearchCompleted`;
//! 7. при ошибке — `ProcessingError`, пользователь получает search_id и контакт поддержки.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use tokio::task::JoinSet;
use tracing::Instrument;
use uuid::Uuid;

use super::orders::{load_order, transition};
use super::radius::{should_expand, wave_plan, Wave};
use super::responses::apply_answer;
use crate::error::{AppError, AppResult};
use crate::modules::geo::{self, LocationCandidate};
use crate::modules::integrations::{AvailabilityQuery, IntegrationContext, QueryDispatch, QueryItem};
use crate::modules::medication::{load_order_medications, MedicationRow};
use crate::modules::notifications::Notification;
use crate::modules::payments::status::OrderStatus;
use crate::state::AppState;

/// Сообщение, если по итогам поиска не набран жёсткий минимум результатов.
pub const NOT_FOUND_MESSAGE: &str =
    "К сожалению, аптеки поблизости не подтвердили наличие. Попробуйте расширенный поиск или измените запрос.";

/// Запускает поиск в фоне. Все логи внутри содержат search_id (и request_id родительского запроса).
pub fn spawn_search(state: AppState, order_id: Uuid) -> tokio::task::JoinHandle<()> {
    let span = tracing::info_span!("search", search_id = %order_id);
    tokio::spawn(
        async move {
            if let Err(err) = run_search(&state, order_id).await {
                tracing::error!(error = %err, "ошибка выполнения поиска");
                mark_failed(&state, order_id, "внутренняя ошибка поиска").await;
            }
        }
        .instrument(span),
    )
}

async fn mark_failed(state: &AppState, order_id: Uuid, reason: &str) {
    let result = async {
        let mut tx = state.db.begin().await?;
        transition(&mut tx, order_id, OrderStatus::ProcessingError, Some(reason)).await?;
        sqlx::query("UPDATE search_orders SET error_message = $2 WHERE id = $1")
            .bind(order_id)
            .bind(reason)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok::<_, AppError>(())
    }
    .await;
    if let Err(e) = result {
        tracing::error!(error = %e, "не удалось перевести заказ в processing_error");
    }
}

/// Количество положительных ответов (полное или частичное совпадение).
async fn positive_count(state: &AppState, order_id: Uuid) -> AppResult<usize> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pharmacy_responses
         WHERE search_order_id = $1 AND status = 'answered' AND match_status IN ('full_match', 'partial_match')",
    )
    .bind(order_id)
    .fetch_one(&state.db)
    .await?;
    Ok(n as usize)
}

async fn pending_count(state: &AppState, order_id: Uuid) -> AppResult<usize> {
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pharmacy_responses WHERE search_order_id = $1 AND status = 'pending'")
            .bind(order_id)
            .fetch_one(&state.db)
            .await?;
    Ok(n as usize)
}

pub async fn run_search(state: &AppState, order_id: Uuid) -> AppResult<()> {
    let cfg = state.config.clone();
    {
        let mut tx = state.db.begin().await?;
        match transition(
            &mut tx,
            order_id,
            OrderStatus::SearchInProgress,
            Some("оплата подтверждена"),
        )
        .await
        {
            Ok(_) => tx.commit().await?,
            Err(AppError::Conflict(_)) => {
                // Поиск уже запущен/завершён — идемпотентно выходим.
                tracing::warn!("повторный запуск поиска проигнорирован");
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
    let order = load_order(&state.db, order_id).await?;
    let meds = load_order_medications(&state.db, order_id).await?;
    let origin = order.location();
    let plan = wave_plan(&cfg.search, order.search_type());
    tracing::info!(waves = plan.len(), medications = meds.len(), "поиск запущен");

    let mut queried: HashSet<Uuid> =
        sqlx::query_scalar::<_, Uuid>("SELECT location_id FROM pharmacy_responses WHERE search_order_id = $1")
            .bind(order_id)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .collect();

    for (idx, wave) in plan.iter().enumerate() {
        let wave_no = idx as i32 + 1;
        let radius = match wave {
            Wave::Radius(r) => Some(*r),
            Wave::WholeCity => None,
        };
        let candidates = geo::find_locations(&state.db, &cfg.search.city_name, origin, radius).await?;
        let fresh: Vec<LocationCandidate> = candidates
            .into_iter()
            .filter(|c| queried.insert(c.location_id))
            .collect();
        let radius_db = radius.map(|r| r as i32).unwrap_or(-1);
        sqlx::query("UPDATE search_orders SET current_radius_m = $2, updated_at = now() WHERE id = $1")
            .bind(order_id)
            .bind(radius.map(|r| r as i32))
            .execute(&state.db)
            .await?;
        tracing::info!(wave = wave_no, radius_m = ?radius, new_pharmacies = fresh.len(), "волна запросов");

        let mut set = JoinSet::new();
        for candidate in fresh {
            let state = state.clone();
            let meds = meds.clone();
            set.spawn(
                async move {
                    if let Err(e) = query_location(&state, order_id, wave_no, radius_db, &candidate, &meds).await {
                        tracing::warn!(location_id = %candidate.location_id, error = %e, "ошибка запроса к аптеке");
                    }
                }
                .in_current_span(),
            );
        }
        while set.join_next().await.is_some() {}

        let positive = positive_count(state, order_id).await?;
        let is_last = idx + 1 == plan.len();
        if !should_expand(positive, &cfg.search) || is_last {
            tracing::info!(positive, "расширение радиуса остановлено");
            break;
        }
        // Пауза перед расширением радиуса: даём аптекам время ответить.
        if !cfg.search.wave_interval.is_zero() {
            tokio::time::sleep(cfg.search.wave_interval).await;
            if !should_expand(positive_count(state, order_id).await?, &cfg.search) {
                break;
            }
        }
    }

    wait_for_responses(state, order_id, cfg.search.response_wait).await?;

    let positive = positive_count(state, order_id).await?;
    let mut tx = state.db.begin().await?;
    transition(
        &mut tx,
        order_id,
        OrderStatus::SearchCompleted,
        Some(&format!("найдено аптек: {positive}")),
    )
    .await?;
    if positive < cfg.search.hard_min_results {
        sqlx::query("UPDATE search_orders SET info_message = $2 WHERE id = $1")
            .bind(order_id)
            .bind(NOT_FOUND_MESSAGE)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    tracing::info!(positive, "поиск завершён");
    state
        .notifier
        .notify(Notification::SearchCompleted {
            user_id: order.user_id,
            search_id: order_id,
            found: positive,
        })
        .await;
    Ok(())
}

/// Ожидание ответов после последней волны.
async fn wait_for_responses(state: &AppState, order_id: Uuid, max_wait: Duration) -> AppResult<()> {
    if max_wait.is_zero() {
        return Ok(());
    }
    let deadline = Instant::now() + max_wait;
    let poll = Duration::from_secs(5).min(max_wait);
    while Instant::now() < deadline {
        if pending_count(state, order_id).await? == 0
            || positive_count(state, order_id).await? >= state.config.search.max_results
        {
            break;
        }
        tokio::time::sleep(poll).await;
    }
    Ok(())
}

/// Отправка запроса в одну торговую точку.
async fn query_location(
    state: &AppState,
    order_id: Uuid,
    wave: i32,
    radius_m: i32,
    c: &LocationCandidate,
    meds: &[MedicationRow],
) -> AppResult<()> {
    let Some(adapter) = state.adapters.resolve(c.integration_kind.as_deref()) else {
        tracing::warn!(kind = ?c.integration_kind, location_id = %c.location_id, "адаптер интеграции не найден");
        return Ok(());
    };
    // Глобальный лимит: не чаще PHARMACY_REQUEST_INTERVAL_SECS на аптеку (по всем поискам).
    let max_wait = state.config.pharmacy_request_interval + state.config.search.wave_interval;
    if !state.pharmacy_limiter.acquire(c.location_id, max_wait).await {
        // Точка уже отмечена как опрошенная в этом поиске и в следующих волнах не повторяется.
        tracing::info!(location_id = %c.location_id, "аптека пропущена: превышен лимит частоты запросов");
        return Ok(());
    }

    let response_id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO pharmacy_responses (search_order_id, pharmacy_id, location_id, wave, radius_m, distance_m)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (search_order_id, location_id) DO NOTHING RETURNING id",
    )
    .bind(order_id)
    .bind(c.pharmacy_id)
    .bind(c.location_id)
    .bind(wave)
    .bind(radius_m)
    .bind(c.distance_m)
    .fetch_optional(&state.db)
    .await?;
    let Some(response_id) = response_id else {
        return Ok(()); // уже опрошена в этом поиске
    };

    let ctx = IntegrationContext {
        pharmacy_id: c.pharmacy_id,
        location_id: c.location_id,
        config: c.integration_config.clone().unwrap_or_else(|| serde_json::json!({})),
    };
    let query = AvailabilityQuery {
        query_id: response_id,
        search_id: order_id,
        items: meds
            .iter()
            .map(|m| QueryItem {
                medication_id: m.id,
                spec: m.to_spec(),
                unit: m.unit.clone(),
            })
            .collect(),
    };
    tracing::debug!(location_id = %c.location_id, adapter = adapter.kind(), "запрос в аптеку");
    match adapter.send_query(&ctx, &query).await {
        Ok(QueryDispatch::Answered(answer)) => {
            apply_answer(state, response_id, &answer, None).await?;
        }
        Ok(QueryDispatch::Pending) => {
            state
                .notifier
                .notify(Notification::NewPharmacyRequest {
                    pharmacy_id: c.pharmacy_id,
                    response_id,
                })
                .await;
        }
        Err(e) => {
            tracing::warn!(location_id = %c.location_id, adapter = adapter.kind(), error = %e, "ошибка интеграции");
            sqlx::query("UPDATE pharmacy_responses SET status = 'failed' WHERE id = $1")
                .bind(response_id)
                .execute(&state.db)
                .await?;
        }
    }
    Ok(())
}
