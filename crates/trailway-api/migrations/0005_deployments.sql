CREATE TABLE projects (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name       text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (user_id, name)
);

CREATE TABLE environments (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id uuid NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name       text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

CREATE TABLE services (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    environment_id uuid NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    name           text NOT NULL,
    image          text NOT NULL,
    vcpus          integer NOT NULL,
    memory_mib     integer NOT NULL,
    env            jsonb NOT NULL DEFAULT '{}',
    port           integer,
    server_id      uuid NOT NULL REFERENCES servers (id),
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (environment_id, name)
);

-- A deployment outlives its service (service_id is nulled) until the agent
-- confirms the VM is gone, so a delete while the agent is offline still
-- removes the VM once it reconnects.
CREATE TABLE deployments (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    service_id uuid REFERENCES services (id) ON DELETE SET NULL,
    server_id  uuid NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    status     text NOT NULL DEFAULT 'queued',
    -- 'running' while the deployment should exist, 'stopped' once it must go.
    desired    text NOT NULL DEFAULT 'running',
    spec       jsonb NOT NULL,
    vm_id      text,
    host_port  integer,
    error      text,
    log_bytes  bigint NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX deployments_service_idx ON deployments (service_id, created_at DESC);
CREATE INDEX deployments_server_idx ON deployments (server_id);

CREATE TABLE deployment_logs (
    id            bigserial PRIMARY KEY,
    deployment_id uuid NOT NULL REFERENCES deployments (id) ON DELETE CASCADE,
    text          text NOT NULL
);
CREATE INDEX deployment_logs_idx ON deployment_logs (deployment_id, id);
