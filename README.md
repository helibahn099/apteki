# Savvy Meds — поиск лекарств в аптеках Бишкека

Бэкенд: Rust, модульный монолит (axum + sqlx + PostgreSQL/PostGIS). Код — в `backend/`, описание API — `backend/openapi.yaml`.

Модули (`backend/src/modules`): auth, users, search, medication, pharmacy, integrations, payments, chat, ai, geo, notifications, logging.

## Локальный запуск

Требования: Rust stable, PostgreSQL 14+ с PostGIS.

```bash
createdb savvy_meds && psql -d savvy_meds -c 'CREATE EXTENSION postgis'
cd backend
cp .env.example .env      # заполните JWT_SECRET и DATABASE_URL
cargo run
```

Миграции применяются автоматически (`RUN_MIGRATIONS=true`), тестовые аптеки загружаются при `SEED_DEV_DATA=true` (не в production).
Сервер слушает `http://0.0.0.0:8080` (HTTP без TLS, SSL будет добавлен позже). Проверка: `curl localhost:8080/health`.

В режиме разработки OTP-код пишется в DEBUG-лог (`LOG_LEVEL=debug`). Тестовый Telegram: `TELEGRAM_BOT_ENABLED=true`,
`TELEGRAM_BOT_TOKEN`, `TELEGRAM_TEST_CHAT_ID` — все коды уходят в один чат (предварительно нажмите /start в боте).

## Docker

```bash
cd backend
docker build -t savvy-meds-backend .
docker run --rm -p 8080:8080 --env-file .env \
  -e DATABASE_URL=postgres://postgres:postgres@host.docker.internal:5432/savvy_meds savvy-meds-backend
```

## Переменные окружения

| Переменная | По умолчанию | Описание |
|---|---|---|
| `APP_ENV` | development | development / test / production (в production DEBUG-логи выключены) |
| `HTTP_HOST`, `HTTP_PORT` | 0.0.0.0, 8080 | Адрес сервера |
| `DATABASE_URL` | — | Строка подключения PostgreSQL |
| `RUN_MIGRATIONS`, `SEED_DEV_DATA` | true, false | Миграции и тестовые данные при старте |
| `LOG_LEVEL`, `LOG_FORMAT` | info, json | Уровень и формат логов |
| `JWT_SECRET` | — | Секрет JWT (≥ 32 символов) |
| `JWT_ACCESS_TTL_SECS`, `JWT_REFRESH_TTL_SECS` | 900, 2592000 | Время жизни токенов |
| `OTP_TTL_SECS`, `OTP_MAX_ATTEMPTS`, `OTP_RESEND_INTERVAL_SECS` | | Параметры OTP |
| `ADMIN_PHONES` | | Номера администраторов через запятую |
| `TELEGRAM_BOT_TOKEN`, `TELEGRAM_BOT_ENABLED`, `TELEGRAM_TEST_CHAT_ID` | —, false, — | Telegram (зарезервировано / тестовый режим) |
| `GEMINI_API_KEY`, `GEMINI_MODEL` | —, gemini-flash-latest | ИИ (зарезервировано, сейчас заглушка) |
| `SEARCH_MIN_RADIUS_METERS` | 100 | Начальный радиус базового поиска |
| `SEARCH_MAX_RADIUS_BASIC_METERS` | 4000 | Максимальный радиус базового поиска |
| `SEARCH_RADIUS_STEP_METERS` | 500 | Шаг расширения радиуса |
| `SEARCH_MAX_RESULTS` | 15 | Максимум результатов |
| `SEARCH_HARD_MIN_RESULTS`, `SEARCH_SOFT_MIN_RESULTS` | 1, 3 | Жёсткий и мягкий минимум |
| `SEARCH_WAVE_INTERVAL_SECS`, `SEARCH_RESPONSE_WAIT_SECS` | 60, 600 | Пауза между волнами и ожидание ответов |
| `PHARMACY_REQUEST_INTERVAL_SECS` | 60 | Не чаще 1 запроса на аптеку (глобально). **Будет уменьшаться по мере стабилизации системы** |
| `PHARMACY_RESPONSE_TTL_SECS` | 1800 | Сколько ждать ручной ответ аптеки |
| `SEARCH_PRICE_BASIC_KGS`, `SEARCH_PRICE_EXTENDED_KGS`, `SEARCH_PRICE_PER_EXTRA_ITEM_KGS`, `SEARCH_PRICE_INCLUDED_ITEMS` | 50, 150, 10, 3 | Стоимость поиска, сом |
| `MOCK_PAYMENT_ENABLED`, `MOCK_PAYMENT_WEBHOOK_SECRET` | true | Mock-провайдер оплаты |
| `PAYMENT_QR_TTL_SECS` | 900 | Время жизни QR |
| `CHAT_ACTIVE_HOURS` | 48 | Сколько часов от оплаты чат активен |
| `AI_EXTRA_TASK_AFTER_SECS` | 1800 | Когда показывать сообщение о долгом поиске |
| `API_RATE_LIMIT_PER_MINUTE` | 120 | Лимит запросов к API с одного IP |
| `CORS_ALLOWED_ORIGINS` | | Источники для веб-панели (через запятую) |
| `SUPPORT_TELEGRAM` | @helibahn | Контакт поддержки при ошибке поиска |

## Подключение новой аптечной интеграции

1. Создайте `backend/src/modules/integrations/<name>.rs` и реализуйте трейт `PharmacyAdapter`:
   `get_catalog`, `get_stock`, `get_prices`, `get_locations`, `send_query`, `receive_response`, плюс `kind()` (например `"onec_neman"`).
   `send_query` возвращает `QueryDispatch::Answered(answer)`, если система отвечает сразу, или `QueryDispatch::Pending`.
2. Подключите модуль в `integrations/mod.rs` и зарегистрируйте адаптер в `AdapterRegistry::with_defaults()`.
3. Назначьте интеграцию аптеке: `PUT /api/v1/admin/pharmacies/{id}/integration` с `{"kind": "onec_neman", "config": {...}}`
   (секреты не храните в `config` открытым текстом).
4. Добавьте unit-тесты адаптера; для примера см. `mock.rs`. Аптеки без интеграции работают в ручном режиме (`manual`) через веб-панель.

## Тесты

```bash
cd backend
cargo test --lib                                  # unit-тесты (БД не нужна)
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test   # + интеграционные
```

Интеграционные тесты (`backend/tests/integration/`) создают отдельную временную БД на каждый тест (нужна роль с правом CREATE DATABASE и PostGIS).
Без `DATABASE_URL` они пропускаются локально и падают в CI.

## CI

`.github/workflows/ci.yml`: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `cargo build --release`.
Чтобы блокировать merge при ошибке, включите в GitHub → Settings → Branches правило защиты `main` с обязательной проверкой `backend`.
