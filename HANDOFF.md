# Передача проекта — Savvy Meds (сервис поиска лекарств)

> Этот файл создан автоматически в конце рабочей сессии Abacus AI Agent.
> Описывает состояние проекта на момент передачи в локальную разработку.

---

## Что сделано

### ✅ Rust Backend (`backend/`)

Полноценный модульный монолит на **axum + sqlx + PostgreSQL/PostGIS**.

**Модули:**
- `auth` — регистрация и вход по номеру телефона, JWT (access + refresh), выход из аккаунта
- `users` — профили пользователей, история поисков
- `search` — поисковый движок с волновым расширением радиуса (100м → 4000м), rate limiting на аптеки
- `medication` — каталог препаратов, сопоставление (FullMatch / PartialMatch / NoMatch)
- `pharmacy` — аптеки, торговые точки
- `integrations` — трейт `PharmacyAdapter` + mock-адаптер для тестирования + ручное подтверждение
- `payments` — Mock Payment Provider, QR-код, webhook, идемпотентность
- `chat` — переписка с аптеками, 48ч активности от оплаты, архив
- `ai` — заглушка Gemini Flash (интерфейс для замены провайдера)
- `geo` — поиск аптек по координатам через PostGIS
- `notifications` — заглушка
- `logging` — JSON-логи, `request_id`, `search_id`, уровни DEBUG/INFO/WARN/ERROR

**Тесты:** 101 unit-тест + 14 интеграционных тестов — все прошли на реальной PostgreSQL/PostGIS.

**Файлы:**
- `backend/migrations/20261106000001_init.sql` — все таблицы БД
- `backend/seeds/dev_seed.sql` — тестовые данные аптек и препаратов
- `backend/openapi.yaml` — документация API (все эндпоинты на русском)
- `backend/Dockerfile` — мультистейдж-сборка, порт 8080
- `.github/workflows/ci.yml` — CI/CD (fmt → clippy → test → build)
- `README.md` — инструкция по запуску, Docker, переменные окружения, добавление новой интеграции

**Не проверено:**
- Сборка Docker-образа (Docker не был запущен в среде Abacus)
- Запуск сервера целиком с `.env` (тесты проходили через интеграционные тесты с embedded конфигом)

---

### 🟡 Flutter Mobile App (`mobile/`)

**Создан только скелет папки**, полный Dart-код НЕ написан.

**Причина:** Инструмент мобильной разработки Abacus поддерживает только React Native/Expo, не Flutter. Flutter SDK не установлен в среде Abacus. Написать код без Flutter SDK означало бы выдать непроверенный код, который может не компилироваться.

**Что нужно реализовать** (всё описано в `README.md` и техническом задании `Uploads/technical_specification.md`):

- Авторизация по номеру телефона (OTP: Telegram/WhatsApp/SMS)
- Создание поискового запроса: список препаратов, редактирование, сохранение истории
- **Локальный OCR** (`google_mlkit_text_recognition`) — сканирование рецептов/упаковок через камеру, фото НЕ отправляются на сервер
- Геолокация (`geolocator`) + ручной ввод + карта (`flutter_map` + OpenStreetMap)
- Экран предпроверки до оплаты (только количество аптек поблизости)
- Оплата: QR-код (валюта KGS), polling статуса каждые 3 сек, таймаут QR
- Результаты: список аптек (FullMatch выше PartialMatch), цены, наличие, время подтверждения
- Чат с аптекой: переписка, звонок, маршрут (`map_launcher`), архив после 48ч
- История поисков, повтор поиска на основе старого
- При ошибке: показать `search_id` + кнопка "Написать в поддержку" → `t.me/helibahn`

**Рекомендуемые зависимости:**
```yaml
go_router, flutter_riverpod, dio, flutter_secure_storage,
google_mlkit_text_recognition, geolocator, flutter_map, latlong2,
map_launcher, qr_flutter, image_picker, intl, logger, permission_handler,
url_launcher, cached_network_image
```

**BASE_URL для Android-эмулятора:** `http://10.0.2.2:8080`
**BASE_URL для реального Android-устройства:** `http://<IP_вашей_машины>:8080`

---

### 🔴 Next.js Web-панель аптеки (`pharmacy-panel/`)

**Не создана.** Нужно реализовать в локальной разработке.

**Что должна уметь:**
- Аккаунт сотрудника:
  - Список входящих запросов поиска
  - Ответ на запрос: «Есть» / «Есть частично» / «Нет» + количество + цена за каждую позицию
- Аккаунт администратора аптеки:
  - Общая активность (сколько запросов пришло)
  - Статистика по клиентам

**API для панели уже готово в бэкенде** (см. `backend/openapi.yaml`):
- `POST /api/pharmacy/responses` — ответ на запрос
- `GET /api/pharmacy/requests` — входящие запросы
- `GET /api/pharmacy/stats` — статистика

---

### 🔴 Docker Compose

**Не создан.** Нужен файл `docker-compose.yml` в корне, который поднимает:
1. `postgres` (с PostGIS) — порт 5432
2. `backend` (Rust, сборка из `backend/Dockerfile`) — порт 8080
3. `pharmacy-panel` (Next.js) — порт 3000

---

## Переменные окружения (`.env`)

Скопировать `backend/.env.example` в `backend/.env` и заполнить:

| Переменная | Описание | Значение для теста |
|---|---|---|
| `DATABASE_URL` | PostgreSQL | `postgres://user:pass@localhost:5432/savvy_meds` |
| `JWT_SECRET` | Секрет для JWT | Любая длинная строка |
| `TELEGRAM_BOT_TOKEN` | Токен Telegram-бота | `8992688088:AAEW...` (получен, хранится локально) |
| `TELEGRAM_BOT_ENABLED` | Включить Telegram OTP | `true` |
| `GEMINI_API_KEY` | API-ключ Gemini Flash | Добавить когда будет готов |
| `SEARCH_MIN_RADIUS_METERS` | Начальный радиус | `100` |
| `SEARCH_MAX_RADIUS_BASIC_METERS` | Макс. радиус базового поиска | `4000` |
| `SEARCH_RADIUS_STEP_METERS` | Шаг расширения | `500` |
| `SEARCH_MAX_RESULTS` | Макс. результатов | `15` |
| `SEARCH_HARD_MIN_RESULTS` | Жёсткий минимум | `1` |
| `SEARCH_SOFT_MIN_RESULTS` | Мягкий минимум | `3` |
| `PHARMACY_REQUEST_INTERVAL_SECS` | Интервал запросов к аптеке | `60` |
| `MOCK_PAYMENT_ENABLED` | Тестовый платёжный провайдер | `true` |
| `SUPPORT_TELEGRAM` | Контакт поддержки | `@helibahn` |

> ⚠️ Перевыпустите Telegram Bot Token через @BotFather — он засветился в переписке с агентом.

---

## Как запустить бэкенд локально

```bash
# 1. PostgreSQL с PostGIS
docker run -d --name savvy-pg \
  -e POSTGRES_USER=savvy \
  -e POSTGRES_PASSWORD=savvy \
  -e POSTGRES_DB=savvy_meds \
  -p 5432:5432 postgis/postgis:16-3.4

# 2. Скопировать и заполнить .env
cp backend/.env.example backend/.env
# Отредактировать DATABASE_URL и JWT_SECRET

# 3. Применить миграции
cd backend
cargo install sqlx-cli --no-default-features --features postgres
sqlx migrate run

# 4. Загрузить тестовые данные (опционально)
psql $DATABASE_URL -f seeds/dev_seed.sql

# 5. Запустить
cargo run

# Сервер будет на http://localhost:8080
# Swagger/OpenAPI: backend/openapi.yaml
```

## Как запустить тесты

```bash
cd backend
cargo test
```

---

## Telegram OTP

Бот: `@hermes676869_bot`

> **Важно:** Нажмите `/start` в боте, иначе он не сможет присылать вам коды.
> **Важно:** Перевыпустите токен через @BotFather — он был виден в логах среды разработки.

---

## Итоги переговорённостей из чата

| Тема | Решение |
|---|---|
| ИИ-модель | Gemini Flash (Google AI Studio), токен добавить позже |
| OTP сейчас | Заглушка: код выводится в DEBUG-лог бэкенда |
| OTP в будущем | Telegram / WhatsApp / SMS на выбор пользователя |
| Платёжная система | Mock сейчас, реальный банк позже |
| HTTPS/TLS | Пока HTTP, добавить когда появится SSL-сертификат |
| Веб-панель аптеки | Отдельный Next.js, запускается в том же Docker Compose |
| Таймаут чата | 48 часов от момента успешной оплаты |
| Задача ИИ при задержке | Закомментирована; вместо неё — статичное сообщение |
| Ошибка поиска | Показывать search_id + ссылка на t.me/helibahn |
| Интервал запросов к аптеке | 60 сек (настраиваемо через конфиг, будет уменьшаться) |

---

## Техническое задание

Оригинал: `Uploads/technical_specification.md`
