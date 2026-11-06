//! Алгоритм сопоставления препаратов.
//!
//! Полностью независим от ИИ: базовый поиск работает только на нём.
//!
//! Сравниваются 7 параметров: название, действующее вещество, дозировка,
//! концентрация, форма выпуска, производитель, количество.
//!
//! Правила:
//! * если параметр в запросе пользователя не указан — он не учитывается
//!   (подходит любое значение);
//! * если параметр не указан в предложении аптеки — считается, что аптека
//!   предлагает ровно запрошенное (так отвечают сотрудники в ручном режиме:
//!   «есть / частично / нет» по позиции из запроса);
//! * количество не совпадает, если в наличии меньше запрошенного;
//! * 0 расхождений → `FullMatch`, 1–2 → `PartialMatch`, 3+ → `NoMatch`;
//! * если в наличии 0 штук — всегда `NoMatch` (купить нечего).

use serde::{Deserialize, Serialize};

/// Единица измерения по умолчанию (если форма выпуска не указана).
pub const DEFAULT_UNIT: &str = "шт";

/// Максимально допустимое число расхождений для частичного совпадения.
pub const MAX_PARTIAL_MISMATCHES: usize = 2;

/// Запрошенный пользователем препарат.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MedicationSpec {
    pub name: String,
    pub active_substance: Option<String>,
    pub dosage: Option<String>,
    pub concentration: Option<String>,
    pub form: Option<String>,
    pub manufacturer: Option<String>,
    pub quantity: u32,
}

/// Предложение аптеки по позиции.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OfferedItem {
    /// `None` — «как в запросе».
    pub name: Option<String>,
    pub active_substance: Option<String>,
    pub dosage: Option<String>,
    pub concentration: Option<String>,
    pub form: Option<String>,
    pub manufacturer: Option<String>,
    pub available_quantity: u32,
    /// Цена за единицу в тыйынах.
    pub unit_price_minor: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MatchField {
    Name,
    ActiveSubstance,
    Dosage,
    Concentration,
    Form,
    Manufacturer,
    Quantity,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MatchStatus {
    // Порядок вариантов важен: используется для сортировки (лучшие — первыми).
    FullMatch,
    PartialMatch,
    NoMatch,
}

impl MatchStatus {
    pub fn as_db(&self) -> &'static str {
        match self {
            MatchStatus::FullMatch => "full_match",
            MatchStatus::PartialMatch => "partial_match",
            MatchStatus::NoMatch => "no_match",
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "full_match" => Some(Self::FullMatch),
            "partial_match" => Some(Self::PartialMatch),
            "no_match" => Some(Self::NoMatch),
            _ => None,
        }
    }

    /// Статус по числу расхождений.
    pub fn from_mismatches(count: usize) -> Self {
        match count {
            0 => Self::FullMatch,
            n if n <= MAX_PARTIAL_MISMATCHES => Self::PartialMatch,
            _ => Self::NoMatch,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchResult {
    pub status: MatchStatus,
    pub mismatched_fields: Vec<MatchField>,
}

/// Нормализация строки для сравнения: регистр, «ё»→«е», пробелы,
/// латинские единицы → кириллические.
pub fn normalize(value: &str) -> String {
    let lowered = value.trim().to_lowercase().replace('ё', "е").replace(',', ".");
    let collapsed: String = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.replace("mg", "мг").replace("ml", "мл").replace("mcg", "мкг")
}

/// Нормализация дозировки/концентрации: дополнительно убираются пробелы
/// («500 мг» == «500мг»).
fn normalize_compact(value: &str) -> String {
    normalize(value).replace(' ', "")
}

fn field_differs(requested: &Option<String>, offered: &Option<String>, compact: bool) -> bool {
    match (requested, offered) {
        (Some(r), Some(o)) => {
            if compact {
                normalize_compact(r) != normalize_compact(o)
            } else {
                normalize(r) != normalize(o)
            }
        }
        // Пользователь не указал параметр, либо аптека предлагает «как в запросе».
        _ => false,
    }
}

/// Сравнивает запрошенный препарат с предложением аптеки.
pub fn compare(requested: &MedicationSpec, offered: &OfferedItem) -> MatchResult {
    if offered.available_quantity == 0 {
        return MatchResult {
            status: MatchStatus::NoMatch,
            mismatched_fields: vec![MatchField::Quantity],
        };
    }
    let mut mismatched = Vec::new();
    let name_req = Some(requested.name.clone());
    if field_differs(&name_req, &offered.name, false) {
        mismatched.push(MatchField::Name);
    }
    if field_differs(&requested.active_substance, &offered.active_substance, false) {
        mismatched.push(MatchField::ActiveSubstance);
    }
    if field_differs(&requested.dosage, &offered.dosage, true) {
        mismatched.push(MatchField::Dosage);
    }
    if field_differs(&requested.concentration, &offered.concentration, true) {
        mismatched.push(MatchField::Concentration);
    }
    if field_differs(&requested.form, &offered.form, false) {
        mismatched.push(MatchField::Form);
    }
    if field_differs(&requested.manufacturer, &offered.manufacturer, false) {
        mismatched.push(MatchField::Manufacturer);
    }
    if offered.available_quantity < requested.quantity {
        mismatched.push(MatchField::Quantity);
    }
    MatchResult {
        status: MatchStatus::from_mismatches(mismatched.len()),
        mismatched_fields: mismatched,
    }
}

/// Результат сопоставления одной позиции запроса.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemEvaluation {
    pub requested_index: usize,
    pub status: MatchStatus,
    pub mismatched_fields: Vec<MatchField>,
    pub requested_quantity: u32,
    /// Сколько штук можно купить (не больше запрошенного).
    pub purchasable_quantity: u32,
    /// Недостающее количество.
    pub missing_quantity: u32,
    pub unit_price_minor: Option<i64>,
    /// Стоимость доступного количества (тыйын).
    pub subtotal_minor: i64,
    pub offered: Option<OfferedItem>,
}

/// Итог по аптеке.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PharmacyEvaluation {
    pub status: MatchStatus,
    pub items: Vec<ItemEvaluation>,
    /// Общая стоимость доступного набора (тыйын).
    pub total_price_minor: i64,
}

/// Выбирает лучшее предложение для позиции: меньше расхождений, затем дешевле.
fn best_offer<'a>(requested: &MedicationSpec, candidates: &'a [OfferedItem]) -> Option<(&'a OfferedItem, MatchResult)> {
    candidates
        .iter()
        .map(|o| (o, compare(requested, o)))
        .min_by(|(oa, a), (ob, b)| {
            a.status
                .cmp(&b.status)
                .then(a.mismatched_fields.len().cmp(&b.mismatched_fields.len()))
                .then(
                    oa.unit_price_minor
                        .unwrap_or(i64::MAX)
                        .cmp(&ob.unit_price_minor.unwrap_or(i64::MAX)),
                )
        })
}

/// Сопоставляет весь запрос с ответом аптеки.
///
/// `offers[i]` — список кандидатов, предложенных аптекой на i-ю позицию запроса.
/// * все позиции `FullMatch` → аптека `FullMatch`;
/// * ни одной позиции, которую можно купить (все `NoMatch`) → `NoMatch`;
/// * иначе → `PartialMatch`.
pub fn evaluate_pharmacy(requested: &[MedicationSpec], offers: &[Vec<OfferedItem>]) -> PharmacyEvaluation {
    let mut items = Vec::with_capacity(requested.len());
    for (idx, req) in requested.iter().enumerate() {
        let candidates = offers.get(idx).map(Vec::as_slice).unwrap_or(&[]);
        let eval = match best_offer(req, candidates) {
            Some((offer, result)) if result.status != MatchStatus::NoMatch => {
                let purchasable = offer.available_quantity.min(req.quantity);
                let subtotal = offer
                    .unit_price_minor
                    .map(|p| p.saturating_mul(purchasable as i64))
                    .unwrap_or(0);
                ItemEvaluation {
                    requested_index: idx,
                    status: result.status,
                    mismatched_fields: result.mismatched_fields,
                    requested_quantity: req.quantity,
                    purchasable_quantity: purchasable,
                    missing_quantity: req.quantity - purchasable,
                    unit_price_minor: offer.unit_price_minor,
                    subtotal_minor: subtotal,
                    offered: Some(offer.clone()),
                }
            }
            other => ItemEvaluation {
                requested_index: idx,
                status: MatchStatus::NoMatch,
                mismatched_fields: other.map(|(_, r)| r.mismatched_fields).unwrap_or_default(),
                requested_quantity: req.quantity,
                purchasable_quantity: 0,
                missing_quantity: req.quantity,
                unit_price_minor: None,
                subtotal_minor: 0,
                offered: None,
            },
        };
        items.push(eval);
    }
    let status = if !items.is_empty() && items.iter().all(|i| i.status == MatchStatus::FullMatch) {
        MatchStatus::FullMatch
    } else if items.iter().all(|i| i.status == MatchStatus::NoMatch) {
        MatchStatus::NoMatch
    } else {
        MatchStatus::PartialMatch
    };
    let total_price_minor = calculate_total(&items);
    PharmacyEvaluation {
        status,
        items,
        total_price_minor,
    }
}

/// Расчёт общей стоимости доступного набора (тыйын).
pub fn calculate_total(items: &[ItemEvaluation]) -> i64 {
    items.iter().fold(0i64, |acc, i| acc.saturating_add(i.subtotal_minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paracetamol(qty: u32) -> MedicationSpec {
        MedicationSpec {
            name: "Парацетамол".into(),
            active_substance: Some("парацетамол".into()),
            dosage: Some("500 мг".into()),
            concentration: None,
            form: Some("таблетки".into()),
            manufacturer: Some("Фармстандарт".into()),
            quantity: qty,
        }
    }

    fn exact_offer(qty: u32, price: i64) -> OfferedItem {
        OfferedItem {
            name: Some("ПАРАЦЕТАМОЛ".into()),
            active_substance: Some("Парацетамол".into()),
            dosage: Some("500мг".into()),
            concentration: None,
            form: Some("Таблетки".into()),
            manufacturer: Some("фармстандарт".into()),
            available_quantity: qty,
            unit_price_minor: Some(price),
        }
    }

    #[test]
    fn full_match_when_all_fields_equal() {
        let r = compare(&paracetamol(10), &exact_offer(20, 500));
        assert_eq!(r.status, MatchStatus::FullMatch);
        assert!(r.mismatched_fields.is_empty());
    }

    #[test]
    fn normalization_handles_case_spaces_and_latin_units() {
        assert_eq!(normalize("  500   MG "), "500 мг");
        assert_eq!(normalize_compact("500 mg"), normalize_compact("500мг"));
        assert_eq!(normalize("Ёлка"), "елка");
    }

    #[test]
    fn partial_match_on_one_mismatch() {
        let mut o = exact_offer(20, 500);
        o.manufacturer = Some("Другой завод".into());
        let r = compare(&paracetamol(10), &o);
        assert_eq!(r.status, MatchStatus::PartialMatch);
        assert_eq!(r.mismatched_fields, vec![MatchField::Manufacturer]);
    }

    #[test]
    fn partial_match_on_two_mismatches() {
        let o = OfferedItem {
            manufacturer: Some("Другой".into()),
            ..exact_offer(5, 500)
        };
        let r = compare(&paracetamol(10), &o);
        assert_eq!(r.status, MatchStatus::PartialMatch);
        assert_eq!(
            r.mismatched_fields,
            vec![MatchField::Manufacturer, MatchField::Quantity]
        );
    }

    #[test]
    fn no_match_on_three_mismatches() {
        let o = OfferedItem {
            dosage: Some("1000 мг".into()),
            form: Some("сироп".into()),
            manufacturer: Some("Другой".into()),
            ..exact_offer(20, 500)
        };
        let r = compare(&paracetamol(10), &o);
        assert_eq!(r.status, MatchStatus::NoMatch);
        assert_eq!(r.mismatched_fields.len(), 3);
    }

    #[test]
    fn insufficient_quantity_is_partial() {
        let r = compare(&paracetamol(10), &exact_offer(3, 500));
        assert_eq!(r.status, MatchStatus::PartialMatch);
        assert_eq!(r.mismatched_fields, vec![MatchField::Quantity]);
    }

    #[test]
    fn zero_quantity_is_no_match() {
        let r = compare(&paracetamol(1), &exact_offer(0, 500));
        assert_eq!(r.status, MatchStatus::NoMatch);
    }

    #[test]
    fn unspecified_fields_are_ignored() {
        // Пользователь указал только название и количество.
        let req = MedicationSpec {
            name: "Нурофен".into(),
            quantity: 1,
            ..Default::default()
        };
        let offer = OfferedItem {
            name: Some("нурофен".into()),
            dosage: Some("400 мг".into()),
            manufacturer: Some("Reckitt".into()),
            available_quantity: 2,
            ..Default::default()
        };
        assert_eq!(compare(&req, &offer).status, MatchStatus::FullMatch);
    }

    #[test]
    fn manual_answer_without_fields_means_same_product() {
        let offer = OfferedItem {
            available_quantity: 10,
            unit_price_minor: Some(1000),
            ..Default::default()
        };
        assert_eq!(compare(&paracetamol(10), &offer).status, MatchStatus::FullMatch);
    }

    #[test]
    fn status_thresholds() {
        assert_eq!(MatchStatus::from_mismatches(0), MatchStatus::FullMatch);
        assert_eq!(MatchStatus::from_mismatches(1), MatchStatus::PartialMatch);
        assert_eq!(MatchStatus::from_mismatches(2), MatchStatus::PartialMatch);
        assert_eq!(MatchStatus::from_mismatches(3), MatchStatus::NoMatch);
        assert_eq!(MatchStatus::from_mismatches(7), MatchStatus::NoMatch);
    }

    #[test]
    fn status_db_roundtrip_and_ordering() {
        for s in [MatchStatus::FullMatch, MatchStatus::PartialMatch, MatchStatus::NoMatch] {
            assert_eq!(MatchStatus::from_db(s.as_db()), Some(s));
        }
        assert!(MatchStatus::FullMatch < MatchStatus::PartialMatch);
        assert!(MatchStatus::PartialMatch < MatchStatus::NoMatch);
    }

    #[test]
    fn pharmacy_full_match_and_total_cost() {
        let req = vec![paracetamol(10), paracetamol(2)];
        let offers = vec![vec![exact_offer(10, 450)], vec![exact_offer(5, 1000)]];
        let e = evaluate_pharmacy(&req, &offers);
        assert_eq!(e.status, MatchStatus::FullMatch);
        // 10 * 4.50 + 2 * 10.00 = 45 + 20 = 65 сом = 6500 тыйын
        assert_eq!(e.total_price_minor, 6500);
        assert!(e.items.iter().all(|i| i.missing_quantity == 0));
    }

    #[test]
    fn pharmacy_partial_counts_only_purchasable() {
        let req = vec![paracetamol(10), paracetamol(2)];
        let offers = vec![vec![exact_offer(4, 500)], vec![]];
        let e = evaluate_pharmacy(&req, &offers);
        assert_eq!(e.status, MatchStatus::PartialMatch);
        assert_eq!(e.items[0].purchasable_quantity, 4);
        assert_eq!(e.items[0].missing_quantity, 6);
        assert_eq!(e.items[1].status, MatchStatus::NoMatch);
        assert_eq!(e.items[1].missing_quantity, 2);
        assert_eq!(e.total_price_minor, 2000);
    }

    #[test]
    fn pharmacy_no_match_when_nothing_available() {
        let req = vec![paracetamol(1)];
        let e = evaluate_pharmacy(&req, &[vec![exact_offer(0, 100)]]);
        assert_eq!(e.status, MatchStatus::NoMatch);
        assert_eq!(e.total_price_minor, 0);
    }

    #[test]
    fn best_offer_prefers_fewer_mismatches_then_cheaper() {
        let req = vec![paracetamol(10)];
        let mut other_brand = exact_offer(10, 100);
        other_brand.manufacturer = Some("Другой".into());
        let offers = vec![vec![other_brand, exact_offer(10, 900), exact_offer(10, 800)]];
        let e = evaluate_pharmacy(&req, &offers);
        assert_eq!(e.status, MatchStatus::FullMatch);
        assert_eq!(e.items[0].unit_price_minor, Some(800));
    }

    #[test]
    fn missing_price_does_not_break_total() {
        let req = vec![paracetamol(1)];
        let mut o = exact_offer(1, 0);
        o.unit_price_minor = None;
        let e = evaluate_pharmacy(&req, &[vec![o]]);
        assert_eq!(e.status, MatchStatus::FullMatch);
        assert_eq!(e.total_price_minor, 0);
    }
}
