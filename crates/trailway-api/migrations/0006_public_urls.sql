-- Public HTTPS URL per service: <host_label>.<base domain>. The label is
-- fixed when the service is created (or moved), so the URL survives deploys
-- and renames. The agent reports its public IP with every heartbeat.
ALTER TABLE servers ADD COLUMN public_ip text;
ALTER TABLE services ADD COLUMN host_label text;
ALTER TABLE deployments ADD COLUMN domain text;

WITH labels AS (
    SELECT s.id,
           s.server_id,
           COALESCE(NULLIF(left(trim(both '-' from lower(regexp_replace(
               s.name || '-' || e.name, '[^a-zA-Z0-9]+', '-', 'g'))), 50), ''), 'service') AS label
    FROM services s JOIN environments e ON e.id = s.environment_id
), ranked AS (
    SELECT id, label, row_number() OVER (PARTITION BY server_id, label ORDER BY id) AS rn
    FROM labels
)
UPDATE services s
SET host_label = CASE WHEN r.rn > 1 THEN r.label || '-' || substr(s.id::text, 1, 6) ELSE r.label END
FROM ranked r WHERE r.id = s.id;

ALTER TABLE services ALTER COLUMN host_label SET NOT NULL;
CREATE UNIQUE INDEX services_server_label_key ON services (server_id, host_label);
