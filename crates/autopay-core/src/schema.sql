CREATE TABLE IF NOT EXISTS subscriptions (
    id                  TEXT PRIMARY KEY,
    wallet_id           TEXT NOT NULL,
    label               TEXT NOT NULL,
    description         TEXT,
    destination_address TEXT NOT NULL,
    payment_network     TEXT NOT NULL DEFAULT 'monero',
    amount              TEXT NOT NULL,
    currency            TEXT NOT NULL,
    schedule            TEXT NOT NULL,
    number_of_payments  INTEGER NOT NULL DEFAULT 0,
    payments_sent       INTEGER NOT NULL DEFAULT 0,
    next_payment_at     TEXT,
    status              TEXT NOT NULL DEFAULT 'pending',
    grace_period_hours  INTEGER NOT NULL DEFAULT 72,
    canary_url          TEXT,
    merchant_pubkey     TEXT,
    last_canary_status  TEXT,
    canary_last_ok      TEXT,
    webhook_url         TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    raw_code            TEXT,
    notes               TEXT
);

CREATE TABLE IF NOT EXISTS payments (
    id                  TEXT PRIMARY KEY,
    subscription_id     TEXT NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    wallet_id           TEXT NOT NULL,
    destination_address TEXT NOT NULL,
    amount_atomic       INTEGER NOT NULL,
    currency            TEXT NOT NULL,
    tx_id               TEXT,
    status              TEXT NOT NULL DEFAULT 'pending',
    error_message       TEXT,
    initiated_at        TEXT NOT NULL,
    confirmed_at        TEXT
);

CREATE INDEX IF NOT EXISTS idx_payments_subscription
    ON payments(subscription_id, initiated_at DESC);

CREATE INDEX IF NOT EXISTS idx_subscriptions_next_payment
    ON subscriptions(next_payment_at ASC)
    WHERE status IN ('active', 'past_due');
