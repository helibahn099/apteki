//! Доставка одноразовых кодов (OTP).
//!
//! Абстракция `OtpSender` позволяет подключать каналы (Telegram / WhatsApp / SMS)
//! без изменения логики модуля Auth.
//!
//! Текущее состояние:
//! * `DebugLogOtpSender` — утверждённая временная заглушка: код пишется в лог
//!   уровня DEBUG. В production отключена (код не логируется никогда).
//! * `TelegramTestOtpSender` — тестовая отправка через Telegram-бота: все коды
//!   приходят в один заранее заданный chat_id (`TELEGRAM_TEST_CHAT_ID`).
//!   Бот не может написать пользователю по номеру телефона — нужен chat_id,
//!   который появляется только после того, как человек нажал /start в боте.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::modules::logging::mask_phone;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OtpChannel {
    Telegram,
    Whatsapp,
    Sms,
}

impl OtpChannel {
    pub const ALL: [OtpChannel; 3] = [OtpChannel::Telegram, OtpChannel::Whatsapp, OtpChannel::Sms];

    pub fn as_str(&self) -> &'static str {
        match self {
            OtpChannel::Telegram => "telegram",
            OtpChannel::Whatsapp => "whatsapp",
            OtpChannel::Sms => "sms",
        }
    }
}

#[async_trait]
pub trait OtpSender: Send + Sync {
    /// Как фактически доставляется код через данный канал (для клиента и диагностики).
    fn delivery_mode(&self, channel: OtpChannel) -> Option<&'static str>;

    fn supports(&self, channel: OtpChannel) -> bool {
        self.delivery_mode(channel).is_some()
    }

    async fn send(&self, phone: &str, code: &str, channel: OtpChannel) -> anyhow::Result<()>;
}

/// Заглушка: вывод кода в DEBUG-лог. Работает только вне production.
pub struct DebugLogOtpSender;

#[async_trait]
impl OtpSender for DebugLogOtpSender {
    fn delivery_mode(&self, _channel: OtpChannel) -> Option<&'static str> {
        Some("debug_log")
    }

    async fn send(&self, phone: &str, code: &str, channel: OtpChannel) -> anyhow::Result<()> {
        // ВНИМАНИЕ: единственное место, где OTP попадает в лог. Утверждённая заглушка
        // для разработки; в production этот отправитель не создаётся (см. build_otp_sender),
        // а уровень DEBUG там отключён.
        tracing::debug!(
            otp_code = %code,
            phone = %mask_phone(phone),
            channel = channel.as_str(),
            "OTP-заглушка: код подтверждения (только для разработки)"
        );

        // TODO(Telegram): production-отправка пользователю через бота
        //   (нужна привязка телефона к chat_id через /start + «Поделиться контактом»
        //   либо Telegram Gateway API для кодов подтверждения).
        // TODO(WhatsApp): WhatsApp Business Cloud API, шаблон authentication.
        //   whatsapp_client.send_template(phone, "otp_code", &[code]).await?;
        // TODO(SMS): подключить SMS-шлюз (например, местного оператора).
        //   sms_gateway.send(phone, &format!("Код: {code}")).await?;
        Ok(())
    }
}

/// Тестовая отправка через Telegram: все коды уходят в один chat_id.
pub struct TelegramTestOtpSender {
    client: reqwest::Client,
    bot_token: String,
    chat_id: i64,
}

impl TelegramTestOtpSender {
    pub fn new(bot_token: String, chat_id: i64) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("не удалось создать HTTP-клиент");
        Self {
            client,
            bot_token,
            chat_id,
        }
    }
}

#[async_trait]
impl OtpSender for TelegramTestOtpSender {
    fn delivery_mode(&self, channel: OtpChannel) -> Option<&'static str> {
        (channel == OtpChannel::Telegram).then_some("telegram_test_chat")
    }

    async fn send(&self, phone: &str, code: &str, _channel: OtpChannel) -> anyhow::Result<()> {
        let text = format!(
            "Код подтверждения Savvy Meds: {code}\nНомер: {}\nНикому не сообщайте этот код.",
            mask_phone(phone)
        );
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let resp = self
            .client
            .post(url)
            .json(&serde_json::json!({"chat_id": self.chat_id, "text": text}))
            .send()
            .await
            // without_url(): URL содержит токен бота — в лог он попадать не должен.
            .map_err(|e| anyhow::anyhow!("ошибка запроса к Telegram: {}", e.without_url()))?;
        let status = resp.status();
        if !status.is_success() {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            let description = body
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("нет описания");
            anyhow::bail!("Telegram вернул {status}: {description}");
        }
        tracing::info!(phone = %mask_phone(phone), "OTP отправлен в тестовый Telegram-чат");
        Ok(())
    }
}

/// Маршрутизация по каналам: Telegram (если включён) + заглушка для остальных
/// каналов вне production.
pub struct RoutingOtpSender {
    telegram: Option<TelegramTestOtpSender>,
    debug_fallback: Option<DebugLogOtpSender>,
}

#[async_trait]
impl OtpSender for RoutingOtpSender {
    fn delivery_mode(&self, channel: OtpChannel) -> Option<&'static str> {
        if let Some(tg) = &self.telegram {
            if let Some(mode) = tg.delivery_mode(channel) {
                return Some(mode);
            }
        }
        self.debug_fallback.as_ref().and_then(|d| d.delivery_mode(channel))
    }

    async fn send(&self, phone: &str, code: &str, channel: OtpChannel) -> anyhow::Result<()> {
        if let Some(tg) = &self.telegram {
            if tg.supports(channel) {
                return tg.send(phone, code, channel).await;
            }
        }
        match &self.debug_fallback {
            Some(d) => d.send(phone, code, channel).await,
            None => anyhow::bail!("канал {} пока не подключён", channel.as_str()),
        }
    }
}

/// Собирает отправителя кодов по конфигурации.
pub fn build_otp_sender(config: &Config) -> Arc<dyn OtpSender> {
    let telegram = match (
        config.telegram_bot_enabled,
        &config.telegram_bot_token,
        config.telegram_test_chat_id,
    ) {
        (true, Some(token), Some(chat_id)) => Some(TelegramTestOtpSender::new(token.clone(), chat_id)),
        (true, _, _) => {
            tracing::warn!(
                "TELEGRAM_BOT_ENABLED=true, но не заданы TELEGRAM_BOT_TOKEN или TELEGRAM_TEST_CHAT_ID — Telegram отключён"
            );
            None
        }
        _ => None,
    };
    let debug_fallback = (!config.is_production()).then_some(DebugLogOtpSender);
    if telegram.is_none() && debug_fallback.is_none() {
        tracing::warn!("ни один канал доставки OTP не настроен — вход по телефону недоступен");
    }
    Arc::new(RoutingOtpSender {
        telegram,
        debug_fallback,
    })
}

/// Отправитель для тестов: запоминает последний код по номеру телефона.
#[derive(Default)]
pub struct CapturingOtpSender {
    codes: Mutex<HashMap<String, String>>,
}

impl CapturingOtpSender {
    pub fn last_code(&self, phone: &str) -> Option<String> {
        self.codes.lock().ok()?.get(phone).cloned()
    }
}

#[async_trait]
impl OtpSender for CapturingOtpSender {
    fn delivery_mode(&self, _channel: OtpChannel) -> Option<&'static str> {
        Some("test_capture")
    }

    async fn send(&self, phone: &str, code: &str, _channel: OtpChannel) -> anyhow::Result<()> {
        if let Ok(mut m) = self.codes.lock() {
            m.insert(phone.to_string(), code.to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_has_no_debug_fallback() {
        let mut cfg = Config::for_tests("postgres://x");
        cfg.app_env = crate::config::AppEnv::Production;
        let sender = build_otp_sender(&cfg);
        assert!(!sender.supports(OtpChannel::Sms));
        assert!(!sender.supports(OtpChannel::Telegram));
    }

    #[test]
    fn development_uses_debug_log_for_all_channels() {
        let cfg = Config::for_tests("postgres://x");
        let sender = build_otp_sender(&cfg);
        for ch in OtpChannel::ALL {
            assert_eq!(sender.delivery_mode(ch), Some("debug_log"));
        }
    }

    #[test]
    fn telegram_routes_only_telegram_channel() {
        let mut cfg = Config::for_tests("postgres://x");
        cfg.telegram_bot_enabled = true;
        cfg.telegram_bot_token = Some("123:abc".into());
        cfg.telegram_test_chat_id = Some(42);
        let sender = build_otp_sender(&cfg);
        assert_eq!(sender.delivery_mode(OtpChannel::Telegram), Some("telegram_test_chat"));
        assert_eq!(sender.delivery_mode(OtpChannel::Sms), Some("debug_log"));
    }

    #[tokio::test]
    async fn capturing_sender_stores_code() {
        let s = CapturingOtpSender::default();
        s.send("+996555000111", "123456", OtpChannel::Sms).await.unwrap();
        assert_eq!(s.last_code("+996555000111").as_deref(), Some("123456"));
    }
}
