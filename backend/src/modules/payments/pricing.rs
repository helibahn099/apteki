//! Расчёт стоимости поиска (KGS). Внутри — тыйыны (1 сом = 100 тыйын).
//!
//! Тариф: базовая цена по типу поиска + доплата за каждый препарат сверх
//! включённых в базовую цену. Все значения — из конфигурации.

use serde::{Deserialize, Serialize};

use crate::config::PricingConfig;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchType {
    Basic,
    Extended,
}

impl SearchType {
    pub fn as_db(&self) -> &'static str {
        match self {
            SearchType::Basic => "basic",
            SearchType::Extended => "extended",
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "basic" => Some(SearchType::Basic),
            "extended" => Some(SearchType::Extended),
            _ => None,
        }
    }
}

pub const MINOR_PER_KGS: i64 = 100;

/// Стоимость поиска в тыйынах.
pub fn search_price_minor(cfg: &PricingConfig, search_type: SearchType, items: usize) -> i64 {
    let base = match search_type {
        SearchType::Basic => cfg.basic_kgs,
        SearchType::Extended => cfg.extended_kgs,
    };
    let extra = items.saturating_sub(cfg.included_items) as i64;
    (base + extra * cfg.per_extra_item_kgs).max(0) * MINOR_PER_KGS
}

/// Форматирование суммы: 12345 тыйын → «123.45».
pub fn format_kgs(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PricingConfig {
        PricingConfig {
            basic_kgs: 50,
            extended_kgs: 150,
            per_extra_item_kgs: 10,
            included_items: 3,
        }
    }

    #[test]
    fn base_price_within_included_items() {
        assert_eq!(search_price_minor(&cfg(), SearchType::Basic, 1), 5000);
        assert_eq!(search_price_minor(&cfg(), SearchType::Basic, 3), 5000);
        assert_eq!(search_price_minor(&cfg(), SearchType::Extended, 2), 15000);
    }

    #[test]
    fn extra_items_are_charged() {
        assert_eq!(search_price_minor(&cfg(), SearchType::Basic, 5), 7000);
        assert_eq!(search_price_minor(&cfg(), SearchType::Extended, 4), 16000);
    }

    #[test]
    fn formats_amounts() {
        assert_eq!(format_kgs(5000), "50.00");
        assert_eq!(format_kgs(12345), "123.45");
        assert_eq!(format_kgs(5), "0.05");
        assert_eq!(format_kgs(-150), "-1.50");
    }

    #[test]
    fn search_type_roundtrip() {
        for t in [SearchType::Basic, SearchType::Extended] {
            assert_eq!(SearchType::from_db(t.as_db()), Some(t));
        }
    }
}
