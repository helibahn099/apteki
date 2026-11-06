//! Уведомления пользователям и аптекам.
//!
//! Сейчас используется `LogNotifier`: он пишет в лог только идентификаторы событий
//! (без персональных данных). В будущем сюда подключаются Telegram/WhatsApp/push.

use std::sync::Mutex;

use async_trait::async_trait;
use serde::Serialize;
use uuid::Uuid;

/// Событие, о котором нужно уведомить участника.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Notification {
    /// Оплата подтверждена, поиск запущен.
    PaymentConfirmed { user_id: Uuid, search_id: Uuid },
    /// Аптека ответила на запрос пользователя.
    PharmacyAnswered {
        user_id: Uuid,
        search_id: Uuid,
        chat_id: Uuid,
    },
    /// Новый запрос для аптеки (ручной режим / панель).
    NewPharmacyRequest { pharmacy_id: Uuid, response_id: Uuid },
    /// Поиск завершён.
    SearchCompleted {
        user_id: Uuid,
        search_id: Uuid,
        found: usize,
    },
    /// Новое сообщение в чате для аптеки.
    NewMessageForPharmacy { pharmacy_id: Uuid, chat_id: Uuid },
    /// Новое сообщение в чате для пользователя.
    NewMessageForUser { user_id: Uuid, chat_id: Uuid },
}

impl Notification {
    pub fn kind(&self) -> &'static str {
        match self {
            Notification::PaymentConfirmed { .. } => "payment_confirmed",
            Notification::PharmacyAnswered { .. } => "pharmacy_answered",
            Notification::NewPharmacyRequest { .. } => "new_pharmacy_request",
            Notification::SearchCompleted { .. } => "search_completed",
            Notification::NewMessageForPharmacy { .. } => "new_message_for_pharmacy",
            Notification::NewMessageForUser { .. } => "new_message_for_user",
        }
    }
}

/// Канал доставки уведомлений. Ошибки доставки не должны ломать бизнес-логику,
/// поэтому метод ничего не возвращает — реализация сама логирует сбои.
#[async_trait]
pub trait Notifier: Send + Sync {
    async fn notify(&self, notification: Notification);
}

/// Заглушка: пишет событие в лог (только ID).
// TODO(notifications): отправка через Telegram-бота (TELEGRAM_BOT_TOKEN) / WhatsApp / push.
pub struct LogNotifier;

#[async_trait]
impl Notifier for LogNotifier {
    async fn notify(&self, n: Notification) {
        // Логируем только тип и идентификаторы — без телефонов и текста сообщений.
        match serde_json::to_string(&n) {
            Ok(payload) => tracing::info!(event = n.kind(), payload = %payload, "уведомление"),
            Err(_) => tracing::info!(event = n.kind(), "уведомление"),
        }
    }
}

/// Уведомитель для тестов: сохраняет все события.
#[derive(Default)]
pub struct RecordingNotifier {
    events: Mutex<Vec<Notification>>,
}

impl RecordingNotifier {
    pub fn events(&self) -> Vec<Notification> {
        self.events.lock().expect("mutex poisoned").clone()
    }
}

#[async_trait]
impl Notifier for RecordingNotifier {
    async fn notify(&self, n: Notification) {
        self.events.lock().expect("mutex poisoned").push(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recording_notifier_stores_events() {
        let r = RecordingNotifier::default();
        let id = Uuid::new_v4();
        r.notify(Notification::NewMessageForUser {
            user_id: id,
            chat_id: id,
        })
        .await;
        assert_eq!(r.events().len(), 1);
        assert_eq!(r.events()[0].kind(), "new_message_for_user");
    }

    #[test]
    fn serialization_has_no_personal_data_fields() {
        let id = Uuid::nil();
        let json = serde_json::to_string(&Notification::SearchCompleted {
            user_id: id,
            search_id: id,
            found: 2,
        })
        .unwrap();
        assert!(json.contains("\"type\":\"search_completed\""));
        assert!(!json.contains("phone"));
    }
}
