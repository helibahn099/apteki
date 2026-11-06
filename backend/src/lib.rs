//! Savvy Meds — бэкенд сервиса поиска лекарств в аптеках Бишкека.
//! Модульный монолит: axum + sqlx + PostgreSQL/PostGIS.

pub mod app;
pub mod config;
pub mod error;
pub mod modules;
pub mod state;

use sqlx::PgPool;

/// Встроенные миграции (`migrations/`).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Тестовые данные для разработки (аптеки, точки, каталог). Идемпотентны.
pub const DEV_SEED_SQL: &str = include_str!("../seeds/dev_seed.sql");

pub async fn run_migrations(db: &PgPool) -> anyhow::Result<()> {
    MIGRATOR.run(db).await?;
    Ok(())
}

pub async fn seed_dev_data(db: &PgPool) -> anyhow::Result<()> {
    sqlx::raw_sql(DEV_SEED_SQL).execute(db).await?;
    Ok(())
}
