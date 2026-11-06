//! Статусы поискового заказа и допустимые переходы между ними.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    /// Создан.
    Created,
    /// Ожидает оплаты (выдан QR-код).
    AwaitingPayment,
    /// Оплачен (подтверждение получено от провайдера).
    Paid,
    /// Поиск выполняется.
    SearchInProgress,
    /// Поиск завершён.
    SearchCompleted,
    /// Ошибка обработки.
    ProcessingError,
}

impl OrderStatus {
    pub fn as_db(&self) -> &'static str {
        match self {
            OrderStatus::Created => "created",
            OrderStatus::AwaitingPayment => "awaiting_payment",
            OrderStatus::Paid => "paid",
            OrderStatus::SearchInProgress => "search_in_progress",
            OrderStatus::SearchCompleted => "search_completed",
            OrderStatus::ProcessingError => "processing_error",
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "created" => OrderStatus::Created,
            "awaiting_payment" => OrderStatus::AwaitingPayment,
            "paid" => OrderStatus::Paid,
            "search_in_progress" => OrderStatus::SearchInProgress,
            "search_completed" => OrderStatus::SearchCompleted,
            "processing_error" => OrderStatus::ProcessingError,
            _ => return None,
        })
    }

    /// Разрешён ли переход `self → next`.
    pub fn can_transition_to(&self, next: OrderStatus) -> bool {
        use OrderStatus::*;
        matches!(
            (self, next),
            (Created, AwaitingPayment)
                | (AwaitingPayment, AwaitingPayment) // перевыпуск QR после истечения
                | (Created | AwaitingPayment, Paid)
                | (Paid, SearchInProgress)
                | (SearchInProgress, SearchCompleted)
                | (Created | AwaitingPayment | Paid | SearchInProgress, ProcessingError)
        )
    }

    /// Заказ уже оплачен (или дальше по жизненному циклу).
    pub fn is_paid_or_later(&self) -> bool {
        matches!(
            self,
            OrderStatus::Paid | OrderStatus::SearchInProgress | OrderStatus::SearchCompleted
        )
    }

    pub fn is_final(&self) -> bool {
        matches!(self, OrderStatus::SearchCompleted | OrderStatus::ProcessingError)
    }
}

#[cfg(test)]
mod tests {
    use super::OrderStatus::*;
    use super::*;

    const ALL: [OrderStatus; 6] = [
        Created,
        AwaitingPayment,
        Paid,
        SearchInProgress,
        SearchCompleted,
        ProcessingError,
    ];

    #[test]
    fn db_roundtrip() {
        for s in ALL {
            assert_eq!(OrderStatus::from_db(s.as_db()), Some(s));
        }
        assert_eq!(OrderStatus::from_db("unknown"), None);
    }

    #[test]
    fn happy_path_transitions() {
        assert!(Created.can_transition_to(AwaitingPayment));
        assert!(AwaitingPayment.can_transition_to(Paid));
        assert!(Paid.can_transition_to(SearchInProgress));
        assert!(SearchInProgress.can_transition_to(SearchCompleted));
    }

    #[test]
    fn forbidden_transitions() {
        // Нельзя запустить поиск без оплаты.
        assert!(!Created.can_transition_to(SearchInProgress));
        assert!(!AwaitingPayment.can_transition_to(SearchInProgress));
        // Повторная оплата / повторный запуск поиска.
        assert!(!Paid.can_transition_to(Paid));
        assert!(!SearchInProgress.can_transition_to(SearchInProgress));
        assert!(!SearchCompleted.can_transition_to(Paid));
        // Финальные статусы.
        for s in ALL {
            assert!(!SearchCompleted.can_transition_to(s));
            assert!(!ProcessingError.can_transition_to(s));
        }
    }

    #[test]
    fn paid_flags() {
        assert!(!Created.is_paid_or_later());
        assert!(!AwaitingPayment.is_paid_or_later());
        assert!(Paid.is_paid_or_later());
        assert!(SearchCompleted.is_paid_or_later());
        assert!(SearchCompleted.is_final() && ProcessingError.is_final() && !Paid.is_final());
    }
}
