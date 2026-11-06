//! Модуль Payments: выставление счёта (QR), приём webhook-уведомлений,
//! идемпотентная обработка оплаты и запуск поиска.

pub mod pricing;
pub mod provider;
pub mod status;

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::modules::auth::AuthUser;
use crate::modules::notifications::Notification;
use crate::modules::search::engine::spawn_search;
use crate::modules::search::orders::{load_user_order, transition};
use crate::state::AppState;
use provider::{CreatePaymentRequest, MockPaymentProvider, PaymentProvider, WebhookStatus};
use status::OrderStatus;

/// Выбор платёжного провайдера по конфигурации.
pub fn build_payment_provider(config: &Config) -> anyhow::Result<Arc<dyn PaymentProvider>> {
    if config.mock_payment_enabled {
        return Ok(Arc::new(MockPaymentProvider::new(
            config.mock_payment_webhook_secret.clone(),
        )));
    }
    // TODO(Payments): подключить реального провайдера (банк) после выбора — реализовать
    // трейт PaymentProvider и вернуть его здесь. Бизнес-логика не меняется.
    anyhow::bail!("MOCK_PAYMENT_ENABLED=false, но реальный платёжный провайдер ещё не подключён")
}

/// SVG с QR-кодом для отображения в приложении.
pub fn qr_svg(payload: &str) -> AppResult<String> {
    let code = qrcode::QrCode::new(payload.as_bytes()).map_err(|e| anyhow::anyhow!("ошибка генерации QR: {e}"))?;
    Ok(code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(256, 256)
        .build())
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PaymentRow {
    pub id: Uuid,
    pub search_order_id: Uuid,
    pub provider: String,
    pub provider_payment_id: String,
    pub amount_minor: i64,
    pub currency: String,
    pub status: String,
    pub qr_payload: String,
    pub expires_at: DateTime<Utc>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

const PAYMENT_COLUMNS: &str = "id, search_order_id, provider, provider_payment_id, amount_minor, currency, status,
    qr_payload, expires_at, paid_at, created_at";

#[derive(Debug, Serialize)]
pub struct PaymentView {
    pub payment_id: Uuid,
    pub search_id: Uuid,
    pub provider: String,
    pub status: String,
    pub amount_minor: i64,
    pub amount_kgs: String,
    pub currency: String,
    pub qr_payload: String,
    pub qr_svg: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub paid_at: Option<DateTime<Utc>>,
    pub order_status: OrderStatus,
}

fn to_view(p: PaymentRow, order_status: OrderStatus, with_svg: bool) -> AppResult<PaymentView> {
    let qr_svg = if with_svg && p.status == "pending" {
        Some(qr_svg(&p.qr_payload)?)
    } else {
        None
    };
    Ok(PaymentView {
        payment_id: p.id,
        search_id: p.search_order_id,
        provider: p.provider,
        status: p.status,
        amount_kgs: pricing::format_kgs(p.amount_minor),
        amount_minor: p.amount_minor,
        currency: p.currency,
        qr_payload: p.qr_payload,
        qr_svg,
        expires_at: p.expires_at,
        paid_at: p.paid_at,
        order_status,
    })
}

/// POST /api/v1/search/orders/{id}/payment — выставить счёт (QR).
/// Идемпотентно: пока действующий QR не истёк, возвращается он же.
async fn create_payment(
    State(state): State<AppState>,
    user: AuthUser,
    Path(order_id): Path<Uuid>,
) -> AppResult<(StatusCode, Json<PaymentView>)> {
    let order = load_user_order(&state.db, order_id, user.user_id).await?;
    let status = order.status();
    if status.is_paid_or_later() {
        return Err(AppError::Conflict("заказ уже оплачен".into()));
    }
    if status == OrderStatus::ProcessingError {
        return Err(AppError::Conflict(
            "заказ в статусе ошибки — обратитесь в поддержку".into(),
        ));
    }
    if order.precheck_pharmacy_count == 0 {
        return Err(AppError::Conflict(
            "в зоне поиска нет подключённых аптек — оплата недоступна".into(),
        ));
    }

    let mut tx = state.db.begin().await?;
    // Блокируем заказ, чтобы параллельные запросы не создали два счёта.
    sqlx::query("SELECT id FROM search_orders WHERE id = $1 FOR UPDATE")
        .bind(order_id)
        .execute(&mut *tx)
        .await?;
    if let Some(existing) = sqlx::query_as::<_, PaymentRow>(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM payments
         WHERE search_order_id = $1 AND status = 'pending' AND expires_at > now()
         ORDER BY created_at DESC LIMIT 1"
    ))
    .bind(order_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        tx.commit().await?;
        return Ok((StatusCode::OK, Json(to_view(existing, status, true)?)));
    }

    let created = state
        .payments
        .create_payment(CreatePaymentRequest {
            order_id,
            amount_minor: order.price_minor,
            currency: order.currency.clone(),
            ttl: state.config.payment_qr_ttl,
        })
        .await
        .map_err(|e| AppError::ServiceUnavailable(format!("платёжный провайдер недоступен: {e}")))?;
    let row = sqlx::query_as::<_, PaymentRow>(&format!(
        "INSERT INTO payments (search_order_id, provider, provider_payment_id, amount_minor, currency, qr_payload, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {PAYMENT_COLUMNS}"
    ))
    .bind(order_id)
    .bind(state.payments.name())
    .bind(&created.provider_payment_id)
    .bind(order.price_minor)
    .bind(&order.currency)
    .bind(&created.qr_payload)
    .bind(created.expires_at)
    .fetch_one(&mut *tx)
    .await?;
    transition(
        &mut tx,
        order_id,
        OrderStatus::AwaitingPayment,
        Some("выставлен QR-код"),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(search_id = %order_id, payment_id = %row.id, provider = state.payments.name(), "выставлен счёт");
    Ok((
        StatusCode::CREATED,
        Json(to_view(row, OrderStatus::AwaitingPayment, true)?),
    ))
}

/// GET /api/v1/search/orders/{id}/payment — статус последнего платежа.
async fn get_payment(
    State(state): State<AppState>,
    user: AuthUser,
    Path(order_id): Path<Uuid>,
) -> AppResult<Json<PaymentView>> {
    let order = load_user_order(&state.db, order_id, user.user_id).await?;
    let row = sqlx::query_as::<_, PaymentRow>(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM payments WHERE search_order_id = $1
         ORDER BY (status = 'paid') DESC, created_at DESC LIMIT 1"
    ))
    .bind(order_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("платёж не найден".into()))?;
    Ok(Json(to_view(row, order.status(), false)?))
}

/// Итог обработки уведомления.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookOutcome {
    /// Оплата принята, поиск запущен.
    Processed,
    /// Повторное уведомление — ничего не сделано (идемпотентность).
    AlreadyProcessed,
    /// Уведомление о неуспешной оплате.
    Failed,
}

/// Обработка webhook: проверка подписи, идемпотентная смена статусов, запуск поиска.
pub async fn process_webhook(state: &AppState, headers: &HeaderMap, body: &[u8]) -> AppResult<WebhookOutcome> {
    let event = state.payments.verify_webhook(headers, body).map_err(|e| {
        tracing::warn!(error = %e, "отклонено платёжное уведомление");
        AppError::Unauthorized
    })?;

    let mut tx = state.db.begin().await?;
    let payment: Option<(Uuid, Uuid, i64, String)> = sqlx::query_as(
        "SELECT id, search_order_id, amount_minor, status FROM payments WHERE provider_payment_id = $1 FOR UPDATE",
    )
    .bind(&event.provider_payment_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (payment_id, order_id, amount, pay_status) =
        payment.ok_or_else(|| AppError::NotFound("платёж не найден".into()))?;

    if pay_status == "paid" {
        tx.commit().await?;
        tracing::info!(search_id = %order_id, payment_id = %payment_id, "повторное уведомление об оплате проигнорировано");
        return Ok(WebhookOutcome::AlreadyProcessed);
    }

    if event.status == WebhookStatus::Failed {
        sqlx::query("UPDATE payments SET status = 'failed', updated_at = now() WHERE id = $1 AND status <> 'paid'")
            .bind(payment_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        tracing::warn!(search_id = %order_id, payment_id = %payment_id, "провайдер сообщил о неуспешной оплате");
        return Ok(WebhookOutcome::Failed);
    }

    if event.amount_minor != amount || event.currency != "KGS" {
        tx.rollback().await?;
        tracing::error!(search_id = %order_id, payment_id = %payment_id, "сумма/валюта в уведомлении не совпадает со счётом");
        return Err(AppError::Validation("сумма платежа не совпадает со счётом".into()));
    }

    // Уникальный индекс uq_payments_one_paid_per_order дополнительно защищает от двойной оплаты.
    let updated = sqlx::query(
        "UPDATE payments SET status = 'paid', paid_at = now(), updated_at = now()
         WHERE id = $1 AND NOT EXISTS (
             SELECT 1 FROM payments WHERE search_order_id = $2 AND status = 'paid')",
    )
    .bind(payment_id)
    .bind(order_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        tx.commit().await?;
        tracing::warn!(search_id = %order_id, "заказ уже оплачен другим платежом — повторная оплата не засчитана");
        return Ok(WebhookOutcome::AlreadyProcessed);
    }
    match transition(
        &mut tx,
        order_id,
        OrderStatus::Paid,
        Some("оплата подтверждена провайдером"),
    )
    .await
    {
        Ok(_) => {}
        Err(AppError::Conflict(_)) => {
            tx.rollback().await?;
            return Ok(WebhookOutcome::AlreadyProcessed);
        }
        Err(e) => return Err(e),
    }
    let user_id: Uuid = sqlx::query_scalar("SELECT user_id FROM search_orders WHERE id = $1")
        .bind(order_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(search_id = %order_id, payment_id = %payment_id, "оплата подтверждена");

    state
        .notifier
        .notify(Notification::PaymentConfirmed {
            user_id,
            search_id: order_id,
        })
        .await;
    // Поиск запускается только здесь — после подтверждения оплаты.
    spawn_search(state.clone(), order_id);
    Ok(WebhookOutcome::Processed)
}

#[derive(Debug, Serialize)]
pub struct WebhookAck {
    pub outcome: WebhookOutcome,
}

/// POST /api/v1/payments/webhook/{provider} — уведомление от платёжного провайдера.
async fn webhook(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<WebhookAck>> {
    if provider != state.payments.name() {
        return Err(AppError::NotFound("неизвестный платёжный провайдер".into()));
    }
    let outcome = process_webhook(&state, &headers, &body).await?;
    Ok(Json(WebhookAck { outcome }))
}

/// POST /api/v1/payments/mock/{payment_id}/confirm — имитация оплаты (только Mock-провайдер).
async fn mock_confirm(
    State(state): State<AppState>,
    user: AuthUser,
    Path(payment_id): Path<Uuid>,
) -> AppResult<Json<WebhookAck>> {
    if !state.config.mock_payment_enabled {
        return Err(AppError::NotFound("тестовая оплата отключена".into()));
    }
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT p.provider_payment_id, p.amount_minor FROM payments p
         JOIN search_orders o ON o.id = p.search_order_id
         WHERE p.id = $1 AND o.user_id = $2",
    )
    .bind(payment_id)
    .bind(user.user_id)
    .fetch_optional(&state.db)
    .await?;
    let (provider_payment_id, amount) = row.ok_or_else(|| AppError::NotFound("платёж не найден".into()))?;
    let (headers, body) = state
        .payments
        .simulate_paid_webhook(&provider_payment_id, amount)
        .ok_or_else(|| AppError::NotFound("провайдер не поддерживает имитацию оплаты".into()))?;
    let outcome = process_webhook(&state, &headers, &body).await?;
    Ok(Json(WebhookAck { outcome }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/search/orders/{id}/payment", post(create_payment).get(get_payment))
        .route("/payments/webhook/{provider}", post(webhook))
        .route("/payments/mock/{payment_id}/confirm", post(mock_confirm))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_svg_is_generated() {
        let svg = qr_svg("mockpay://pay?pid=mock_1&amount=5000").unwrap();
        assert!(svg.starts_with("<?xml") || svg.contains("<svg"));
    }

    #[test]
    fn real_provider_required_when_mock_disabled() {
        let mut cfg = Config::for_tests("postgres://x");
        cfg.mock_payment_enabled = false;
        assert!(build_payment_provider(&cfg).is_err());
        cfg.mock_payment_enabled = true;
        assert_eq!(build_payment_provider(&cfg).unwrap().name(), "mock");
    }
}
