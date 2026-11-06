//! Постепенное расширение радиуса поиска.
//!
//! Базовый поиск: от `min` до `max` с шагом `step`; последний шаг всегда
//! равен `max`. Пример (100, 4000, 500): 100, 600, 1100, …, 3600, 4000.
//! Каждый шаг — новая «волна» запросов к аптекам, которые ещё не опрошены.

use super::super::payments::pricing::SearchType;
use crate::config::SearchConfig;

/// Радиус волны. `None` — «весь город» (финальная волна расширенного поиска).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wave {
    Radius(u32),
    WholeCity,
}

/// Последовательность радиусов от `min` до `max` включительно.
pub fn radius_steps(min: u32, max: u32, step: u32) -> Vec<u32> {
    if min == 0 || step == 0 || min > max {
        return Vec::new();
    }
    let mut steps = Vec::new();
    let mut r = min;
    while r < max {
        steps.push(r);
        r = r.saturating_add(step);
    }
    steps.push(max);
    steps
}

/// План волн для типа поиска.
pub fn wave_plan(cfg: &SearchConfig, search_type: SearchType) -> Vec<Wave> {
    let mut plan: Vec<Wave> = radius_steps(cfg.min_radius_m, cfg.max_radius_basic_m, cfg.radius_step_m)
        .into_iter()
        .map(Wave::Radius)
        .collect();
    if search_type == SearchType::Extended {
        plan.push(Wave::WholeCity);
    }
    plan
}

/// Нужно ли расширять радиус дальше: найдено меньше мягкого минимума
/// и ещё не набран максимум результатов.
pub fn should_expand(positive_results: usize, cfg: &SearchConfig) -> bool {
    positive_results < cfg.soft_min_results && positive_results < cfg.max_results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn default_steps() {
        let s = radius_steps(100, 4000, 500);
        assert_eq!(s.first(), Some(&100));
        assert_eq!(s.last(), Some(&4000));
        assert_eq!(s, vec![100, 600, 1100, 1600, 2100, 2600, 3100, 3600, 4000]);
    }

    #[test]
    fn exact_multiple_has_no_duplicate() {
        assert_eq!(radius_steps(500, 1500, 500), vec![500, 1000, 1500]);
    }

    #[test]
    fn degenerate_inputs() {
        assert_eq!(radius_steps(100, 100, 500), vec![100]);
        assert!(radius_steps(0, 100, 10).is_empty());
        assert!(radius_steps(100, 50, 10).is_empty());
        assert!(radius_steps(100, 1000, 0).is_empty());
    }

    #[test]
    fn extended_plan_ends_with_whole_city() {
        let cfg = Config::for_tests("x").search;
        let basic = wave_plan(&cfg, SearchType::Basic);
        let ext = wave_plan(&cfg, SearchType::Extended);
        assert_eq!(basic.last(), Some(&Wave::Radius(4000)));
        assert_eq!(ext.last(), Some(&Wave::WholeCity));
        assert_eq!(ext.len(), basic.len() + 1);
    }

    #[test]
    fn expansion_stops_at_soft_min() {
        let cfg = Config::for_tests("x").search; // soft_min = 3
        assert!(should_expand(0, &cfg));
        assert!(should_expand(2, &cfg));
        assert!(!should_expand(3, &cfg));
        assert!(!should_expand(15, &cfg));
    }
}
