//! Точка входа HTTP-сервера.

use std::net::SocketAddr;

use anyhow::Context;
use savvy_meds::{app, config::Config, modules, run_migrations, seed_dev_data, state::AppState};
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // .env необязателен: в Docker/production переменные задаются окружением.
    let _ = dotenvy::dotenv();
    let config = Config::from_env().context("ошибка конфигурации")?;
    modules::logging::init(&config);

    tracing::info!(env = ?config.app_env, "запуск savvy-meds-backend");
    let db = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&config.database_url)
        .await
        .context("не удалось подключиться к PostgreSQL")?;

    if config.run_migrations {
        run_migrations(&db).await.context("ошибка миграций")?;
        tracing::info!("миграции применены");
    }
    if config.seed_dev_data {
        if config.is_production() {
            tracing::warn!("SEED_DEV_DATA игнорируется в production");
        } else {
            seed_dev_data(&db).await.context("ошибка загрузки тестовых данных")?;
            tracing::info!("тестовые данные загружены");
        }
    }

    let addr: SocketAddr = format!("{}:{}", config.http_host, config.http_port)
        .parse()
        .context("некорректные HTTP_HOST/HTTP_PORT")?;
    let state = AppState::new(config, db)?;
    modules::search::maintenance::spawn(state.clone());

    let router = app::build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "HTTP-сервер запущен (без TLS)");
    axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("сервер остановлен");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
