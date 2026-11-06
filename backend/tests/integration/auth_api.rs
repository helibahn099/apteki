//! Аутентификация: OTP, JWT, refresh, logout, права доступа.

use axum::http::{Method, StatusCode};
use serde_json::json;

use crate::app_or_skip;

#[tokio::test]
async fn health_and_request_id() {
    let app = app_or_skip!();
    let r = app.request(Method::GET, "/health", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["status"], "ok");
    let r = app.request(Method::GET, "/health/ready", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    app.cleanup().await;
}

#[tokio::test]
async fn otp_login_refresh_logout() {
    let app = app_or_skip!();
    let phone = "+996555123456";
    let (access, refresh) = app.login(phone).await;

    let me = app.get("/api/v1/users/me", &access).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["phone"], phone);
    assert_eq!(me.body["role"], "user");

    // Ротация refresh-токена: старый становится недействительным.
    let r = app
        .request(
            Method::POST,
            "/api/v1/auth/refresh",
            None,
            Some(json!({"refresh_token": refresh})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let new_access = r.body["access_token"].as_str().unwrap().to_string();
    let r2 = app
        .request(
            Method::POST,
            "/api/v1/auth/refresh",
            None,
            Some(json!({"refresh_token": refresh})),
        )
        .await;
    assert_eq!(r2.status, StatusCode::UNAUTHORIZED);

    // Logout отзывает сессию — токен перестаёт работать сразу.
    let r = app
        .request(Method::POST, "/api/v1/auth/logout", Some(&new_access), None)
        .await;
    assert!(r.status.is_success(), "{:?}", r.body);
    let r = app.get("/api/v1/users/me", &new_access).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    app.cleanup().await;
}

#[tokio::test]
async fn wrong_otp_is_rejected_and_attempts_limited() {
    let app = app_or_skip!();
    let phone = "+996555000111";
    let r = app
        .request(
            Method::POST,
            "/api/v1/auth/otp/request",
            None,
            Some(json!({"phone": phone, "channel": "telegram"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let real = app.otp.last_code(phone).unwrap();
    let wrong = if real == "000000" { "111111" } else { "000000" };
    for _ in 0..3 {
        let r = app
            .request(
                Method::POST,
                "/api/v1/auth/otp/verify",
                None,
                Some(json!({"phone": phone, "code": wrong})),
            )
            .await;
        assert!(r.status.is_client_error());
    }
    // После исчерпания попыток не принимается даже верный код.
    let r = app
        .request(
            Method::POST,
            "/api/v1/auth/otp/verify",
            None,
            Some(json!({"phone": phone, "code": real})),
        )
        .await;
    assert!(r.status.is_client_error(), "{:?}", r.body);
    app.cleanup().await;
}

#[tokio::test]
async fn invalid_phone_and_missing_token() {
    let app = app_or_skip!();
    let r = app
        .request(
            Method::POST,
            "/api/v1/auth/otp/request",
            None,
            Some(json!({"phone": "123", "channel": "telegram"})),
        )
        .await;
    assert!(r.status.is_client_error());
    let r = app.request(Method::GET, "/api/v1/users/me", None, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = app.get("/api/v1/users/me", "garbage").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    app.cleanup().await;
}

#[tokio::test]
async fn admin_endpoints_require_admin_role() {
    let app = app_or_skip!();
    let (user, _) = app.login("+996555777888").await;
    let r = app
        .post("/api/v1/admin/pharmacies", &user, json!({"name": "Тест"}))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = app.get("/api/v1/pharmacy-panel/requests", &user).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    let (admin, _) = app.login(crate::common::ADMIN_PHONE).await;
    let r = app
        .post("/api/v1/admin/pharmacies", &admin, json!({"name": "Новая аптека"}))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    app.cleanup().await;
}
