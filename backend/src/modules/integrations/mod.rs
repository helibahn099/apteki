//! Модуль Integrations: универсальный интерфейс подключения аптек.
//!
//! Бизнес-логика не знает о конкретных системах (1С и т.п.) — она работает
//! только с трейтом `PharmacyAdapter`. Для каждой аптечной сети пишется свой
//! адаптер и регистрируется в `AdapterRegistry` под уникальным `kind`.
//! Связь аптеки с адаптером хранится в таблице `integrations`.
//!
//! Встроенные адаптеры:
//! * `mock`   — тестовые данные (без внешних систем);
//! * `manual` — ручное подтверждение сотрудником через веб-панель аптеки.

pub mod manual;
pub mod mock;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::modules::medication::matching::{MedicationSpec, OfferedItem};

#[derive(Debug, thiserror::Error)]
pub enum IntegrationError {
    #[error("операция не поддерживается адаптером")]
    NotSupported,
    #[error("ошибка внешней системы: {0}")]
    External(String),
    #[error("некорректная конфигурация интеграции: {0}")]
    Config(String),
}

pub type IntegrationResult<T> = Result<T, IntegrationError>;

/// Контекст вызова: какая аптека/точка и её настройки интеграции.
#[derive(Debug, Clone)]
pub struct IntegrationContext {
    pub pharmacy_id: Uuid,
    pub location_id: Uuid,
    pub config: serde_json::Value,
}

/// Позиция запроса о наличии.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryItem {
    pub medication_id: Uuid,
    pub spec: MedicationSpec,
    pub unit: String,
}

/// Запрос на подтверждение наличия в конкретной торговой точке.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailabilityQuery {
    /// ID записи `pharmacy_responses` — уникален для пары (поиск, точка).
    pub query_id: Uuid,
    pub search_id: Uuid,
    pub items: Vec<QueryItem>,
}

/// Ответ аптеки по наличию в целом (как его заявил сотрудник/система).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Есть полностью.
    Full,
    /// Есть частично.
    Partial,
    /// Нет в наличии.
    None,
}

impl Availability {
    pub fn as_db(&self) -> &'static str {
        match self {
            Availability::Full => "full",
            Availability::Partial => "partial",
            Availability::None => "none",
        }
    }
}

/// Предложение аптеки по позиции запроса (их может быть несколько на позицию).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerItem {
    pub medication_id: Uuid,
    #[serde(flatten)]
    pub offered: OfferedItem,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PharmacyAnswer {
    pub availability: Availability,
    pub items: Vec<AnswerItem>,
}

/// Результат отправки запроса.
#[derive(Debug, Clone)]
pub enum QueryDispatch {
    /// Система ответила сразу (например, по API остатков).
    Answered(PharmacyAnswer),
    /// Ответ придёт позже (сотрудник ответит в панели или адаптер опрашивается).
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub sku: String,
    pub name: String,
    pub active_substance: Option<String>,
    pub dosage: Option<String>,
    pub form: Option<String>,
    pub manufacturer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StockEntry {
    pub sku: String,
    pub quantity: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceEntry {
    pub sku: String,
    pub unit_price_minor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalLocation {
    pub external_id: String,
    pub address: String,
    pub lat: f64,
    pub lon: f64,
}

/// Универсальный интерфейс адаптера аптечной системы.
#[async_trait]
pub trait PharmacyAdapter: Send + Sync {
    /// Уникальный тип адаптера (значение `integrations.kind`).
    fn kind(&self) -> &'static str;

    /// Каталог товаров аптеки.
    async fn get_catalog(&self, ctx: &IntegrationContext) -> IntegrationResult<Vec<CatalogEntry>>;

    /// Остатки по SKU в торговой точке.
    async fn get_stock(&self, ctx: &IntegrationContext, skus: &[String]) -> IntegrationResult<Vec<StockEntry>>;

    /// Цены по SKU.
    async fn get_prices(&self, ctx: &IntegrationContext, skus: &[String]) -> IntegrationResult<Vec<PriceEntry>>;

    /// Торговые точки сети (для сопоставления с `pharmacy_locations.external_id`).
    async fn get_locations(&self, ctx: &IntegrationContext) -> IntegrationResult<Vec<ExternalLocation>>;

    /// Отправка запроса на подтверждение наличия.
    async fn send_query(&self, ctx: &IntegrationContext, query: &AvailabilityQuery)
        -> IntegrationResult<QueryDispatch>;

    /// Получение ответа на ранее отправленный запрос (для опрашиваемых систем).
    async fn receive_response(
        &self,
        ctx: &IntegrationContext,
        query_id: Uuid,
    ) -> IntegrationResult<Option<PharmacyAnswer>>;
}

/// Реестр адаптеров по `kind`.
#[derive(Clone, Default)]
pub struct AdapterRegistry {
    adapters: HashMap<String, Arc<dyn PharmacyAdapter>>,
}

impl AdapterRegistry {
    /// Реестр со встроенными адаптерами (`mock`, `manual`).
    pub fn with_defaults() -> Self {
        let mut r = Self::default();
        r.register(Arc::new(mock::MockPharmacyAdapter));
        r.register(Arc::new(manual::ManualPharmacyAdapter));
        // Новые адаптеры регистрируются здесь, например:
        // r.register(Arc::new(onec_neman::NemanOneCAdapter::new(...)));
        r
    }

    pub fn register(&mut self, adapter: Arc<dyn PharmacyAdapter>) {
        self.adapters.insert(adapter.kind().to_string(), adapter);
    }

    /// Адаптер по типу. Если интеграция не настроена — ручной режим.
    pub fn resolve(&self, kind: Option<&str>) -> Option<Arc<dyn PharmacyAdapter>> {
        self.adapters.get(kind.unwrap_or(manual::KIND)).cloned()
    }

    pub fn kinds(&self) -> Vec<String> {
        let mut k: Vec<_> = self.adapters.keys().cloned().collect();
        k.sort();
        k
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_defaults_and_fallback() {
        let r = AdapterRegistry::with_defaults();
        assert_eq!(r.kinds(), vec!["manual".to_string(), "mock".to_string()]);
        assert_eq!(r.resolve(Some("mock")).unwrap().kind(), "mock");
        assert_eq!(r.resolve(None).unwrap().kind(), "manual");
        assert!(r.resolve(Some("unknown_1c")).is_none());
    }

    #[test]
    fn answer_item_serialization_is_flat() {
        let item = AnswerItem {
            medication_id: Uuid::nil(),
            offered: OfferedItem {
                available_quantity: 3,
                unit_price_minor: Some(1500),
                ..Default::default()
            },
        };
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v["available_quantity"], 3);
        assert_eq!(v["unit_price_minor"], 1500);
    }
}
