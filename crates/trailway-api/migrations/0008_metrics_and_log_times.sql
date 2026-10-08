-- CPU and memory of each service's running VM, one row per agent report.
CREATE TABLE service_metrics (
    id             bigserial PRIMARY KEY,
    service_id     uuid NOT NULL REFERENCES services (id) ON DELETE CASCADE,
    deployment_id  uuid NOT NULL,
    ts             timestamptz NOT NULL DEFAULT now(),
    cpu_millicores integer NOT NULL CHECK (cpu_millicores >= 0),
    memory_bytes   bigint NOT NULL CHECK (memory_bytes >= 0)
);
CREATE INDEX service_metrics_service_ts ON service_metrics (service_id, ts);

-- Log chunks get a time so the Logs and Observability tabs can show when a line was written.
ALTER TABLE deployment_logs ADD COLUMN created_at timestamptz NOT NULL DEFAULT now();
CREATE INDEX deployment_logs_created_idx ON deployment_logs (created_at);
