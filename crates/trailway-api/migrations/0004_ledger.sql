-- Usage samples sent by agents: one row per reported interval.
CREATE TABLE usage_samples (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    server_id    uuid NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    user_id      uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    period_start timestamptz NOT NULL,
    period_end   timestamptz NOT NULL,
    cpu_total    bigint NOT NULL,
    cpu_used     bigint NOT NULL,
    memory_total bigint NOT NULL,
    memory_used  bigint NOT NULL,
    kvm          boolean NOT NULL,
    credited     boolean NOT NULL,
    received_at  timestamptz NOT NULL DEFAULT now(),
    CHECK (period_end > period_start),
    UNIQUE (server_id, period_start)
);
CREATE INDEX usage_samples_server_period ON usage_samples (server_id, period_end);

-- Append-only ledger. Balance is always derived with SUM, never stored.
-- Compute is kept in two separate units: millicore-seconds and byte-seconds.
CREATE TABLE ledger_entries (
    seq              bigserial PRIMARY KEY,
    user_id          uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    server_id        uuid,
    sample_id        uuid UNIQUE,
    kind             text NOT NULL CHECK (kind IN ('contributed', 'consumed')),
    period_start     timestamptz NOT NULL,
    period_end       timestamptz NOT NULL,
    millicore_seconds bigint NOT NULL CHECK (millicore_seconds >= 0),
    byte_seconds     numeric NOT NULL CHECK (byte_seconds >= 0),
    created_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ledger_entries_user_seq ON ledger_entries (user_id, seq DESC);

-- server_id and sample_id are plain ids (no FK actions): deleting a server must
-- never rewrite history. Past entries are never changed.
CREATE FUNCTION ledger_entries_no_update() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'ledger_entries is append-only';
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER ledger_entries_append_only
    BEFORE UPDATE ON ledger_entries
    FOR EACH ROW EXECUTE FUNCTION ledger_entries_no_update();
