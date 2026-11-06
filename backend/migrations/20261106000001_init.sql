-- Начальная схема базы данных сервиса поиска лекарств.
-- Все суммы хранятся в тыйынах (1 сом = 100 тыйын), валюта — KGS.
-- Координаты хранятся как geography(Point, 4326) — PostGIS.

CREATE EXTENSION IF NOT EXISTS postgis;

-- ---------------------------------------------------------------------------
-- Граница города (для «расширенного поиска по всему Бишкеку» и проверки,
-- что точка пользователя находится в зоне обслуживания).
-- ---------------------------------------------------------------------------
CREATE TABLE city_boundaries (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL UNIQUE,
    boundary    GEOGRAPHY(POLYGON, 4326) NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Приближённый контур Бишкека. TODO: заменить на точную границу из OpenStreetMap
-- (relation «Бишкек», admin_level=4) при подготовке к production.
INSERT INTO city_boundaries (name, boundary) VALUES (
    'Бишкек',
    ST_GeogFromText('SRID=4326;POLYGON((74.47 42.83, 74.52 42.80, 74.62 42.79, 74.70 42.81, 74.72 42.87, 74.68 42.92, 74.58 42.93, 74.49 42.91, 74.47 42.83))')
);

-- ---------------------------------------------------------------------------
-- Аптеки и торговые точки
-- ---------------------------------------------------------------------------
CREATE TABLE pharmacies (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    chain_name  TEXT,
    phone       TEXT,
    is_active   BOOLEAN NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE pharmacy_locations (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    pharmacy_id   UUID NOT NULL REFERENCES pharmacies(id) ON DELETE CASCADE,
    address       TEXT NOT NULL,
    city          TEXT NOT NULL DEFAULT 'Бишкек',
    location      GEOGRAPHY(POINT, 4326) NOT NULL,
    phone         TEXT,
    working_hours TEXT,
    external_id   TEXT, -- идентификатор точки во внешней системе (1С и т.п.)
    is_active     BOOLEAN NOT NULL DEFAULT TRUE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_pharmacy_locations_geo ON pharmacy_locations USING GIST (location);
CREATE INDEX idx_pharmacy_locations_pharmacy ON pharmacy_locations (pharmacy_id);

-- Интеграции: какой адаптер используется для аптеки (mock, manual, 1c_*, ...).
CREATE TABLE integrations (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    pharmacy_id UUID NOT NULL UNIQUE REFERENCES pharmacies(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind ~ '^[a-z0-9_]{2,40}$'),
    config      JSONB NOT NULL DEFAULT '{}'::jsonb, -- параметры адаптера (без секретов в открытом виде)
    is_enabled  BOOLEAN NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Пользователи, OTP и сессии
-- ---------------------------------------------------------------------------
CREATE TABLE users (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    phone        TEXT NOT NULL UNIQUE,
    display_name TEXT CHECK (display_name IS NULL OR length(display_name) <= 100),
    role         TEXT NOT NULL DEFAULT 'user'
                 CHECK (role IN ('user', 'pharmacy_staff', 'pharmacy_admin', 'admin')),
    pharmacy_id  UUID REFERENCES pharmacies(id) ON DELETE SET NULL,
    is_blocked   BOOLEAN NOT NULL DEFAULT FALSE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Сотрудник аптеки обязан быть привязан к аптеке.
    CHECK (role NOT IN ('pharmacy_staff', 'pharmacy_admin') OR pharmacy_id IS NOT NULL)
);

-- Одноразовые коды. Хранится только хеш кода (HMAC-SHA256), не сам код.
CREATE TABLE otp_codes (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    phone        TEXT NOT NULL,
    code_hash    TEXT NOT NULL,
    channel      TEXT NOT NULL,
    attempts     INT NOT NULL DEFAULT 0,
    expires_at   TIMESTAMPTZ NOT NULL,
    consumed_at  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_otp_codes_phone ON otp_codes (phone, created_at DESC);

-- Сессии: хранится только SHA-256 хеш refresh-токена.
CREATE TABLE sessions (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id             UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    refresh_token_hash  TEXT NOT NULL UNIQUE,
    user_agent          TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at          TIMESTAMPTZ NOT NULL,
    revoked_at          TIMESTAMPTZ
);
CREATE INDEX idx_sessions_user ON sessions (user_id);

-- ---------------------------------------------------------------------------
-- Каталог препаратов и чёрный список
-- ---------------------------------------------------------------------------
CREATE TABLE medication_catalog (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name             TEXT NOT NULL,
    active_substance TEXT, -- МНН
    dosage           TEXT,
    concentration    TEXT,
    form             TEXT,
    manufacturer     TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_medication_catalog_name ON medication_catalog (lower(name));

-- Препараты, поиск которых через сервис запрещён (например, наркотические/психотропные).
CREATE TABLE medication_blacklist (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name             TEXT,
    active_substance TEXT,
    reason           TEXT NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (name IS NOT NULL OR active_substance IS NOT NULL)
);

-- ---------------------------------------------------------------------------
-- Поисковые заказы
-- ---------------------------------------------------------------------------
CREATE TABLE search_orders (
    id                        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id                   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    search_type               TEXT NOT NULL CHECK (search_type IN ('basic', 'extended')),
    status                    TEXT NOT NULL DEFAULT 'created'
                              CHECK (status IN ('created', 'awaiting_payment', 'paid',
                                                'search_in_progress', 'search_completed',
                                                'processing_error')),
    location                  GEOGRAPHY(POINT, 4326) NOT NULL,
    current_radius_m          INT,
    precheck_pharmacy_count   INT NOT NULL DEFAULT 0,
    price_minor               BIGINT NOT NULL CHECK (price_minor >= 0),
    currency                  TEXT NOT NULL DEFAULT 'KGS',
    info_message              TEXT, -- служебное сообщение пользователю (например, о долгом поиске)
    error_message             TEXT,
    source_order_id           UUID REFERENCES search_orders(id) ON DELETE SET NULL,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    paid_at                   TIMESTAMPTZ,
    search_started_at         TIMESTAMPTZ,
    search_completed_at       TIMESTAMPTZ
);
CREATE INDEX idx_search_orders_user ON search_orders (user_id, created_at DESC);
CREATE INDEX idx_search_orders_status ON search_orders (status);

-- Препараты конкретного заказа (снимок запроса пользователя).
CREATE TABLE medications (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    search_order_id  UUID NOT NULL REFERENCES search_orders(id) ON DELETE CASCADE,
    catalog_id       UUID REFERENCES medication_catalog(id) ON DELETE SET NULL,
    position         INT NOT NULL,
    name             TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    active_substance TEXT,
    dosage           TEXT,
    concentration    TEXT,
    form             TEXT,
    manufacturer     TEXT,
    quantity         INT NOT NULL CHECK (quantity > 0 AND quantity <= 1000),
    unit             TEXT NOT NULL DEFAULT 'шт'
);
CREATE INDEX idx_medications_order ON medications (search_order_id, position);

-- История смены статусов заказа.
CREATE TABLE order_history (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    search_order_id  UUID NOT NULL REFERENCES search_orders(id) ON DELETE CASCADE,
    from_status      TEXT,
    to_status        TEXT NOT NULL,
    note             TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_order_history_order ON order_history (search_order_id, created_at);

-- ---------------------------------------------------------------------------
-- Платежи
-- ---------------------------------------------------------------------------
CREATE TABLE payments (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    search_order_id      UUID NOT NULL REFERENCES search_orders(id) ON DELETE CASCADE,
    provider             TEXT NOT NULL,
    provider_payment_id  TEXT NOT NULL UNIQUE,
    amount_minor         BIGINT NOT NULL CHECK (amount_minor >= 0),
    currency             TEXT NOT NULL DEFAULT 'KGS',
    status               TEXT NOT NULL DEFAULT 'pending'
                         CHECK (status IN ('pending', 'paid', 'failed', 'expired')),
    qr_payload           TEXT NOT NULL,
    idempotency_key      TEXT UNIQUE,
    expires_at           TIMESTAMPTZ NOT NULL,
    paid_at              TIMESTAMPTZ,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_payments_order ON payments (search_order_id);
-- Защита от двойной оплаты: у заказа может быть только один успешный платёж.
CREATE UNIQUE INDEX uq_payments_one_paid_per_order ON payments (search_order_id) WHERE status = 'paid';

-- ---------------------------------------------------------------------------
-- Ответы аптек (временный снимок результатов конкретного поиска,
-- НЕ глобальная база остатков).
-- ---------------------------------------------------------------------------
CREATE TABLE pharmacy_responses (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    search_order_id    UUID NOT NULL REFERENCES search_orders(id) ON DELETE CASCADE,
    pharmacy_id        UUID NOT NULL REFERENCES pharmacies(id) ON DELETE CASCADE,
    location_id        UUID NOT NULL REFERENCES pharmacy_locations(id) ON DELETE CASCADE,
    wave               INT NOT NULL,
    radius_m           INT NOT NULL,
    distance_m         DOUBLE PRECISION NOT NULL,
    status             TEXT NOT NULL DEFAULT 'pending'
                       CHECK (status IN ('pending', 'answered', 'expired', 'failed')),
    availability       TEXT CHECK (availability IN ('full', 'partial', 'none')),
    match_status       TEXT CHECK (match_status IN ('full_match', 'partial_match', 'no_match')),
    items              JSONB NOT NULL DEFAULT '[]'::jsonb,
    total_price_minor  BIGINT,
    requested_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    answered_at        TIMESTAMPTZ,
    answered_by        UUID REFERENCES users(id) ON DELETE SET NULL,
    UNIQUE (search_order_id, location_id)
);
CREATE INDEX idx_pharmacy_responses_pharmacy ON pharmacy_responses (pharmacy_id, status, requested_at DESC);

-- ---------------------------------------------------------------------------
-- Чаты и сообщения
-- ---------------------------------------------------------------------------
CREATE TABLE chats (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    search_order_id       UUID NOT NULL REFERENCES search_orders(id) ON DELETE CASCADE,
    pharmacy_id           UUID NOT NULL REFERENCES pharmacies(id) ON DELETE CASCADE,
    location_id           UUID NOT NULL REFERENCES pharmacy_locations(id) ON DELETE CASCADE,
    pharmacy_response_id  UUID REFERENCES pharmacy_responses(id) ON DELETE SET NULL,
    user_id               UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status                TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    active_until          TIMESTAMPTZ NOT NULL,
    archived_at           TIMESTAMPTZ,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (search_order_id, location_id)
);
CREATE INDEX idx_chats_user ON chats (user_id, created_at DESC);
CREATE INDEX idx_chats_pharmacy ON chats (pharmacy_id, created_at DESC);

CREATE TABLE messages (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    chat_id         UUID NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    sender_type     TEXT NOT NULL CHECK (sender_type IN ('user', 'pharmacy', 'system')),
    sender_user_id  UUID REFERENCES users(id) ON DELETE SET NULL,
    body            TEXT NOT NULL CHECK (length(body) BETWEEN 1 AND 4000),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_messages_chat ON messages (chat_id, created_at);
