//! Интеграционные тесты: HTTP API + реальная PostgreSQL/PostGIS.
//!
//! Запуск: `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --test integration`

mod common;

mod auth_api;
mod orders_api;
mod payments_api;
mod search_flow;
