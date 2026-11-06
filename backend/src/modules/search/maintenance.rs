//! Фоновые задачи обслуживания (запускаются раз в MAINTENANCE_INTERVAL_SECS):
//! * архивирование чатов старше 48 часов;
//! * просрочка неотвеченных запросов к аптекам;
//! * просрочка неоплаченных QR-кодов;
//! * сообщение о долгом поиске (> 30 минут) — вместо задачи ИИ;
//! * очистка памяти rate limiter.

use crate::error::AppResult;
use crate::modules::chat;
use crate::state::AppState;

/// Статичное сообщение для долгого поиска (пока ИИ-задача закомментирована).
pub const LONG_SEARCH_MESSAGE: &str =
    "Поиск занимает больше времени, чем обычно: препарат может быть редким или сложным для поиска. \
Мы продолжаем опрашивать аптеки и сообщим, как только получим ответы.";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    pub archived_chats: u64,
    pub expired_responses: u64,
    pub expired_payments: u64,
    pub long_searches: u64,
}

pub async fn run_once(state: &AppState) -> AppResult<MaintenanceReport> {
    let cfg = &state.config;
    let archived_chats = chat::archive_expired(&state.db).await?;

    let expired_responses = sqlx::query(
        "UPDATE pharmacy_responses SET status = 'expired'
         WHERE status = 'pending' AND requested_at < now() - make_interval(secs => $1)",
    )
    .bind(cfg.pharmacy_response_ttl.as_secs() as f64)
    .execute(&state.db)
    .await?
    .rows_affected();

    let expired_payments = sqlx::query(
        "UPDATE payments SET status = 'expired', updated_at = now() WHERE status = 'pending' AND expires_at < now()",
    )
    .execute(&state.db)
    .await?
    .rows_affected();

    // Задача ИИ после 30 минут поиска.
    // TODO(AI): после подключения Gemini (GEMINI_API_KEY) заменить статичное сообщение
    // на дополнительную задачу ИИ-агента без доп. оплаты:
    //
    // for order_id in long_running_orders {
    //     let meds = load_order_medications(&state.db, order_id).await?;
    //     match state.ai.find_analogs(&specs).await {
    //         Ok(analysis) => save_ai_analysis(&state.db, order_id, &analysis).await?, // предположения, не факты
    //         Err(e) => tracing::warn!(search_id = %order_id, error = %e, "ИИ-задача не выполнена"),
    //     }
    // }
    let long_searches = sqlx::query(
        "UPDATE search_orders SET info_message = $2, updated_at = now()
         WHERE status = 'search_in_progress' AND info_message IS NULL
           AND search_started_at < now() - make_interval(secs => $1)",
    )
    .bind(cfg.ai_extra_task_after.as_secs() as f64)
    .bind(LONG_SEARCH_MESSAGE)
    .execute(&state.db)
    .await?
    .rows_affected();

    state.pharmacy_limiter.cleanup();

    let report = MaintenanceReport {
        archived_chats,
        expired_responses,
        expired_payments,
        long_searches,
    };
    if report != MaintenanceReport::default() {
        tracing::info!(?report, "обслуживание выполнено");
    }
    Ok(report)
}

/// Бесконечный цикл обслуживания.
pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(state.config.maintenance_interval);
        loop {
            interval.tick().await;
            if let Err(e) = run_once(&state).await {
                tracing::error!(error = %e, "ошибка фоновой задачи обслуживания");
            }
        }
    })
}
