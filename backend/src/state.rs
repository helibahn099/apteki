//! Общее состояние приложения, передаваемое во все обработчики.

use std::sync::Arc;

use sqlx::PgPool;

use crate::config::Config;
use crate::modules::ai::{build_ai_provider, AiProvider};
use crate::modules::auth::otp::{build_otp_sender, OtpSender};
use crate::modules::integrations::AdapterRegistry;
use crate::modules::notifications::{LogNotifier, Notifier};
use crate::modules::payments::{build_payment_provider, provider::PaymentProvider};
use crate::modules::search::rate_limiter::{ApiRateLimiter, PharmacyRateLimiter};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: PgPool,
    pub otp_sender: Arc<dyn OtpSender>,
    pub payments: Arc<dyn PaymentProvider>,
    pub adapters: Arc<AdapterRegistry>,
    /// Глобальный лимит запросов к аптекам (PHARMACY_REQUEST_INTERVAL_SECS).
    pub pharmacy_limiter: Arc<PharmacyRateLimiter>,
    /// Лимит запросов к API по IP.
    pub api_limiter: Arc<ApiRateLimiter>,
    pub notifier: Arc<dyn Notifier>,
    pub ai: Arc<dyn AiProvider>,
}

impl AppState {
    /// Состояние с реализациями по умолчанию (выбираются по конфигурации).
    pub fn new(config: Config, db: PgPool) -> anyhow::Result<Self> {
        let otp_sender = build_otp_sender(&config);
        let payments = build_payment_provider(&config)?;
        let ai = build_ai_provider(&config);
        Ok(Self {
            pharmacy_limiter: Arc::new(PharmacyRateLimiter::new(config.pharmacy_request_interval)),
            api_limiter: Arc::new(ApiRateLimiter::new(config.api_rate_limit_per_minute)),
            adapters: Arc::new(AdapterRegistry::with_defaults()),
            notifier: Arc::new(LogNotifier),
            config: Arc::new(config),
            db,
            otp_sender,
            payments,
            ai,
        })
    }

    pub fn with_otp_sender(mut self, sender: Arc<dyn OtpSender>) -> Self {
        self.otp_sender = sender;
        self
    }

    pub fn with_notifier(mut self, notifier: Arc<dyn Notifier>) -> Self {
        self.notifier = notifier;
        self
    }

    pub fn with_adapters(mut self, adapters: AdapterRegistry) -> Self {
        self.adapters = Arc::new(adapters);
        self
    }
}
