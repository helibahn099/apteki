//! Общая инфраструктура интеграционных тестов.
//!
//! Каждый тест получает отдельную временную БД (`savvy_test_<uuid>`), созданную
//! на сервере из `TEST_DATABASE_URL` (или `DATABASE_URL`). Применяются миграции
//! и тестовые данные `seeds/dev_seed.sql`.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use savvy_meds::{
    app::build_router,
    config::Config,
    modules::{auth::otp::CapturingOtpSender, notifications::RecordingNotifier},
    run_migrations, seed_dev_data,
    state::AppState,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, Connection, PgConnection, PgPool};
use tower::ServiceExt;
use uuid::Uuid;

/// Центр Бишкека (рядом с тестовыми аптеками).
pub const CENTER: (f64, f64) = (42.8746, 74.5698);
/// Аптека с mock-интеграцией `always_full`.
pub const FULL_PHARMACY: &str = "00000000-0000-0000-0000-00000000a002";
/// Аптека с ручным подтверждением.
pub const MANUAL_PHARMACY: &str = "00000000-0000-0000-0000-00000000a003";
pub const ADMIN_PHONE: &str = "+996700000001";

pub struct TestApp {
    pub router: Router,
    pub state: AppState,
    pub db: PgPool,
    pub otp: Arc<CapturingOtpSender>,
    pub notifier: Arc<RecordingNotifier>,
    admin_url: String,
    db_name: String,
}

pub struct Resp {
    pub status: StatusCode,
    pub body: Value,
}

fn base_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
}

fn with_db_name(url: &str, name: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let prefix = &base[..base.rfind('/').expect("в URL нет имени БД")];
    match query {
        Some(q) => format!("{prefix}/{name}?{q}"),
        None => format!("{prefix}/{name}"),
    }
}

impl TestApp {
    /// Поднимает приложение на новой БД. Возвращает `None`, если БД не настроена
    /// (локально без PostgreSQL тесты пропускаются; в CI переменная `CI` делает это ошибкой).
    pub async fn spawn() -> Option<Self> {
        Self::spawn_with(|_| {}).await
    }

    pub async fn spawn_with(customize: impl FnOnce(&mut Config)) -> Option<Self> {
        let Some(url) = base_url() else {
            if std::env::var("CI").is_ok() {
                panic!("в CI должна быть задана TEST_DATABASE_URL или DATABASE_URL");
            }
            eprintln!("ПРОПУСК: не задана TEST_DATABASE_URL/DATABASE_URL");
            return None;
        };
        let db_name = format!("savvy_test_{}", Uuid::new_v4().simple());
        let admin_url = with_db_name(&url, "postgres");
        let mut admin = PgConnection::connect(&admin_url)
            .await
            .expect("подключение к PostgreSQL");
        sqlx::query(&format!("CREATE DATABASE {db_name}"))
            .execute(&mut admin)
            .await
            .expect("создание тестовой БД");
        admin.close().await.ok();

        let test_url = with_db_name(&url, &db_name);
        let db = PgPoolOptions::new()
            .max_connections(10)
            .connect(&test_url)
            .await
            .expect("подключение к тестовой БД");
        run_migrations(&db).await.expect("миграции");
        seed_dev_data(&db).await.expect("seed");

        let mut config = Config::for_tests(&test_url);
        customize(&mut config);
        let otp = Arc::new(CapturingOtpSender::default());
        let notifier = Arc::new(RecordingNotifier::default());
        let state = AppState::new(config, db.clone())
            .expect("состояние приложения")
            .with_otp_sender(otp.clone())
            .with_notifier(notifier.clone());
        let router = build_router(state.clone());
        Some(Self {
            router,
            state,
            db,
            otp,
            notifier,
            admin_url,
            db_name,
        })
    }

    /// Удаляет тестовую БД.
    pub async fn cleanup(self) {
        self.db.close().await;
        if let Ok(mut admin) = PgConnection::connect(&self.admin_url).await {
            let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.db_name))
                .execute(&mut admin)
                .await;
        }
    }

    pub async fn request(&self, method: Method, uri: &str, token: Option<&str>, body: Option<Value>) -> Resp {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let req = match body {
            Some(b) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(b.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        self.send(req).await
    }

    pub async fn send(&self, req: Request<Body>) -> Resp {
        let resp = self.router.clone().oneshot(req).await.expect("ответ сервера");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
        };
        Resp { status, body }
    }

    pub async fn get(&self, uri: &str, token: &str) -> Resp {
        self.request(Method::GET, uri, Some(token), None).await
    }

    pub async fn post(&self, uri: &str, token: &str, body: Value) -> Resp {
        self.request(Method::POST, uri, Some(token), Some(body)).await
    }

    /// Полный вход по OTP. Возвращает (access_token, refresh_token).
    pub async fn login(&self, phone: &str) -> (String, String) {
        let r = self
            .request(
                Method::POST,
                "/api/v1/auth/otp/request",
                None,
                Some(json!({"phone": phone, "channel": "telegram"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "otp/request: {:?}", r.body);
        let code = self.otp.last_code(phone).expect("код должен быть отправлен");
        let r = self
            .request(
                Method::POST,
                "/api/v1/auth/otp/verify",
                None,
                Some(json!({"phone": phone, "code": code})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "otp/verify: {:?}", r.body);
        (
            r.body["access_token"].as_str().unwrap().to_string(),
            r.body["refresh_token"].as_str().unwrap().to_string(),
        )
    }

    /// Создаёт заказ (базовый поиск, центр города) и возвращает его ID.
    pub async fn create_order(&self, token: &str, medications: Value) -> Uuid {
        let r = self
            .post(
                "/api/v1/search/orders",
                token,
                json!({
                    "search_type": "basic",
                    "location": {"lat": CENTER.0, "lon": CENTER.1},
                    "medications": medications,
                }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "create_order: {:?}", r.body);
        r.body["id"].as_str().unwrap().parse().unwrap()
    }

    /// Выставляет счёт и подтверждает оплату через mock. Возвращает payment_id.
    pub async fn pay(&self, token: &str, order_id: Uuid) -> Uuid {
        let r = self
            .post(&format!("/api/v1/search/orders/{order_id}/payment"), token, json!({}))
            .await;
        assert!(r.status.is_success(), "payment: {:?}", r.body);
        let payment_id: Uuid = r.body["payment_id"].as_str().unwrap().parse().unwrap();
        let r = self
            .post(&format!("/api/v1/payments/mock/{payment_id}/confirm"), token, json!({}))
            .await;
        assert_eq!(r.status, StatusCode::OK, "mock confirm: {:?}", r.body);
        payment_id
    }

    /// Ждёт, пока заказ перейдёт в один из финальных статусов.
    pub async fn wait_status(&self, token: &str, order_id: Uuid, expected: &[&str]) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let r = self.get(&format!("/api/v1/search/orders/{order_id}"), token).await;
            let status = r.body["status"].as_str().unwrap_or_default().to_string();
            if expected.contains(&status.as_str()) {
                return r.body;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "таймаут ожидания статуса, текущий: {status}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

pub fn paracetamol(qty: u32) -> Value {
    json!([{ "name": "Парацетамол", "dosage": "500 мг", "form": "таблетки", "quantity": qty }])
}

/// Создаёт пользователя-сотрудника аптеки и возвращает его токен.
pub async fn login_staff(app: &TestApp, phone: &str, pharmacy_id: &str, role: &str) -> String {
    let (admin, _) = app.login(ADMIN_PHONE).await;
    let r = app
        .post(
            &format!("/api/v1/admin/pharmacies/{pharmacy_id}/staff"),
            &admin,
            json!({"phone": phone, "role": role}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "assign staff: {:?}", r.body);
    app.login(phone).await.0
}

/// Макрос: поднять приложение или пропустить тест без БД.
#[macro_export]
macro_rules! app_or_skip {
    () => {
        match $crate::common::TestApp::spawn().await {
            Some(app) => app,
            None => return,
        }
    };
    ($customize:expr) => {
        match $crate::common::TestApp::spawn_with($customize).await {
            Some(app) => app,
            None => return,
        }
    };
}
