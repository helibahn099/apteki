//! Абстракция платёжного провайдера и тестовый Mock Payment Provider.
//!
//! Бизнес-логика работает только с трейтом `PaymentProvider`. Чтобы подключить
//! банк, достаточно реализовать трейт и выбрать реализацию в `build_payment_provider`.

use async_trait::async_trait;
use axum::http::{HeaderMap, HeaderValue};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use crate::modules::auth::jwt::constant_time_eq;

#[derive(Debug, Clone)]
pub struct CreatePaymentRequest {
    pub order_id: Uuid,
    pub amount_minor: i64,
    pub currency: String,
    pub ttl: std::time::Duration,
}

#[derive(Debug, Clone)]
pub struct ProviderPayment {
    pub provider_payment_id: String,
    /// Содержимое QR-кода (строка, которую сканирует банковское приложение).
    pub qr_payload: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebhookStatus {
    Paid,
    Failed,
}

/// Нормализованное уведомление провайдера.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookEvent {
    pub event_id: String,
    pub provider_payment_id: String,
    pub status: WebhookStatus,
    pub amount_minor: i64,
    pub currency: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PaymentProviderError {
    #[error("неверная подпись уведомления")]
    InvalidSignature,
    #[error("некорректное уведомление: {0}")]
    InvalidPayload(String),
    #[error("ошибка провайдера: {0}")]
    Provider(String),
}

#[async_trait]
pub trait PaymentProvider: Send + Sync {
    fn name(&self) -> &'static str;

    /// Создание платежа и QR-кода.
    async fn create_payment(&self, req: CreatePaymentRequest) -> Result<ProviderPayment, PaymentProviderError>;

    /// Проверка подписи и разбор webhook-уведомления.
    fn verify_webhook(&self, headers: &HeaderMap, body: &[u8]) -> Result<WebhookEvent, PaymentProviderError>;

    /// Только для тестовых провайдеров: сформировать подписанное уведомление об оплате.
    fn simulate_paid_webhook(&self, _provider_payment_id: &str, _amount_minor: i64) -> Option<(HeaderMap, Vec<u8>)> {
        None
    }
}

/// Заголовок с подписью mock-провайдера (HMAC-SHA256 тела запроса, hex).
pub const MOCK_SIGNATURE_HEADER: &str = "x-mock-signature";

/// Тестовый провайдер: генерирует QR, проверяет HMAC-подпись уведомлений,
/// умеет имитировать подтверждение оплаты. Настоящих денежных операций нет.
pub struct MockPaymentProvider {
    webhook_secret: String,
}

impl MockPaymentProvider {
    pub fn new(webhook_secret: String) -> Self {
        Self { webhook_secret }
    }

    pub fn sign(&self, body: &[u8]) -> String {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(self.webhook_secret.as_bytes()).expect("HMAC принимает ключ любой длины");
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }
}

#[async_trait]
impl PaymentProvider for MockPaymentProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn create_payment(&self, req: CreatePaymentRequest) -> Result<ProviderPayment, PaymentProviderError> {
        let provider_payment_id = format!("mock_{}", Uuid::new_v4().simple());
        let ttl = ChronoDuration::from_std(req.ttl).map_err(|e| PaymentProviderError::Provider(e.to_string()))?;
        let qr_payload = format!(
            "mockpay://pay?pid={provider_payment_id}&amount={}&currency={}&order={}",
            req.amount_minor, req.currency, req.order_id
        );
        Ok(ProviderPayment {
            provider_payment_id,
            qr_payload,
            expires_at: Utc::now() + ttl,
        })
    }

    fn verify_webhook(&self, headers: &HeaderMap, body: &[u8]) -> Result<WebhookEvent, PaymentProviderError> {
        let signature = headers
            .get(MOCK_SIGNATURE_HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or(PaymentProviderError::InvalidSignature)?;
        if !constant_time_eq(signature, &self.sign(body)) {
            return Err(PaymentProviderError::InvalidSignature);
        }
        let event: WebhookEvent =
            serde_json::from_slice(body).map_err(|e| PaymentProviderError::InvalidPayload(e.to_string()))?;
        if event.amount_minor < 0 {
            return Err(PaymentProviderError::InvalidPayload("отрицательная сумма".into()));
        }
        Ok(event)
    }

    fn simulate_paid_webhook(&self, provider_payment_id: &str, amount_minor: i64) -> Option<(HeaderMap, Vec<u8>)> {
        let event = WebhookEvent {
            // Стабильный event_id: повторная имитация = повторное то же уведомление.
            event_id: format!("evt_{provider_payment_id}"),
            provider_payment_id: provider_payment_id.to_string(),
            status: WebhookStatus::Paid,
            amount_minor,
            currency: "KGS".into(),
        };
        let body = serde_json::to_vec(&event).ok()?;
        let mut headers = HeaderMap::new();
        headers.insert(MOCK_SIGNATURE_HEADER, HeaderValue::from_str(&self.sign(&body)).ok()?);
        Some((headers, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> MockPaymentProvider {
        MockPaymentProvider::new("secret".into())
    }

    #[tokio::test]
    async fn creates_payment_with_qr_payload() {
        let p = provider()
            .create_payment(CreatePaymentRequest {
                order_id: Uuid::nil(),
                amount_minor: 5000,
                currency: "KGS".into(),
                ttl: std::time::Duration::from_secs(900),
            })
            .await
            .unwrap();
        assert!(p.provider_payment_id.starts_with("mock_"));
        assert!(p.qr_payload.contains("amount=5000"));
        assert!(p.qr_payload.contains("currency=KGS"));
        assert!(p.expires_at > Utc::now());
    }

    #[test]
    fn simulated_webhook_verifies() {
        let p = provider();
        let (h, b) = p.simulate_paid_webhook("mock_1", 5000).unwrap();
        let ev = p.verify_webhook(&h, &b).unwrap();
        assert_eq!(ev.provider_payment_id, "mock_1");
        assert_eq!(ev.status, WebhookStatus::Paid);
        assert_eq!(ev.amount_minor, 5000);
    }

    #[test]
    fn rejects_tampered_or_unsigned_webhook() {
        let p = provider();
        let (h, mut b) = p.simulate_paid_webhook("mock_1", 5000).unwrap();
        assert!(matches!(
            p.verify_webhook(&HeaderMap::new(), &b),
            Err(PaymentProviderError::InvalidSignature)
        ));
        b[5] ^= 1;
        assert!(matches!(
            p.verify_webhook(&h, &b),
            Err(PaymentProviderError::InvalidSignature)
        ));
        let other = MockPaymentProvider::new("other".into());
        let (h2, b2) = other.simulate_paid_webhook("mock_1", 5000).unwrap();
        assert!(p.verify_webhook(&h2, &b2).is_err());
    }

    #[test]
    fn same_payment_produces_same_event_id() {
        let p = provider();
        let (_, b1) = p.simulate_paid_webhook("mock_1", 1).unwrap();
        let (_, b2) = p.simulate_paid_webhook("mock_1", 1).unwrap();
        assert_eq!(b1, b2);
    }
}
