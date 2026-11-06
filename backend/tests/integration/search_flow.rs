//! Сквозной сценарий: оплата → поиск (mock-адаптеры) → ответ сотрудника из панели → чат.

use axum::http::StatusCode;
use serde_json::json;

use crate::app_or_skip;
use crate::common::{login_staff, paracetamol, FULL_PHARMACY, MANUAL_PHARMACY};

#[tokio::test]
async fn full_search_flow_with_mock_adapter_and_manual_answer() {
    let app = app_or_skip!();
    let (t, _) = app.login("+996555300001").await;
    let id = app.create_order(&t, paracetamol(2)).await;
    app.pay(&t, id).await;
    app.wait_status(&t, id, &["search_completed"]).await;

    // Mock-адаптер always_full даёт полное совпадение.
    let r = app.get(&format!("/api/v1/search/orders/{id}/results"), &t).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let results = r.body["results"].as_array().unwrap();
    assert!(!results.is_empty() && results.len() <= 15);
    assert!(results
        .iter()
        .any(|x| x["pharmacy_id"] == FULL_PHARMACY && x["match_status"] == "full_match"));
    assert_eq!(r.body["hard_min_reached"], true);

    // Сотрудник ручной аптеки видит запрос без данных пользователя и отвечает.
    let staff = login_staff(&app, "+996555300002", MANUAL_PHARMACY, "pharmacy_staff").await;
    let r = app.get("/api/v1/pharmacy-panel/requests", &staff).await;
    assert_eq!(r.status, StatusCode::OK);
    let req = r
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["search_id"] == id.to_string())
        .cloned();
    if let Some(req) = req {
        let rid = req["id"].as_str().unwrap();
        let detail = app.get(&format!("/api/v1/pharmacy-panel/requests/{rid}"), &staff).await;
        assert!(
            !detail.body.to_string().contains("+996555300001"),
            "телефон не должен передаваться аптеке"
        );
        let med_id = detail.body["medications"][0]["medication_id"].clone();
        let r = app
            .post(
                &format!("/api/v1/pharmacy-panel/requests/{rid}/answer"),
                &staff,
                json!({"availability": "partial", "items": [{"medication_id": med_id, "available_quantity": 1, "unit_price_minor": 2500}]}),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        assert_eq!(r.body["match_status"], "partial_match");
        // Повторный ответ невозможен.
        let r2 = app
            .post(
                &format!("/api/v1/pharmacy-panel/requests/{rid}/answer"),
                &staff,
                json!({"availability": "none", "items": []}),
            )
            .await;
        assert_eq!(r2.status, StatusCode::CONFLICT);
    }

    // Чат пользователя с аптекой активен (48 ч от оплаты).
    let chats = app.get("/api/v1/chats", &t).await;
    let chat = chats
        .body
        .as_array()
        .unwrap()
        .first()
        .cloned()
        .expect("должен быть чат");
    let cid = chat["id"].as_str().unwrap();
    let r = app
        .post(
            &format!("/api/v1/chats/{cid}/messages"),
            &t,
            json!({"body": "Здравствуйте!"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    // После истечения срока — только чтение.
    sqlx::query("UPDATE chats SET active_until = now() - interval '1 minute' WHERE id = $1::uuid")
        .bind(cid)
        .execute(&app.db)
        .await
        .unwrap();
    let r = app
        .post(&format!("/api/v1/chats/{cid}/messages"), &t, json!({"body": "Ещё"}))
        .await;
    assert!(r.status.is_client_error(), "{:?}", r.body);
    let r = app.get(&format!("/api/v1/chats/{cid}/messages"), &t).await;
    assert_eq!(r.status, StatusCode::OK);

    assert!(!app.notifier.events().is_empty());
    app.cleanup().await;
}
