//! Обработка ответов аптек: сопоставление, сохранение снимка результата,
//! создание чата при положительном ответе.

use chrono::Duration as ChronoDuration;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::modules::chat;
use crate::modules::integrations::{Availability, PharmacyAnswer};
use crate::modules::medication::load_order_medications;
use crate::modules::medication::matching::{evaluate_pharmacy, MatchField, MatchStatus, OfferedItem};
use crate::modules::notifications::Notification;
use crate::state::AppState;

/// Позиция в сохранённом снимке ответа аптеки (`pharmacy_responses.items`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseItemView {
    pub medication_id: Uuid,
    pub name: String,
    pub unit: String,
    pub status: MatchStatus,
    pub mismatched_fields: Vec<MatchField>,
    pub requested_quantity: u32,
    pub available_quantity: u32,
    pub missing_quantity: u32,
    pub unit_price_minor: Option<i64>,
    pub subtotal_minor: i64,
    pub offered: Option<OfferedItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyOutcome {
    pub response_id: Uuid,
    pub match_status: MatchStatus,
    pub total_price_minor: i64,
    pub chat_id: Option<Uuid>,
}

/// Проверка ответа сотрудника: количества и цены неотрицательные, согласованность с «нет в наличии».
pub fn validate_answer(answer: &PharmacyAnswer) -> Result<(), String> {
    for item in &answer.items {
        if item.offered.available_quantity > 100_000 {
            return Err("слишком большое количество".into());
        }
        if let Some(p) = item.offered.unit_price_minor {
            if !(0..=100_000_000).contains(&p) {
                return Err("цена должна быть от 0 до 1 000 000 сом".into());
            }
        }
    }
    if answer.availability == Availability::None && answer.items.iter().any(|i| i.offered.available_quantity > 0) {
        return Err("при ответе «нет в наличии» не указывайте доступное количество".into());
    }
    if answer.availability != Availability::None && answer.items.is_empty() {
        return Err("укажите хотя бы одну доступную позицию".into());
    }
    Ok(())
}

/// Применяет ответ аптеки к запросу `response_id`.
pub async fn apply_answer(
    state: &AppState,
    response_id: Uuid,
    answer: &PharmacyAnswer,
    answered_by: Option<Uuid>,
) -> AppResult<ApplyOutcome> {
    validate_answer(answer).map_err(AppError::Validation)?;
    let mut tx = state.db.begin().await?;
    let row: Option<(Uuid, Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT search_order_id, pharmacy_id, location_id, status FROM pharmacy_responses WHERE id = $1 FOR UPDATE",
    )
    .bind(response_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (order_id, pharmacy_id, location_id, status) =
        row.ok_or_else(|| AppError::NotFound("запрос аптеке не найден".into()))?;
    if status != "pending" {
        return Err(AppError::Conflict(format!(
            "на запрос уже нельзя ответить (статус: {status})"
        )));
    }

    let meds = load_order_medications(&state.db, order_id).await?;
    if let Some(bad) = answer
        .items
        .iter()
        .find(|i| !meds.iter().any(|m| m.id == i.medication_id))
    {
        return Err(AppError::Validation(format!(
            "позиция {} не относится к этому запросу",
            bad.medication_id
        )));
    }
    let specs: Vec<_> = meds.iter().map(|m| m.to_spec()).collect();
    let offers: Vec<Vec<OfferedItem>> = meds
        .iter()
        .map(|m| {
            answer
                .items
                .iter()
                .filter(|i| i.medication_id == m.id)
                .map(|i| i.offered.clone())
                .collect()
        })
        .collect();
    let eval = evaluate_pharmacy(&specs, &offers);
    let items: Vec<ResponseItemView> = eval
        .items
        .iter()
        .map(|e| {
            let m = &meds[e.requested_index];
            ResponseItemView {
                medication_id: m.id,
                name: m.name.clone(),
                unit: m.unit.clone(),
                status: e.status,
                mismatched_fields: e.mismatched_fields.clone(),
                requested_quantity: e.requested_quantity,
                available_quantity: e.purchasable_quantity,
                missing_quantity: e.missing_quantity,
                unit_price_minor: e.unit_price_minor,
                subtotal_minor: e.subtotal_minor,
                offered: e.offered.clone(),
            }
        })
        .collect();

    sqlx::query(
        "UPDATE pharmacy_responses SET status = 'answered', availability = $2, match_status = $3,
                items = $4, total_price_minor = $5, answered_at = now(), answered_by = $6
         WHERE id = $1",
    )
    .bind(response_id)
    .bind(answer.availability.as_db())
    .bind(eval.status.as_db())
    .bind(serde_json::to_value(&items).map_err(anyhow::Error::from)?)
    .bind(eval.total_price_minor)
    .bind(answered_by)
    .execute(&mut *tx)
    .await?;

    let mut chat_id = None;
    let mut user_to_notify = None;
    if eval.status != MatchStatus::NoMatch {
        let (user_id, paid_at): (Uuid, Option<chrono::DateTime<chrono::Utc>>) =
            sqlx::query_as("SELECT user_id, paid_at FROM search_orders WHERE id = $1")
                .bind(order_id)
                .fetch_one(&mut *tx)
                .await?;
        // Чат активен 48 часов от момента успешной оплаты.
        let active_until =
            paid_at.unwrap_or_else(chrono::Utc::now) + ChronoDuration::hours(state.config.chat_active_hours);
        chat_id = Some(
            chat::ensure_chat(
                &mut tx,
                order_id,
                user_id,
                pharmacy_id,
                location_id,
                response_id,
                active_until,
            )
            .await?,
        );
        user_to_notify = Some(user_id);
    }
    tx.commit().await?;

    tracing::info!(
        search_id = %order_id,
        pharmacy_id = %pharmacy_id,
        location_id = %location_id,
        match_status = eval.status.as_db(),
        "получен ответ аптеки"
    );
    if let (Some(user_id), Some(chat_id)) = (user_to_notify, chat_id) {
        state
            .notifier
            .notify(Notification::PharmacyAnswered {
                user_id,
                search_id: order_id,
                chat_id,
            })
            .await;
    }
    Ok(ApplyOutcome {
        response_id,
        match_status: eval.status,
        total_price_minor: eval.total_price_minor,
        chat_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::integrations::AnswerItem;

    fn item(qty: u32, price: Option<i64>) -> AnswerItem {
        AnswerItem {
            medication_id: Uuid::new_v4(),
            offered: OfferedItem {
                available_quantity: qty,
                unit_price_minor: price,
                ..Default::default()
            },
        }
    }

    #[test]
    fn valid_answers() {
        assert!(validate_answer(&PharmacyAnswer {
            availability: Availability::None,
            items: vec![]
        })
        .is_ok());
        assert!(validate_answer(&PharmacyAnswer {
            availability: Availability::Full,
            items: vec![item(5, Some(1000))]
        })
        .is_ok());
    }

    #[test]
    fn invalid_answers() {
        assert!(validate_answer(&PharmacyAnswer {
            availability: Availability::Full,
            items: vec![]
        })
        .is_err());
        assert!(validate_answer(&PharmacyAnswer {
            availability: Availability::None,
            items: vec![item(3, None)]
        })
        .is_err());
        assert!(validate_answer(&PharmacyAnswer {
            availability: Availability::Partial,
            items: vec![item(1, Some(-5))]
        })
        .is_err());
    }
}
