//! Сборка HTTP-приложения: маршруты и middleware.

use std::net::SocketAddr;
use std::time::Duration;

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use crate::error::AppError;
use crate::modules::{self, logging};
use crate::state::AppState;

/// Максимальный размер тела запроса.
const MAX_BODY_BYTES: usize = 256 * 1024;
/// Таймаут обработки HTTP-запроса.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Полный роутер приложения. Все API — под префиксом `/api/v1`.
pub fn build_router(state: AppState) -> Router {
    let api = Router::new()
        .merge(modules::auth::router())
        .merge(modules::users::router())
        .merge(modules::medication::router())
        .merge(modules::search::router())
        .merge(modules::payments::router())
        .merge(modules::chat::router())
        .merge(modules::pharmacy::router())
        .merge(modules::ai::router())
        .route_layer(middleware::from_fn_with_state(state.clone(), api_rate_limit));

    let cors = build_cors(&state.config.cors_allowed_origins);

    Router::new()
        .route("/health", get(health))
        .route("/health/ready", get(ready))
        .nest("/api/v1", api)
        .fallback(not_found)
        .with_state(state)
        // Слои применяются снизу вверх: последний — самый внешний.
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(cors)
        .layer(PropagateRequestIdLayer::new(logging::REQUEST_ID_HEADER.clone()))
        .layer(TraceLayer::new_for_http().make_span_with(logging::make_request_span))
        .layer(SetRequestIdLayer::new(
            logging::REQUEST_ID_HEADER.clone(),
            MakeRequestUuid,
        ))
        .layer(middleware::from_fn(sanitize_request_id))
}

fn build_cors(origins: &[String]) -> CorsLayer {
    let base = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::PATCH, Method::DELETE])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            logging::REQUEST_ID_HEADER.clone(),
        ])
        .expose_headers([logging::REQUEST_ID_HEADER.clone()])
        .max_age(Duration::from_secs(3600));
    let parsed: Vec<HeaderValue> = origins.iter().filter_map(|o| o.parse().ok()).collect();
    if parsed.is_empty() {
        // Без списка разрешённых источников браузерные кросс-доменные запросы запрещены.
        base
    } else {
        base.allow_origin(AllowOrigin::list(parsed))
    }
}

/// Отбрасывает некорректный `x-request-id` от клиента (защита логов от инъекций);
/// в этом случае будет сгенерирован новый UUID.
async fn sanitize_request_id(mut req: Request, next: Next) -> Response {
    let invalid = req
        .headers()
        .get(&logging::REQUEST_ID_HEADER)
        .map(|v| !is_valid_request_id(v.as_bytes()))
        .unwrap_or(false);
    if invalid {
        req.headers_mut().remove(&logging::REQUEST_ID_HEADER);
    }
    next.run(req).await
}

pub fn is_valid_request_id(v: &[u8]) -> bool {
    !v.is_empty() && v.len() <= 64 && v.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
}

/// Пути, не подпадающие под лимит по IP (вебхуки платёжного провайдера).
fn rate_limit_exempt(path: &str) -> bool {
    path.starts_with("/payments/webhook/") || path.starts_with("/api/v1/payments/webhook/")
}

/// Ограничение частоты запросов к API по IP-адресу клиента.
/// Примечание: за обратным прокси (nginx) нужно передавать реальный IP — тогда ключом
/// станет адрес прокси; это будет настроено вместе с TLS.
async fn api_rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if rate_limit_exempt(req.uri().path()) {
        return next.run(req).await;
    }
    let key = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    match state.api_limiter.check(&key) {
        Ok(()) => next.run(req).await,
        Err(retry_after_secs) => {
            tracing::warn!("превышен лимит запросов к API");
            AppError::TooManyRequests { retry_after_secs }.into_response()
        }
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

/// Готовность: проверяет доступность БД.
async fn ready(State(state): State<AppState>) -> Response {
    match sqlx::query("SELECT 1").execute(&state.db).await {
        Ok(_) => Json(json!({ "status": "ready" })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "БД недоступна");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "db_unavailable" })),
            )
                .into_response()
        }
    }
}

async fn not_found() -> AppError {
    AppError::NotFound("маршрут не найден".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_validation() {
        assert!(is_valid_request_id(b"abc-123_DEF"));
        assert!(!is_valid_request_id(b""));
        assert!(!is_valid_request_id(b"a b"));
        assert!(!is_valid_request_id(b"x\ny"));
        assert!(!is_valid_request_id(&[b'a'; 65]));
    }

    #[test]
    fn webhooks_are_exempt_from_ip_limit() {
        assert!(rate_limit_exempt("/payments/webhook/mock"));
        assert!(!rate_limit_exempt("/search/orders"));
    }
}
