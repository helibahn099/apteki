//! Заказы поиска: precheck, создание, валидация, чёрный список, изоляция пользователей.

use axum::http::StatusCode;
use serde_json::json;

use crate::app_or_skip;
use crate::common::{paracetamol, CENTER};

#[tokio::test]
async fn precheck_counts_pharmacies_and_price() {
    let app = app_or_skip!();
    let (t, _) = app.login("+996555100001").await;
    let r = app
        .post(
            "/api/v1/search/precheck",
            &t,
            json!({"search_type": "basic", "location": {"lat": CENTER.0, "lon": CENTER.1}, "medications_count": 1}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert!(r.body["pharmacy_count"].as_i64().unwrap() > 0);
    assert_eq!(r.body["radius_m"], 4000);
    assert_eq!(r.body["currency"], "KGS");
    assert_eq!(r.body["price_minor"], 5000);

    // Расширенный поиск — весь город.
    let r = app
        .post(
            "/api/v1/search/precheck",
            &t,
            json!({"search_type": "extended", "location": {"lat": CENTER.0, "lon": CENTER.1}}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body["radius_m"].is_null());
    app.cleanup().await;
}

#[tokio::test]
async fn location_outside_bishkek_is_rejected() {
    let app = app_or_skip!();
    let (t, _) = app.login("+996555100002").await;
    // Ош.
    let r = app
        .post(
            "/api/v1/search/precheck",
            &t,
            json!({"search_type": "basic", "location": {"lat": 40.5283, "lon": 72.7985}}),
        )
        .await;
    assert!(r.status.is_client_error(), "{:?}", r.body);
    app.cleanup().await;
}

#[tokio::test]
async fn create_order_defaults_unit_and_status() {
    let app = app_or_skip!();
    let (t, _) = app.login("+996555100003").await;
    let id = app.create_order(&t, paracetamol(2)).await;
    let r = app.get(&format!("/api/v1/search/orders/{id}"), &t).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["status"], "created");
    assert_eq!(r.body["medications"][0]["unit"], "шт");
    assert_eq!(r.body["medications"][0]["quantity"], 2);

    // Результаты до оплаты недоступны.
    let r = app.get(&format!("/api/v1/search/orders/{id}/results"), &t).await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    // История поисков пользователя.
    let r = app.get("/api/v1/users/me/orders", &t).await;
    assert_eq!(r.body.as_array().unwrap().len(), 1);
    app.cleanup().await;
}

#[tokio::test]
async fn blacklisted_and_invalid_medications_rejected() {
    let app = app_or_skip!();
    let (t, _) = app.login("+996555100004").await;
    let base = json!({"search_type": "basic", "location": {"lat": CENTER.0, "lon": CENTER.1}});

    let mut body = base.clone();
    body["medications"] = json!([{"name": "Морфин", "quantity": 1}]);
    let r = app.post("/api/v1/search/orders", &t, body).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);

    let mut body = base.clone();
    body["medications"] = json!([{"name": "Парацетамол", "quantity": 0}]);
    let r = app.post("/api/v1/search/orders", &t, body).await;
    assert!(r.status.is_client_error());

    let mut body = base;
    body["medications"] = json!([]);
    let r = app.post("/api/v1/search/orders", &t, body).await;
    assert!(r.status.is_client_error());
    app.cleanup().await;
}

#[tokio::test]
async fn user_cannot_see_foreign_order() {
    let app = app_or_skip!();
    let (alice, _) = app.login("+996555100005").await;
    let (bob, _) = app.login("+996555100006").await;
    let id = app.create_order(&alice, paracetamol(1)).await;
    let r = app.get(&format!("/api/v1/search/orders/{id}"), &bob).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .post(&format!("/api/v1/search/orders/{id}/payment"), &bob, json!({}))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    app.cleanup().await;
}
