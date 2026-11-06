//! Модуль Medication: каталог препаратов, чёрный список, валидация
//! позиций запроса и алгоритм сопоставления (см. `matching`).

pub mod matching;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::modules::auth::AuthUser;
use crate::state::AppState;
use matching::{MedicationSpec, DEFAULT_UNIT};

/// Позиция поискового запроса, присланная клиентом.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MedicationInput {
    pub catalog_id: Option<Uuid>,
    pub name: String,
    pub active_substance: Option<String>,
    pub dosage: Option<String>,
    pub concentration: Option<String>,
    pub form: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: u32,
    /// Единица измерения; по умолчанию — «шт».
    pub unit: Option<String>,
}

/// Препарат заказа, сохранённый в БД.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct MedicationRow {
    pub id: Uuid,
    pub position: i32,
    pub catalog_id: Option<Uuid>,
    pub name: String,
    pub active_substance: Option<String>,
    pub dosage: Option<String>,
    pub concentration: Option<String>,
    pub form: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: i32,
    pub unit: String,
}

impl MedicationRow {
    pub fn to_spec(&self) -> MedicationSpec {
        MedicationSpec {
            name: self.name.clone(),
            active_substance: self.active_substance.clone(),
            dosage: self.dosage.clone(),
            concentration: self.concentration.clone(),
            form: self.form.clone(),
            manufacturer: self.manufacturer.clone(),
            quantity: self.quantity.max(0) as u32,
        }
    }
}

const MAX_TEXT_LEN: usize = 200;
const MAX_QUANTITY: u32 = 1000;

fn clean_opt(v: &Option<String>) -> Option<String> {
    v.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl MedicationInput {
    /// Валидация и нормализация пробелов. Единица по умолчанию — «шт».
    pub fn validate(&self) -> Result<MedicationInput, String> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err("название препарата не может быть пустым".into());
        }
        if self.quantity == 0 || self.quantity > MAX_QUANTITY {
            return Err(format!("количество должно быть от 1 до {MAX_QUANTITY}"));
        }
        let fields = [
            &self.active_substance,
            &self.dosage,
            &self.concentration,
            &self.form,
            &self.manufacturer,
            &self.unit,
        ];
        if name.chars().count() > MAX_TEXT_LEN
            || fields
                .iter()
                .any(|f| f.as_ref().is_some_and(|s| s.chars().count() > MAX_TEXT_LEN))
        {
            return Err(format!("текстовые поля не длиннее {MAX_TEXT_LEN} символов"));
        }
        Ok(MedicationInput {
            catalog_id: self.catalog_id,
            name,
            active_substance: clean_opt(&self.active_substance),
            dosage: clean_opt(&self.dosage),
            concentration: clean_opt(&self.concentration),
            form: clean_opt(&self.form),
            manufacturer: clean_opt(&self.manufacturer),
            quantity: self.quantity,
            unit: Some(clean_opt(&self.unit).unwrap_or_else(|| DEFAULT_UNIT.to_string())),
        })
    }
}

/// Валидирует список позиций заказа.
pub fn validate_items(items: &[MedicationInput], max: usize) -> Result<Vec<MedicationInput>, AppError> {
    if items.is_empty() {
        return Err(AppError::Validation("добавьте хотя бы один препарат".into()));
    }
    if items.len() > max {
        return Err(AppError::Validation(format!(
            "не больше {max} препаратов в одном поиске"
        )));
    }
    items
        .iter()
        .enumerate()
        .map(|(i, m)| {
            m.validate()
                .map_err(|e| AppError::Validation(format!("позиция {}: {e}", i + 1)))
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct BlacklistEntry {
    pub id: Uuid,
    pub name: Option<String>,
    pub active_substance: Option<String>,
    pub reason: String,
}

/// Проверка позиции по чёрному списку (сравнение нормализованных строк).
pub fn is_blacklisted(item: &MedicationInput, blacklist: &[BlacklistEntry]) -> Option<String> {
    use matching::normalize;
    let name = normalize(&item.name);
    let substance = item.active_substance.as_deref().map(normalize);
    blacklist.iter().find_map(|b| {
        let by_name = b.name.as_deref().map(normalize).is_some_and(|n| n == name);
        let by_substance = b
            .active_substance
            .as_deref()
            .map(normalize)
            .is_some_and(|s| s == name || substance.as_deref() == Some(s.as_str()));
        (by_name || by_substance).then(|| b.reason.clone())
    })
}

pub async fn load_blacklist(db: &PgPool) -> AppResult<Vec<BlacklistEntry>> {
    Ok(
        sqlx::query_as::<_, BlacklistEntry>("SELECT id, name, active_substance, reason FROM medication_blacklist")
            .fetch_all(db)
            .await?,
    )
}

pub async fn load_order_medications(db: &PgPool, order_id: Uuid) -> AppResult<Vec<MedicationRow>> {
    Ok(sqlx::query_as::<_, MedicationRow>(
        "SELECT id, position, catalog_id, name, active_substance, dosage, concentration, form,
                manufacturer, quantity, unit
         FROM medications WHERE search_order_id = $1 ORDER BY position",
    )
    .bind(order_id)
    .fetch_all(db)
    .await?)
}

// ---------------------------------------------------------------------------
// HTTP: каталог
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CatalogItem {
    pub id: Uuid,
    pub name: String,
    pub active_substance: Option<String>,
    pub dosage: Option<String>,
    pub concentration: Option<String>,
    pub form: Option<String>,
    pub manufacturer: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CatalogQuery {
    pub q: String,
    pub limit: Option<i64>,
}

/// GET /api/v1/medications/catalog?q= — подсказки по каталогу.
async fn search_catalog(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<CatalogQuery>,
) -> AppResult<Json<Vec<CatalogItem>>> {
    let term = q.q.trim();
    if term.chars().count() < 2 {
        return Err(AppError::Validation("строка поиска — минимум 2 символа".into()));
    }
    let limit = q.limit.unwrap_or(20).clamp(1, 50);
    let pattern = format!("%{}%", term.replace(['%', '_'], ""));
    let items = sqlx::query_as::<_, CatalogItem>(
        "SELECT id, name, active_substance, dosage, concentration, form, manufacturer
         FROM medication_catalog
         WHERE name ILIKE $1 OR active_substance ILIKE $1
         ORDER BY name LIMIT $2",
    )
    .bind(pattern)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(items))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/medications/catalog", get(search_catalog))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(name: &str, qty: u32) -> MedicationInput {
        MedicationInput {
            catalog_id: None,
            name: name.into(),
            active_substance: None,
            dosage: None,
            concentration: None,
            form: None,
            manufacturer: None,
            quantity: qty,
            unit: None,
        }
    }

    #[test]
    fn default_unit_is_pieces() {
        let v = input("Парацетамол", 2).validate().unwrap();
        assert_eq!(v.unit.as_deref(), Some("шт"));
    }

    #[test]
    fn rejects_empty_name_and_bad_quantity() {
        assert!(input("  ", 1).validate().is_err());
        assert!(input("Аспирин", 0).validate().is_err());
        assert!(input("Аспирин", 5000).validate().is_err());
    }

    #[test]
    fn trims_and_drops_empty_optional_fields() {
        let mut i = input(" Нурофен ", 1);
        i.dosage = Some("   ".into());
        i.form = Some(" сироп ".into());
        let v = i.validate().unwrap();
        assert_eq!(v.name, "Нурофен");
        assert_eq!(v.dosage, None);
        assert_eq!(v.form.as_deref(), Some("сироп"));
    }

    #[test]
    fn validate_items_limits() {
        assert!(validate_items(&[], 5).is_err());
        let many: Vec<_> = (0..6).map(|_| input("А", 1)).collect();
        assert!(validate_items(&many, 5).is_err());
        assert!(validate_items(&many[..5], 5).is_ok());
    }

    #[test]
    fn blacklist_matches_by_substance_or_name() {
        let bl = vec![BlacklistEntry {
            id: Uuid::nil(),
            name: None,
            active_substance: Some("Трамадол".into()),
            reason: "запрещено".into(),
        }];
        assert!(is_blacklisted(&input("трамадол", 1), &bl).is_some());
        let mut by_subst = input("Трамал", 1);
        by_subst.active_substance = Some("трамадол".into());
        assert!(is_blacklisted(&by_subst, &bl).is_some());
        assert!(is_blacklisted(&input("Парацетамол", 1), &bl).is_none());
    }
}
