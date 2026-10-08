import { timeAgo } from "./format";
import type { Deployment, DeploymentStatus, Server } from "./types";

export const IN_PROGRESS: DeploymentStatus[] = ["queued", "building", "deploying"];

export function isInProgress(d: Deployment | undefined): boolean {
  return !!d && IN_PROGRESS.includes(d.status);
}

/** Railway-style status line of a service card. */
export function statusLine(d: Deployment | undefined, now = Date.now()): string {
  if (!d) return "Not deployed";
  switch (d.status) {
    case "queued":
    case "building":
    case "deploying":
      return "Deploying...";
    case "running":
      return `Deployed ${timeAgo(d.updated_at, now)} via Docker Image`;
    case "failed":
      return "Deploy failed";
    case "stopped":
      return "Stopped";
  }
}

/** Public URL of a running deployment: the server host plus the forwarded port. */
export function serviceUrl(d: Deployment | undefined, servers: Server[]): string | null {
  if (!d || d.status !== "running" || d.host_port == null) return null;
  const host = servers.find((s) => s.id === d.server_id)?.hostname;
  return host ? `http://${host}:${d.host_port}` : null;
}

/** "KEY=value" lines to a record, and back. */
export function parseEnv(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const i = line.indexOf("=");
    if (i > 0) out[line.slice(0, i).trim()] = line.slice(i + 1);
  }
  return out;
}

export function formatEnv(env: Record<string, string>): string {
  return Object.entries(env).map(([k, v]) => `${k}=${v}`).join("\n");
}
