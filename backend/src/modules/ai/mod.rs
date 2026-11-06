//! ИИ-модуль (вспомогательный).
//!
//! Правила (ТЗ):
//! - ИИ не делает медицинских выводов и не ставит диагнозы;
//! - любые предположения ИИ отделяются от подтверждённых данных (`assumptions` vs `confirmed`);
//! - каждый ответ проходит валидацию (`validate_ai_response`);
//! - провайдер скрыт за трейтом `AiProvider`, его можно заменить без изменения бизнес-логики.
//!
//! Сейчас используется заглушка `GeminiFlashProvider` (Gemini Flash через Google AI Studio).

use std::sync::Arc;

use async_trait::async_trait;
use axum::{extract::State, routing::get, Json, Router};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::modules::auth::AuthUser;
use crate::modules::medication::matching::MedicationSpec;
use crate::state::AppState;

/// Обязательная оговорка, добавляемая к каждому ответу ИИ.
pub const AI_DISCLAIMER: &str =
    "Информация носит справочный характер и не является медицинской рекомендацией. Проконсультируйтесь с врачом или фармацевтом.";

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("ИИ-провайдер не настроен")]
    NotConfigured,
    #[error("ответ ИИ не прошёл валидацию: {0}")]
    InvalidResponse(String),
    #[error("ошибка ИИ-провайдера: {0}")]
    Provider(String),
}

/// Ответ ИИ: подтверждённые факты строго отделены от предположений.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AiAnalysis {
    /// Данные, подтверждённые справочником/каталогом.
    pub confirmed: Vec<String>,
    /// Предположения ИИ — показываются пользователю с пометкой «предположение».
    pub assumptions: Vec<String>,
    /// Уточняющие вопросы пользователю.
    pub questions: Vec<String>,
    pub disclaimer: String,
}

#[async_trait]
pub trait AiProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn is_configured(&self) -> bool;
    /// Уточняющие вопросы по неполному запросу.
    async fn clarifying_questions(&self, items: &[MedicationSpec]) -> Result<AiAnalysis, AiError>;
    /// Анализ действующих веществ.
    async fn analyze_substances(&self, items: &[MedicationSpec]) -> Result<AiAnalysis, AiError>;
    /// Поиск возможных аналогов (только как предположения!).
    async fn find_analogs(&self, items: &[MedicationSpec]) -> Result<AiAnalysis, AiError>;
}

/// Слова, недопустимые в ответе ИИ (медицинские назначения/диагнозы).
const FORBIDDEN_PHRASES: &[&str] = &["диагноз", "назначаю", "принимайте по", "вам следует принимать"];

/// Валидация ответа ИИ перед показом пользователю.
pub fn validate_ai_response(a: &AiAnalysis) -> Result<(), AiError> {
    if a.disclaimer.trim().is_empty() {
        return Err(AiError::InvalidResponse("нет оговорки о справочном характере".into()));
    }
    let all = a.confirmed.iter().chain(&a.assumptions).chain(&a.questions);
    for text in all {
        if text.chars().count() > 1000 {
            return Err(AiError::InvalidResponse("слишком длинный фрагмент".into()));
        }
        let lower = text.to_lowercase();
        if let Some(p) = FORBIDDEN_PHRASES.iter().find(|p| lower.contains(*p)) {
            return Err(AiError::InvalidResponse(format!("недопустимая формулировка: «{p}»")));
        }
    }
    Ok(())
}

/// Заглушка Gemini Flash.
pub struct GeminiFlashProvider {
    api_key: Option<String>,
    model: String,
}

impl GeminiFlashProvider {
    pub fn new(api_key: Option<String>, model: String) -> Self {
        Self { api_key, model }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn stub(&self, questions: Vec<String>) -> Result<AiAnalysis, AiError> {
        // TODO(AI): когда появится GEMINI_API_KEY — реализовать вызов
        // POST https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent
        // с заголовком `x-goog-api-key` (ключ НЕ логировать), разобрать JSON-ответ
        // в AiAnalysis и прогнать через validate_ai_response.
        let analysis = AiAnalysis {
            confirmed: vec![],
            assumptions: vec![],
            questions,
            disclaimer: AI_DISCLAIMER.to_string(),
        };
        validate_ai_response(&analysis)?;
        Ok(analysis)
    }
}

#[async_trait]
impl AiProvider for GeminiFlashProvider {
    fn name(&self) -> &'static str {
        "gemini_flash"
    }

    fn is_configured(&self) -> bool {
        self.api_key.as_deref().is_some_and(|k| !k.trim().is_empty())
    }

    async fn clarifying_questions(&self, items: &[MedicationSpec]) -> Result<AiAnalysis, AiError> {
        // Без ИИ: простые правила по незаполненным полям.
        let mut q = Vec::new();
        for it in items {
            if it.dosage.is_none() {
                q.push(format!("Уточните дозировку препарата «{}».", it.name));
            }
            if it.form.is_none() {
                q.push(format!(
                    "Уточните форму выпуска препарата «{}» (таблетки, сироп и т.д.).",
                    it.name
                ));
            }
        }
        self.stub(q)
    }

    async fn analyze_substances(&self, _items: &[MedicationSpec]) -> Result<AiAnalysis, AiError> {
        if !self.is_configured() {
            return Err(AiError::NotConfigured);
        }
        self.stub(vec![])
    }

    async fn find_analogs(&self, _items: &[MedicationSpec]) -> Result<AiAnalysis, AiError> {
        if !self.is_configured() {
            return Err(AiError::NotConfigured);
        }
        self.stub(vec![])
    }
}

pub fn build_ai_provider(config: &Config) -> Arc<dyn AiProvider> {
    Arc::new(GeminiFlashProvider::new(
        config.gemini_api_key.clone(),
        config.gemini_model.clone(),
    ))
}

#[derive(Serialize)]
struct AiStatus {
    provider: &'static str,
    configured: bool,
    disclaimer: &'static str,
}

/// GET /ai/status — состояние ИИ-модуля.
async fn status(State(state): State<AppState>, _user: AuthUser) -> Json<AiStatus> {
    Json(AiStatus {
        provider: state.ai.name(),
        configured: state.ai.is_configured(),
        disclaimer: AI_DISCLAIMER,
    })
}

pub fn router() -> Router<AppState> {
    Router::new().route("/ai/status", get(status))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> MedicationSpec {
        MedicationSpec {
            name: name.into(),
            active_substance: None,
            dosage: None,
            concentration: None,
            form: None,
            manufacturer: None,
            quantity: 1,
        }
    }

    #[tokio::test]
    async fn stub_without_key_is_not_configured() {
        let p = GeminiFlashProvider::new(None, "gemini-flash-latest".into());
        assert!(!p.is_configured());
        assert!(matches!(
            p.find_analogs(&[spec("Парацетамол")]).await,
            Err(AiError::NotConfigured)
        ));
    }

    #[tokio::test]
    async fn clarifying_questions_work_without_ai() {
        let p = GeminiFlashProvider::new(None, "m".into());
        let a = p.clarifying_questions(&[spec("Нурофен")]).await.unwrap();
        assert_eq!(a.questions.len(), 2);
        assert!(a.assumptions.is_empty());
        assert_eq!(a.disclaimer, AI_DISCLAIMER);
    }

    #[test]
    fn validation_rejects_medical_advice() {
        let a = AiAnalysis {
            confirmed: vec![],
            assumptions: vec!["Вам следует принимать 2 таблетки".into()],
            questions: vec![],
            disclaimer: AI_DISCLAIMER.into(),
        };
        assert!(validate_ai_response(&a).is_err());
    }

    #[test]
    fn validation_requires_disclaimer() {
        let a = AiAnalysis {
            confirmed: vec![],
            assumptions: vec![],
            questions: vec![],
            disclaimer: " ".into(),
        };
        assert!(validate_ai_response(&a).is_err());
    }
}
