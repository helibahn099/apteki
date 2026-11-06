//! Mock-адаптер аптеки: детерминированные тестовые данные без внешних систем.
//!
//! Режим задаётся в `integrations.config`:
//! * `{"mode": "random"}`      — по умолчанию: псевдослучайно (детерминированно
//!   от пары «точка + препарат»): ~60% есть полностью, ~20% частично, ~20% нет;
//! * `{"mode": "always_full"}` — всё есть;
//! * `{"mode": "partial"}`     — есть половина запрошенного;
//! * `{"mode": "always_none"}` — ничего нет;
//! * `{"mode": "pending"}`     — ответ придёт позже (как у ручного режима);
//! * `{"mode": "fail"}`        — имитация ошибки внешней системы.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::*;

pub const KIND: &str = "mock";

pub struct MockPharmacyAdapter;

/// Детерминированное число 0..100 от точки и названия препарата.
fn bucket(location_id: Uuid, key: &str) -> u8 {
    let digest = Sha256::digest(format!("{location_id}:{}", key.to_lowercase()));
    digest[0] % 100
}

/// Детерминированная цена 50..1049 сом (в тыйынах).
fn price(location_id: Uuid, key: &str) -> i64 {
    let digest = Sha256::digest(format!("price:{location_id}:{}", key.to_lowercase()));
    let n = u16::from_be_bytes([digest[0], digest[1]]) as i64;
    (50 + n % 1000) * 100
}

fn mode(ctx: &IntegrationContext) -> String {
    ctx.config
        .get("mode")
        .and_then(|m| m.as_str())
        .unwrap_or("random")
        .to_string()
}

/// Строит ответ mock-аптеки на запрос.
pub fn build_answer(ctx: &IntegrationContext, query: &AvailabilityQuery, mode: &str) -> PharmacyAnswer {
    let mut items = Vec::new();
    for q in &query.items {
        let requested = q.spec.quantity;
        let available = match mode {
            "always_full" => requested,
            "always_none" => 0,
            "partial" => (requested / 2).max(1).min(requested.saturating_sub(1)),
            _ => match bucket(ctx.location_id, &q.spec.name) {
                0..=59 => requested,
                60..=79 => requested / 2,
                _ => 0,
            },
        };
        if available == 0 {
            continue;
        }
        items.push(AnswerItem {
            medication_id: q.medication_id,
            offered: OfferedItem {
                name: Some(q.spec.name.clone()),
                active_substance: q.spec.active_substance.clone(),
                dosage: q.spec.dosage.clone(),
                concentration: q.spec.concentration.clone(),
                form: q.spec.form.clone(),
                manufacturer: q.spec.manufacturer.clone(),
                available_quantity: available,
                unit_price_minor: Some(price(ctx.location_id, &q.spec.name)),
            },
        });
    }
    let full = !query.items.is_empty()
        && query.items.iter().all(|q| {
            items
                .iter()
                .any(|i| i.medication_id == q.medication_id && i.offered.available_quantity >= q.spec.quantity)
        });
    let availability = if items.is_empty() {
        Availability::None
    } else if full {
        Availability::Full
    } else {
        Availability::Partial
    };
    PharmacyAnswer { availability, items }
}

#[async_trait]
impl PharmacyAdapter for MockPharmacyAdapter {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn get_catalog(&self, _ctx: &IntegrationContext) -> IntegrationResult<Vec<CatalogEntry>> {
        Ok(vec![
            CatalogEntry {
                sku: "MOCK-001".into(),
                name: "Парацетамол".into(),
                active_substance: Some("парацетамол".into()),
                dosage: Some("500 мг".into()),
                form: Some("таблетки".into()),
                manufacturer: Some("Фармстандарт".into()),
            },
            CatalogEntry {
                sku: "MOCK-002".into(),
                name: "Нурофен".into(),
                active_substance: Some("ибупрофен".into()),
                dosage: Some("200 мг".into()),
                form: Some("таблетки".into()),
                manufacturer: Some("Reckitt Benckiser".into()),
            },
        ])
    }

    async fn get_stock(&self, ctx: &IntegrationContext, skus: &[String]) -> IntegrationResult<Vec<StockEntry>> {
        Ok(skus
            .iter()
            .map(|s| StockEntry {
                sku: s.clone(),
                quantity: bucket(ctx.location_id, s) as u32,
            })
            .collect())
    }

    async fn get_prices(&self, ctx: &IntegrationContext, skus: &[String]) -> IntegrationResult<Vec<PriceEntry>> {
        Ok(skus
            .iter()
            .map(|s| PriceEntry {
                sku: s.clone(),
                unit_price_minor: price(ctx.location_id, s),
            })
            .collect())
    }

    async fn get_locations(&self, ctx: &IntegrationContext) -> IntegrationResult<Vec<ExternalLocation>> {
        Ok(vec![ExternalLocation {
            external_id: format!("MOCK-{}", ctx.location_id),
            address: "г. Бишкек, тестовый адрес".into(),
            lat: 42.8746,
            lon: 74.5698,
        }])
    }

    async fn send_query(
        &self,
        ctx: &IntegrationContext,
        query: &AvailabilityQuery,
    ) -> IntegrationResult<QueryDispatch> {
        match mode(ctx).as_str() {
            "fail" => Err(IntegrationError::External("mock: имитация сбоя".into())),
            "pending" => Ok(QueryDispatch::Pending),
            m => Ok(QueryDispatch::Answered(build_answer(ctx, query, m))),
        }
    }

    async fn receive_response(
        &self,
        _ctx: &IntegrationContext,
        _query_id: Uuid,
    ) -> IntegrationResult<Option<PharmacyAnswer>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::medication::matching::MedicationSpec;

    fn ctx(mode: &str) -> IntegrationContext {
        IntegrationContext {
            pharmacy_id: Uuid::new_v4(),
            location_id: Uuid::new_v4(),
            config: serde_json::json!({"mode": mode}),
        }
    }

    fn query(qty: u32) -> AvailabilityQuery {
        AvailabilityQuery {
            query_id: Uuid::new_v4(),
            search_id: Uuid::new_v4(),
            items: vec![QueryItem {
                medication_id: Uuid::new_v4(),
                spec: MedicationSpec {
                    name: "Парацетамол".into(),
                    quantity: qty,
                    ..Default::default()
                },
                unit: "шт".into(),
            }],
        }
    }

    #[tokio::test]
    async fn always_full_returns_full() {
        let a = MockPharmacyAdapter;
        match a.send_query(&ctx("always_full"), &query(10)).await.unwrap() {
            QueryDispatch::Answered(ans) => {
                assert_eq!(ans.availability, Availability::Full);
                assert_eq!(ans.items[0].offered.available_quantity, 10);
                assert!(ans.items[0].offered.unit_price_minor.unwrap() >= 5000);
            }
            _ => panic!("ожидался немедленный ответ"),
        }
    }

    #[tokio::test]
    async fn partial_and_none_modes() {
        let a = MockPharmacyAdapter;
        let QueryDispatch::Answered(p) = a.send_query(&ctx("partial"), &query(10)).await.unwrap() else {
            panic!()
        };
        assert_eq!(p.availability, Availability::Partial);
        assert_eq!(p.items[0].offered.available_quantity, 5);
        let QueryDispatch::Answered(n) = a.send_query(&ctx("always_none"), &query(10)).await.unwrap() else {
            panic!()
        };
        assert_eq!(n.availability, Availability::None);
        assert!(n.items.is_empty());
    }

    #[tokio::test]
    async fn fail_and_pending_modes() {
        let a = MockPharmacyAdapter;
        assert!(a.send_query(&ctx("fail"), &query(1)).await.is_err());
        assert!(matches!(
            a.send_query(&ctx("pending"), &query(1)).await.unwrap(),
            QueryDispatch::Pending
        ));
    }

    #[test]
    fn random_mode_is_deterministic() {
        let c = ctx("random");
        let q = query(4);
        let a1 = build_answer(&c, &q, "random");
        let a2 = build_answer(&c, &q, "random");
        assert_eq!(a1.availability, a2.availability);
        assert_eq!(a1.items.len(), a2.items.len());
    }

    #[test]
    fn partial_of_one_unit_is_never_full() {
        let a = build_answer(&ctx("partial"), &query(1), "partial");
        // Для 1 шт «половина» = 0 → позиции нет.
        assert_eq!(a.availability, Availability::None);
    }

    #[tokio::test]
    async fn catalog_stock_prices_locations() {
        let a = MockPharmacyAdapter;
        let c = ctx("random");
        let cat = a.get_catalog(&c).await.unwrap();
        assert_eq!(cat.len(), 2);
        let skus: Vec<String> = cat.iter().map(|e| e.sku.clone()).collect();
        assert_eq!(a.get_stock(&c, &skus).await.unwrap().len(), 2);
        assert!(a
            .get_prices(&c, &skus)
            .await
            .unwrap()
            .iter()
            .all(|p| p.unit_price_minor > 0));
        assert_eq!(a.get_locations(&c).await.unwrap().len(), 1);
    }
}
