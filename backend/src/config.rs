//! Конфигурация приложения.
//!
//! Все параметры читаются из переменных окружения (или файла `.env`).
//! В коде не должно быть «магических» констант поиска, тарифов и лимитов —
//! всё задаётся здесь и описано в `.env.example` и README.

use std::env;
use std::str::FromStr;
use std::time::Duration;

use thiserror::Error;

/// Окружение запуска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEnv {
    Development,
    Test,
    Production,
}

impl FromStr for AppEnv {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "development" | "dev" => Ok(Self::Development),
            "test" => Ok(Self::Test),
            "production" | "prod" => Ok(Self::Production),
            other => Err(format!("неизвестное значение APP_ENV: {other}")),
        }
    }
}

/// Формат логов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("обязательная переменная окружения {0} не задана")]
    Missing(&'static str),
    #[error("некорректное значение переменной {name}: {reason}")]
    Invalid { name: &'static str, reason: String },
}

/// Параметры поискового движка.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    /// Стартовый радиус базового поиска (м).
    pub min_radius_m: u32,
    /// Максимальный радиус базового поиска (м).
    pub max_radius_basic_m: u32,
    /// Шаг расширения радиуса (м). Каждый шаг — новая «волна» запросов.
    pub radius_step_m: u32,
    /// Максимум результатов, показываемых пользователю.
    pub max_results: usize,
    /// Жёсткий минимум: если найдено меньше — поиск считается безрезультатным.
    pub hard_min_results: usize,
    /// Мягкий минимум: пока найдено меньше — продолжаем расширять радиус.
    pub soft_min_results: usize,
    /// Пауза между волнами (ожидание ответов аптек перед расширением радиуса).
    pub wave_interval: Duration,
    /// Сколько ждать ответов после последней волны, прежде чем завершить поиск.
    pub response_wait: Duration,
    /// Название города (граница из таблицы `city_boundaries`).
    pub city_name: String,
    /// Максимум препаратов в одном заказе.
    pub max_medications: usize,
}

/// Тарифы (в сомах; внутри системы суммы хранятся в тыйынах: 1 сом = 100 тыйын).
#[derive(Debug, Clone)]
pub struct PricingConfig {
    pub basic_kgs: i64,
    pub extended_kgs: i64,
    pub per_extra_item_kgs: i64,
    pub included_items: usize,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub app_env: AppEnv,
    pub http_host: String,
    pub http_port: u16,

    pub database_url: String,
    pub database_max_connections: u32,
    pub run_migrations: bool,
    pub seed_dev_data: bool,

    pub log_level: String,
    pub log_allow_debug_in_prod: bool,
    pub log_format: LogFormat,

    pub jwt_secret: String,
    pub jwt_access_ttl: Duration,
    pub jwt_refresh_ttl: Duration,

    pub otp_ttl: Duration,
    pub otp_length: usize,
    pub otp_max_attempts: i32,
    pub otp_resend_interval: Duration,
    /// Номера телефонов, которые при входе получают роль администратора системы.
    pub admin_phones: Vec<String>,

    /// Токен Telegram-бота (секрет, только из окружения).
    pub telegram_bot_token: Option<String>,
    pub telegram_bot_enabled: bool,
    /// Тестовый режим: все OTP-коды отправляются в этот chat_id.
    pub telegram_test_chat_id: Option<i64>,

    /// Ключ Google AI Studio (Gemini Flash). Пока не задан — ИИ работает как заглушка.
    pub gemini_api_key: Option<String>,
    pub gemini_model: String,
    /// Через сколько секунд поиска запускать доп. задачу ИИ (по ТЗ — 30 минут).
    pub ai_extra_task_after: Duration,

    pub search: SearchConfig,

    /// PHARMACY_REQUEST_INTERVAL_SECS — будет уменьшаться по мере стабилизации системы.
    /// Минимальный интервал между запросами к одной аптеке (глобально по всем поискам).
    pub pharmacy_request_interval: Duration,
    /// Сколько ждём ответа аптеки, после чего запрос помечается просроченным.
    pub pharmacy_response_ttl: Duration,

    pub pricing: PricingConfig,

    pub mock_payment_enabled: bool,
    pub mock_payment_webhook_secret: String,
    pub payment_qr_ttl: Duration,

    pub chat_active_hours: i64,
    pub chat_message_max_len: usize,

    pub maintenance_interval: Duration,
    pub api_rate_limit_per_minute: u32,
    pub cors_allowed_origins: Vec<String>,

    /// Контакт поддержки, показывается при ошибках поиска.
    pub support_telegram: String,
}

fn var(name: &'static str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn parse_or<T: FromStr>(name: &'static str, default: T) -> Result<T, ConfigError>
where
    T::Err: std::fmt::Display,
{
    match var(name) {
        None => Ok(default),
        Some(v) => v.trim().parse::<T>().map_err(|e| ConfigError::Invalid {
            name,
            reason: e.to_string(),
        }),
    }
}

fn parse_bool(name: &'static str, default: bool) -> Result<bool, ConfigError> {
    match var(name) {
        None => Ok(default),
        Some(v) => match v.trim().to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            other => Err(ConfigError::Invalid {
                name,
                reason: format!("ожидается true/false, получено «{other}»"),
            }),
        },
    }
}

fn secs(name: &'static str, default: u64) -> Result<Duration, ConfigError> {
    parse_or::<u64>(name, default).map(Duration::from_secs)
}

fn list(name: &'static str, default: &str) -> Vec<String> {
    var(name)
        .unwrap_or_else(|| default.to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl Config {
    /// Загружает конфигурацию из окружения и проверяет её согласованность.
    pub fn from_env() -> Result<Self, ConfigError> {
        let app_env = match var("APP_ENV") {
            None => AppEnv::Development,
            Some(v) => v.parse().map_err(|reason| ConfigError::Invalid {
                name: "APP_ENV",
                reason,
            })?,
        };

        let log_format = match var("LOG_FORMAT").as_deref().map(str::to_lowercase).as_deref() {
            None | Some("json") => LogFormat::Json,
            Some("pretty") => LogFormat::Pretty,
            Some(other) => {
                return Err(ConfigError::Invalid {
                    name: "LOG_FORMAT",
                    reason: format!("ожидается json или pretty, получено «{other}»"),
                })
            }
        };

        let telegram_test_chat_id = match var("TELEGRAM_TEST_CHAT_ID") {
            None => None,
            Some(v) => Some(v.trim().parse::<i64>().map_err(|e| ConfigError::Invalid {
                name: "TELEGRAM_TEST_CHAT_ID",
                reason: e.to_string(),
            })?),
        };

        let cfg = Config {
            app_env,
            http_host: var("HTTP_HOST").unwrap_or_else(|| "0.0.0.0".into()),
            http_port: parse_or("HTTP_PORT", 8080)?,
            database_url: var("DATABASE_URL").ok_or(ConfigError::Missing("DATABASE_URL"))?,
            database_max_connections: parse_or("DATABASE_MAX_CONNECTIONS", 10)?,
            run_migrations: parse_bool("RUN_MIGRATIONS", true)?,
            seed_dev_data: parse_bool("SEED_DEV_DATA", false)?,
            log_level: var("LOG_LEVEL").unwrap_or_else(|| "info".into()),
            log_allow_debug_in_prod: parse_bool("LOG_ALLOW_DEBUG_IN_PROD", false)?,
            log_format,
            jwt_secret: var("JWT_SECRET").ok_or(ConfigError::Missing("JWT_SECRET"))?,
            jwt_access_ttl: secs("JWT_ACCESS_TTL_SECS", 900)?,
            jwt_refresh_ttl: secs("JWT_REFRESH_TTL_SECS", 30 * 24 * 3600)?,
            otp_ttl: secs("OTP_TTL_SECS", 300)?,
            otp_length: parse_or("OTP_LENGTH", 6)?,
            otp_max_attempts: parse_or("OTP_MAX_ATTEMPTS", 5)?,
            otp_resend_interval: secs("OTP_RESEND_INTERVAL_SECS", 60)?,
            admin_phones: list("ADMIN_PHONES", ""),
            telegram_bot_token: var("TELEGRAM_BOT_TOKEN"),
            telegram_bot_enabled: parse_bool("TELEGRAM_BOT_ENABLED", false)?,
            telegram_test_chat_id,
            gemini_api_key: var("GEMINI_API_KEY"),
            gemini_model: var("GEMINI_MODEL").unwrap_or_else(|| "gemini-flash-latest".into()),
            ai_extra_task_after: secs("AI_EXTRA_TASK_AFTER_SECS", 1800)?,
            search: SearchConfig {
                min_radius_m: parse_or("SEARCH_MIN_RADIUS_METERS", 100)?,
                max_radius_basic_m: parse_or("SEARCH_MAX_RADIUS_BASIC_METERS", 4000)?,
                radius_step_m: parse_or("SEARCH_RADIUS_STEP_METERS", 500)?,
                max_results: parse_or("SEARCH_MAX_RESULTS", 15)?,
                hard_min_results: parse_or("SEARCH_HARD_MIN_RESULTS", 1)?,
                soft_min_results: parse_or("SEARCH_SOFT_MIN_RESULTS", 3)?,
                wave_interval: secs("SEARCH_WAVE_INTERVAL_SECS", 60)?,
                response_wait: secs("SEARCH_RESPONSE_WAIT_SECS", 600)?,
                city_name: var("SEARCH_CITY_NAME").unwrap_or_else(|| "Бишкек".into()),
                max_medications: parse_or("SEARCH_MAX_MEDICATIONS", 20)?,
            },
            // PHARMACY_REQUEST_INTERVAL_SECS — будет уменьшаться по мере стабилизации системы.
            pharmacy_request_interval: secs("PHARMACY_REQUEST_INTERVAL_SECS", 60)?,
            pharmacy_response_ttl: secs("PHARMACY_RESPONSE_TTL_SECS", 1800)?,
            pricing: PricingConfig {
                basic_kgs: parse_or("SEARCH_PRICE_BASIC_KGS", 50)?,
                extended_kgs: parse_or("SEARCH_PRICE_EXTENDED_KGS", 150)?,
                per_extra_item_kgs: parse_or("SEARCH_PRICE_PER_EXTRA_ITEM_KGS", 10)?,
                included_items: parse_or("SEARCH_PRICE_INCLUDED_ITEMS", 3)?,
            },
            mock_payment_enabled: parse_bool("MOCK_PAYMENT_ENABLED", true)?,
            mock_payment_webhook_secret: var("MOCK_PAYMENT_WEBHOOK_SECRET")
                .unwrap_or_else(|| "mock-webhook-secret-change-me".into()),
            payment_qr_ttl: secs("PAYMENT_QR_TTL_SECS", 900)?,
            chat_active_hours: parse_or("CHAT_ACTIVE_HOURS", 48)?,
            chat_message_max_len: parse_or("CHAT_MESSAGE_MAX_LEN", 2000)?,
            maintenance_interval: secs("MAINTENANCE_INTERVAL_SECS", 60)?,
            api_rate_limit_per_minute: parse_or("API_RATE_LIMIT_PER_MINUTE", 120)?,
            cors_allowed_origins: list("CORS_ALLOWED_ORIGINS", "http://localhost:3000"),
            support_telegram: var("SUPPORT_TELEGRAM").unwrap_or_else(|| "@helibahn".into()),
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Проверка согласованности параметров.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let s = &self.search;
        let invalid = |name: &'static str, reason: &str| {
            Err(ConfigError::Invalid {
                name,
                reason: reason.to_string(),
            })
        };
        if s.min_radius_m == 0 {
            return invalid("SEARCH_MIN_RADIUS_METERS", "должен быть больше 0");
        }
        if s.radius_step_m == 0 {
            return invalid("SEARCH_RADIUS_STEP_METERS", "должен быть больше 0");
        }
        if s.min_radius_m > s.max_radius_basic_m {
            return invalid(
                "SEARCH_MAX_RADIUS_BASIC_METERS",
                "не может быть меньше SEARCH_MIN_RADIUS_METERS",
            );
        }
        if s.hard_min_results == 0 {
            return invalid("SEARCH_HARD_MIN_RESULTS", "должен быть не меньше 1");
        }
        if !(s.hard_min_results <= s.soft_min_results && s.soft_min_results <= s.max_results) {
            return invalid(
                "SEARCH_SOFT_MIN_RESULTS",
                "требуется HARD_MIN <= SOFT_MIN <= MAX_RESULTS",
            );
        }
        if !(4..=10).contains(&self.otp_length) {
            return invalid("OTP_LENGTH", "допустимо от 4 до 10 цифр");
        }
        if self.app_env == AppEnv::Production && self.jwt_secret.len() < 32 {
            return invalid("JWT_SECRET", "в production секрет должен быть не короче 32 символов");
        }
        if self.chat_active_hours <= 0 {
            return invalid("CHAT_ACTIVE_HOURS", "должен быть больше 0");
        }
        Ok(())
    }

    pub fn is_production(&self) -> bool {
        self.app_env == AppEnv::Production
    }

    /// Ссылка на поддержку в Telegram (из «@helibahn» → «https://t.me/helibahn»).
    pub fn support_url(&self) -> String {
        format!("https://t.me/{}", self.support_telegram.trim_start_matches('@'))
    }

    /// Конфигурация для тестов: короткие интервалы, без ожиданий.
    pub fn for_tests(database_url: &str) -> Self {
        Config {
            app_env: AppEnv::Test,
            http_host: "127.0.0.1".into(),
            http_port: 0,
            database_url: database_url.to_string(),
            database_max_connections: 5,
            run_migrations: false,
            seed_dev_data: false,
            log_level: "warn".into(),
            log_allow_debug_in_prod: false,
            log_format: LogFormat::Pretty,
            jwt_secret: "test-secret-test-secret-test-secret-123".into(),
            jwt_access_ttl: Duration::from_secs(900),
            jwt_refresh_ttl: Duration::from_secs(3600),
            otp_ttl: Duration::from_secs(300),
            otp_length: 6,
            otp_max_attempts: 3,
            otp_resend_interval: Duration::from_secs(0),
            admin_phones: vec!["+996700000001".into()],
            telegram_bot_token: None,
            telegram_bot_enabled: false,
            telegram_test_chat_id: None,
            gemini_api_key: None,
            gemini_model: "gemini-flash-latest".into(),
            ai_extra_task_after: Duration::from_secs(1800),
            search: SearchConfig {
                min_radius_m: 100,
                max_radius_basic_m: 4000,
                radius_step_m: 500,
                max_results: 15,
                hard_min_results: 1,
                soft_min_results: 3,
                wave_interval: Duration::ZERO,
                response_wait: Duration::ZERO,
                city_name: "Бишкек".into(),
                max_medications: 20,
            },
            pharmacy_request_interval: Duration::ZERO,
            pharmacy_response_ttl: Duration::from_secs(1800),
            pricing: PricingConfig {
                basic_kgs: 50,
                extended_kgs: 150,
                per_extra_item_kgs: 10,
                included_items: 3,
            },
            mock_payment_enabled: true,
            mock_payment_webhook_secret: "test-webhook-secret".into(),
            payment_qr_ttl: Duration::from_secs(900),
            chat_active_hours: 48,
            chat_message_max_len: 2000,
            maintenance_interval: Duration::from_secs(60),
            api_rate_limit_per_minute: 10_000,
            cors_allowed_origins: vec!["http://localhost:3000".into()],
            support_telegram: "@helibahn".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_is_valid() {
        assert!(Config::for_tests("postgres://x").validate().is_ok());
    }

    #[test]
    fn rejects_inconsistent_minimums() {
        let mut c = Config::for_tests("postgres://x");
        c.search.soft_min_results = 20;
        assert!(c.validate().is_err());
        let mut c = Config::for_tests("postgres://x");
        c.search.hard_min_results = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_bad_radius() {
        let mut c = Config::for_tests("postgres://x");
        c.search.min_radius_m = 5000;
        assert!(c.validate().is_err());
        let mut c = Config::for_tests("postgres://x");
        c.search.radius_step_m = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn short_jwt_secret_rejected_in_production() {
        let mut c = Config::for_tests("postgres://x");
        c.app_env = AppEnv::Production;
        c.jwt_secret = "short".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn support_url_built_from_handle() {
        let c = Config::for_tests("postgres://x");
        assert_eq!(c.support_url(), "https://t.me/helibahn");
    }
}
