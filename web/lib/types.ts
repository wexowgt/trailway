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
