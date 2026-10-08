// Mirrors crates/trailway-proto. CPU is millicores, memory and disk are bytes.
export type Resource = { total: number; used: number };

export type User = { id: string; email: string; created_at: string };

export type Server = {
  id: string;
  hostname: string;
  status: "online" | "offline";
  cpu: Resource;
  memory: Resource;
  disk: Resource;
  kvm: boolean;
  agent_version: string;
  last_heartbeat_at: string | null;
  created_at: string;
};

export type ServerKey = {
  id: string;
  name: string;
  prefix: string;
  created_at: string;
  revoked_at: string | null;
};

export type CreatedServerKey = ServerKey & { secret: string };

export type Project = { id: string; name: string; created_at: string };
export type Environment = { id: string; project_id: string; name: string; created_at: string };

export type DeploymentStatus = "queued" | "building" | "deploying" | "running" | "failed" | "stopped";

export type Service = {
  id: string;
  environment_id: string;
  name: string;
  image: string;
  vcpus: number;
  memory_mib: number;
  env: Record<string, string>;
  port: number | null;
  server_id: string;
  created_at: string;
  updated_at: string;
};

export type Deployment = {
  id: string;
  service_id: string | null;
  server_id: string;
  status: DeploymentStatus;
  image: string;
  vcpus: number;
  memory_mib: number;
  host_port: number | null;
  error: string | null;
  created_at: string;
  updated_at: string;
};

export type MetricPoint = { ts: string; cpu_millicores: number; memory_bytes: number };
export type ServiceSeries = {
  service_id: string;
  name: string;
  cpu_limit_millicores: number;
  memory_limit_bytes: number;
  points: MetricPoint[];
};
export type EnvironmentMetrics = {
  from: string;
  to: string;
  bucket_secs: number;
  services: ServiceSeries[];
};
export type MetricsRange = "1h" | "24h" | "7d";

export type LogChunk = { id: number; ts: string; service_id: string; service: string; text: string };
export type EnvironmentLogs = { chunks: LogChunk[]; next_after: number };

// Ledger compute is in seconds (vCPU-seconds and GB-seconds), never mixed.
export type Compute = { vcpu_seconds: number; gb_seconds: number };
export type LedgerBalance = { contributed: Compute; consumed: Compute; balance: Compute };
export type ServerLedger = {
  server_id: string;
  hostname: string | null;
  contributed: Compute;
  consumed: Compute;
  balance: Compute;
};
