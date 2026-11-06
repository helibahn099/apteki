//! Адаптер ручного подтверждения: для аптек без 1С.
//!
//! Запрос сохраняется в `pharmacy_responses` со статусом `pending`,
//! сотрудник видит его в веб-панели и отвечает «есть / частично / нет»
//! с количеством и ценой (см. `modules::pharmacy`).

use async_trait::async_trait;
use uuid::Uuid;

use super::*;

pub const KIND: &str = "manual";

pub struct ManualPharmacyAdapter;

#[async_trait]
impl PharmacyAdapter for ManualPharmacyAdapter {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn get_catalog(&self, _ctx: &IntegrationContext) -> IntegrationResult<Vec<CatalogEntry>> {
        Err(IntegrationError::NotSupported)
    }

    async fn get_stock(&self, _ctx: &IntegrationContext, _skus: &[String]) -> IntegrationResult<Vec<StockEntry>> {
        Err(IntegrationError::NotSupported)
    }

    async fn get_prices(&self, _ctx: &IntegrationContext, _skus: &[String]) -> IntegrationResult<Vec<PriceEntry>> {
        Err(IntegrationError::NotSupported)
    }

    async fn get_locations(&self, _ctx: &IntegrationContext) -> IntegrationResult<Vec<ExternalLocation>> {
        Err(IntegrationError::NotSupported)
    }

    async fn send_query(
        &self,
        _ctx: &IntegrationContext,
        _query: &AvailabilityQuery,
    ) -> IntegrationResult<QueryDispatch> {
        // Запись уже создана поисковым движком; сотрудник ответит через панель.
        Ok(QueryDispatch::Pending)
    }

    async fn receive_response(
        &self,
        _ctx: &IntegrationContext,
        _query_id: Uuid,
    ) -> IntegrationResult<Option<PharmacyAnswer>> {
        // Ответы приходят через API панели, а не опросом.
        Ok(None)
    }
}
