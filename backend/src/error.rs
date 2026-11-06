//! Единый тип ошибок API.
//!
//! Все ошибки отдаются клиенту в JSON-формате:
//! `{"error": "<код>", "message": "<описание>", ...}`.
//! Внутренние детали (SQL, стектрейсы) клиенту не передаются — только в лог.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("некорректный запрос: {0}")]
    BadRequest(String),
    #[error("ошибка валидации: {0}")]
    Validation(String),
    #[error("требуется авторизация")]
    Unauthorized,
    #[error("доступ запрещён: {0}")]
    Forbidden(String),
    #[error("не найдено: {0}")]
    NotFound(String),
    #[error("конфликт: {0}")]
    Conflict(String),
    #[error("слишком много запросов")]
    TooManyRequests { retry_after_secs: u64 },
    #[error("сервис недоступен: {0}")]
    ServiceUnavailable(String),
    /// Ошибка поиска: клиенту возвращается search_id и контакт поддержки.
    #[error("ошибка поиска {search_id}")]
    SearchFailed {
        search_id: Uuid,
        support_telegram: String,
        support_url: String,
    },
    #[error("внутренняя ошибка")]
    Internal(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => AppError::NotFound("запись не найдена".into()),
            other => AppError::Internal(anyhow::Error::new(other).context("ошибка базы данных")),
        }
    }
}

impl AppError {
    fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            AppError::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "validation_error"),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            AppError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            AppError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            AppError::TooManyRequests { .. } => (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
            AppError::ServiceUnavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
            AppError::SearchFailed { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "search_failed"),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = self.status_and_code();
        let body = match &self {
            AppError::Internal(err) => {
                // Полная цепочка ошибки — только в лог (request_id попадёт из span).
                tracing::error!(error = ?err, "внутренняя ошибка сервера");
                json!({"error": code, "message": "Внутренняя ошибка сервера"})
            }
            AppError::SearchFailed {
                search_id,
                support_telegram,
                support_url,
            } => {
                tracing::error!(search_id = %search_id, "ошибка поиска возвращена клиенту");
                json!({
                    "error": code,
                    "message": "Не удалось выполнить поиск. Сообщите номер поиска в поддержку.",
                    "search_id": search_id,
                    "support": {"telegram": support_telegram, "url": support_url}
                })
            }
            AppError::TooManyRequests { retry_after_secs } => json!({
                "error": code,
                "message": "Слишком много запросов, повторите позже",
                "retry_after_secs": retry_after_secs
            }),
            other => {
                let message = match other {
                    AppError::BadRequest(m)
                    | AppError::Validation(m)
                    | AppError::Forbidden(m)
                    | AppError::NotFound(m)
                    | AppError::Conflict(m)
                    | AppError::ServiceUnavailable(m) => m.clone(),
                    _ => "Требуется авторизация".to_string(),
                };
                json!({"error": code, "message": message})
            }
        };
        let mut resp = (status, Json(body)).into_response();
        if let AppError::TooManyRequests { retry_after_secs } = self {
            if let Ok(v) = HeaderValue::from_str(&retry_after_secs.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        resp
    }
}
