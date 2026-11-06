//! Модули монолита. Каждый модуль отвечает за свою предметную область
//! и экспортирует `router()` с HTTP-эндпоинтами (если они есть).

pub mod ai;
pub mod auth;
pub mod chat;
pub mod geo;
pub mod integrations;
pub mod logging;
pub mod medication;
pub mod notifications;
pub mod payments;
pub mod pharmacy;
pub mod search;
pub mod users;
