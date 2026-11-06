//! Модуль Logging: структурированные JSON-логи (tracing), request_id для
//! каждого HTTP-запроса, search_id для поисковых событий.
//!
//! Правила безопасности логов (обязательны для всего кода):
//! * НЕ логировать OTP-коды (исключение — утверждённая DEBUG-заглушка вне production),
//!   пароли, JWT и refresh-токены, платёжные секреты и подписи;
//! * НЕ логировать номера телефонов целиком — только `mask_phone`, а лучше user_id;
//! * НЕ логировать тексты сообщений чатов и фото рецептов;
//! * для диагностики использовать внутренние идентификаторы (user_id, search_id).

use axum::http::{HeaderName, Request};
use tracing::Span;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::config::{Config, LogFormat};

/// Заголовок с идентификатором запроса (принимается от клиента или генерируется).
pub static REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Итоговый фильтр логов с учётом запрета DEBUG в production.
pub fn effective_log_filter(config: &Config) -> String {
    let level = config.log_level.to_lowercase();
    let wants_verbose = level.contains("debug") || level.contains("trace");
    if config.is_production() && wants_verbose && !config.log_allow_debug_in_prod {
        // В production DEBUG выключен; включается только явно через LOG_ALLOW_DEBUG_IN_PROD=true.
        "info".to_string()
    } else {
        level
    }
}

/// Инициализация глобального подписчика tracing.
pub fn init(config: &Config) {
    let filter_str = effective_log_filter(config);
    let filter = EnvFilter::try_new(&filter_str).unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    let result = match config.log_format {
        LogFormat::Json => registry
            .with(
                fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_span_list(true)
                    .with_target(true),
            )
            .try_init(),
        LogFormat::Pretty => registry.with(fmt::layer().with_target(false)).try_init(),
    };
    if result.is_ok() && filter_str != config.log_level.to_lowercase() {
        tracing::warn!("DEBUG-логи запрещены в production: уровень понижен до INFO");
    }
}

/// Создаёт span HTTP-запроса. Путь логируется без query-строки.
pub fn make_request_span<B>(req: &Request<B>) -> Span {
    let request_id = req
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    tracing::info_span!(
        "http_request",
        request_id = %request_id,
        method = %req.method(),
        path = %req.uri().path(),
        user_id = tracing::field::Empty,
    )
}

/// Маскирование номера телефона: `+996555123456` → `+996*****3456`.
pub fn mask_phone(phone: &str) -> String {
    let chars: Vec<char> = phone.chars().collect();
    if chars.len() <= 8 {
        return "***".into();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}{}{tail}", "*".repeat(chars.len() - 8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppEnv;

    #[test]
    fn masks_phone_numbers() {
        assert_eq!(mask_phone("+996555123456"), "+996*****3456");
        assert_eq!(mask_phone("+1234"), "***");
        assert!(!mask_phone("+996555123456").contains("555123"));
    }

    #[test]
    fn debug_disabled_in_production_by_default() {
        let mut c = Config::for_tests("postgres://x");
        c.app_env = AppEnv::Production;
        c.log_level = "debug".into();
        assert_eq!(effective_log_filter(&c), "info");
        c.log_allow_debug_in_prod = true;
        assert_eq!(effective_log_filter(&c), "debug");
    }

    #[test]
    fn debug_allowed_in_development() {
        let mut c = Config::for_tests("postgres://x");
        c.app_env = AppEnv::Development;
        c.log_level = "savvy_meds=debug,info".into();
        assert_eq!(effective_log_filter(&c), "savvy_meds=debug,info");
    }
}
