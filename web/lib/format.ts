const UNITS = ["B", "KB", "MB", "GB", "TB"];

export function formatBytes(bytes: number): string {
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const digits = value >= 100 || unit === 0 ? 0 : 1;
  return `${value.toFixed(digits)} ${UNITS[unit]}`;
}

export function formatCores(millicores: number): string {
  return (millicores / 1000).toFixed(millicores % 1000 === 0 ? 0 : 2);
}

export function percent(used: number, total: number): number {
  return total > 0 ? Math.min(100, Math.round((used / total) * 100)) : 0;
}

export function timeAgo(iso: string | null, now = Date.now()): string {
  if (!iso) return "never";
  const secs = Math.max(0, Math.round((now - new Date(iso).getTime()) / 1000));
  if (secs < 10) return "just now";
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`;
  return `${Math.floor(secs / 86400)}d ago`;
}

export function installCommand(origin: string, secret: string): string {
  return `curl -fsSL ${origin}/install.sh | sudo sh -s -- --key ${secret} --api ${origin}`;
}

const SECONDS_PER_HOUR = 3600;

/** Ledger seconds as hours, for readability: "12.50". */
export function formatHours(seconds: number): string {
  return (seconds / SECONDS_PER_HOUR).toFixed(2);
}
