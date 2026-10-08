import { formatBytes, formatCores, percent, timeAgo } from "@/lib/format";
import type { Resource, Server } from "@/lib/types";

function Meter({ label, res, fmt }: { label: string; res: Resource; fmt: (n: number) => string }) {
  const pct = percent(res.used, res.total);
  return (
    <div className="meter">
      <div className="meter-head">
        <span>{label}</span>
        <span className="muted">
          {fmt(res.used)} / {fmt(res.total)}
        </span>
      </div>
      <div
        className="bar"
        role="progressbar"
        aria-label={`${label} used`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct}
      >
        <div className="bar-fill" style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

export function ServerCard({ server, now }: { server: Server; now: number }) {
  const online = server.status === "online";
  return (
    <article className="card server">
      <header className="server-head">
        <span className={`dot ${online ? "on" : "off"}`} aria-hidden="true" />
        <h2>{server.hostname}</h2>
        <span className="status">{online ? "Online" : "Offline"}</span>
      </header>
      <Meter label="CPU" res={server.cpu} fmt={(n) => `${formatCores(n)} cores`} />
      <Meter label="RAM" res={server.memory} fmt={formatBytes} />
      <Meter label="Disk" res={server.disk} fmt={formatBytes} />
      <footer className="server-foot muted">
        <span className={`pill ${server.kvm ? "ok" : "bad"}`}>KVM {server.kvm ? "ready" : "missing"}</span>
        <span>Last seen {timeAgo(server.last_heartbeat_at, now)}</span>
      </footer>
    </article>
  );
}
