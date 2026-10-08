CREATE TABLE servers (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id           uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    key_id            uuid NOT NULL REFERENCES server_keys (id) ON DELETE CASCADE,
    machine_id        text NOT NULL,
    hostname          text NOT NULL,
    agent_version     text NOT NULL,
    token_hash        text NOT NULL UNIQUE,
    cpu_total         bigint NOT NULL DEFAULT 0,
    cpu_used          bigint NOT NULL DEFAULT 0,
    memory_total      bigint NOT NULL DEFAULT 0,
    memory_used       bigint NOT NULL DEFAULT 0,
    disk_total        bigint NOT NULL DEFAULT 0,
    disk_used         bigint NOT NULL DEFAULT 0,
    kvm               boolean NOT NULL DEFAULT false,
    last_heartbeat_at timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now()
);
-- One server per host and owner: re-running the installer updates this row.
CREATE UNIQUE INDEX servers_user_machine_key ON servers (user_id, machine_id);
