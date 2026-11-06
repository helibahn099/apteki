//! Платежи: QR, webhook с подписью, идемпотентность.

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use savvy_meds::modules::payments::provider::{MockPaymentProvider, PaymentProvider};
use serde_json::json;

use crate::app_or_skip;
use crate::common::paracetamol;

#[tokio::test]
async fn payment_is_idempotent_and_returns_qr() {
    let app = app_or_skip!();
    let (t, _) = app.login("+996555200001").await;
    let id = app.create_order(&t, paracetamol(1)).await;

    let r1 = app
        .post(&format!("/api/v1/search/orders/{id}/payment"), &t, json!({}))
        .await;
    assert_eq!(r1.status, StatusCode::CREATED, "{:?}", r1.body);
    assert!(r1.body["qr_payload"].as_str().unwrap().starts_with("mockpay://"));
    assert!(r1.body["qr_svg"].as_str().unwrap().contains("<svg"));
    assert_eq!(r1.body["order_status"], "awaiting_payment");

    // Повторный запрос возвращает тот же счёт.
    let r2 = app
        .post(&format!("/api/v1/search/orders/{id}/payment"), &t, json!({}))
        .await;
    assert_eq!(r2.status, StatusCode::OK);
    assert_eq!(r1.body["payment_id"], r2.body["payment_id"]);
    app.cleanup().await;
}

#[tokio::test]
async fn webhook_signature_and_repeated_delivery() {
    let app = app_or_skip!();
    let (t, _) = app.login("+996555200002").await;
    let id = app.create_order(&t, paracetamol(1)).await;
    let r = app
        .post(&format!("/api/v1/search/orders/{id}/payment"), &t, json!({}))
        .await;
    let payment_id = r.body["payment_id"].as_str().unwrap().to_string();
    let (provider_pid, amount): (String, i64) =
        sqlx::query_as("SELECT provider_payment_id, amount_minor FROM payments WHERE id = $1::uuid")
            .bind(&payment_id)
            .fetch_one(&app.db)
            .await
            .unwrap();

    let provider = MockPaymentProvider::new(app.state.config.mock_payment_webhook_secret.clone());
    let (headers, body) = provider.simulate_paid_webhook(&provider_pid, amount).unwrap();
    let build = |sign: bool| {
        let mut b = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/payments/webhook/mock");
        for (k, v) in headers.iter() {
            if sign || k.as_str() != "x-mock-signature" {
                b = b.header(k, v);
            }
        }
        b.header("content-type", "application/json")
            .body(Body::from(body.clone()))
            .unwrap()
    };

    // Без подписи — отказ.
    let r = app.send(build(false)).await;
    assert!(r.status.is_client_error(), "{:?}", r.body);

    let r = app.send(build(true)).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["outcome"], "processed");

    // Повторное уведомление не запускает поиск повторно.
    let r = app.send(build(true)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["outcome"], "already_processed");

    app.wait_status(&t, id, &["search_completed"]).await;
    let (started,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM order_history WHERE search_order_id = $1 AND to_status = 'search_in_progress'",
    )
    .bind(id)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(started, 1, "поиск должен быть запущен ровно один раз");
    let (paid,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM payments WHERE search_order_id = $1 AND status = 'paid'")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(paid, 1);

    // Оплаченный заказ нельзя оплатить снова.
    let r = app
        .post(&format!("/api/v1/search/orders/{id}/payment"), &t, json!({}))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    app.cleanup().await;
}

#[tokio::test]
async fn concurrent_webhooks_start_search_once() {
    let app = app_or_skip!();
    let (t, _) = app.login("+996555200003").await;
    let id = app.create_order(&t, paracetamol(1)).await;
    let r = app
        .post(&format!("/api/v1/search/orders/{id}/payment"), &t, json!({}))
        .await;
    let payment_id = r.body["payment_id"].as_str().unwrap().to_string();

    let mut handles = Vec::new();
    for _ in 0..8 {
        let router = app.router.clone();
        let (token, pid) = (t.clone(), payment_id.clone());
        handles.push(tokio::spawn(async move {
            use tower::ServiceExt;
            let req = Request::builder()
                .method(Method::POST)
                .uri(format!("/api/v1/payments/mock/{pid}/confirm"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap();
            router.oneshot(req).await.unwrap().status()
        }));
    }
    for h in handles {
        assert!(h.await.unwrap().is_success());
    }
    app.wait_status(&t, id, &["search_completed"]).await;
    let (started,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM order_history WHERE search_order_id = $1 AND to_status = 'search_in_progress'",
    )
    .bind(id)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(started, 1);
    app.cleanup().await;
}
