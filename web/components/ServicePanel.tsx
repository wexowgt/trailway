"use client";

import { useEffect, useState } from "react";
import { api } from "@/lib/api";
import { timeAgo } from "@/lib/format";
import { formatEnv, isInProgress, parseEnv, serviceUrl } from "@/lib/service";
import type { Deployment, Server, Service } from "@/lib/types";

type Props = {
  service: Service;
  deployments: Deployment[];
  servers: Server[];
  onClose: () => void;
  onChanged: () => void;
};

export function ServicePanel({ service, deployments, servers, onClose, onChanged }: Props) {
  const [envText, setEnvText] = useState(formatEnv(service.env));
  const [vcpus, setVcpus] = useState(service.vcpus);
  const [memory, setMemory] = useState(service.memory_mib);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const latest = deployments[0];
  const url = serviceUrl(latest, servers);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  async function run(label: string, fn: () => Promise<unknown>) {
    setBusy(label);
    setError(null);
    try {
      await fn();
      onChanged();
    } catch (err) {
      setError(err instanceof Error ? err.message : `${label} failed`);
    } finally {
      setBusy(null);
    }
  }

  const save = () =>
    run("Save", () =>
      api(`/services/${service.id}`, {
        method: "PATCH",
        body: JSON.stringify({ env: parseEnv(envText), vcpus, memory_mib: memory }),
      }),
    );
  const redeploy = () =>
    run("Redeploy", async () => {
      await api(`/services/${service.id}`, {
        method: "PATCH",
        body: JSON.stringify({ env: parseEnv(envText), vcpus, memory_mib: memory }),
      });
      await api(`/services/${service.id}/deploy`, { method: "POST" });
    });
  const stop = () => run("Stop", () => api(`/services/${service.id}/stop`, { method: "POST" }));
  const remove = () =>
    run("Delete", async () => {
      await api(`/services/${service.id}/stop`, { method: "POST" });
      await api(`/services/${service.id}`, { method: "DELETE" });
      onClose();
    });

  return (
    <aside className="panel" aria-label={`${service.name} service`}>
      <div className="panel-head">
        <h2>{service.name}</h2>
        <button type="button" className="btn" onClick={onClose} aria-label="Close panel">Close</button>
      </div>
      <p className="muted">{service.image}</p>
      {url && <a href={url} target="_blank" rel="noreferrer" className="svc-url">{url.replace(/^https?:\/\//, "")}</a>}

      <div className="row">
        <button type="button" className="btn primary" onClick={redeploy} disabled={!!busy}>
          {busy === "Redeploy" ? "Redeploying..." : latest ? "Redeploy" : "Deploy"}
        </button>
        <button type="button" className="btn" onClick={stop} disabled={!!busy || !latest || latest.status === "stopped"}>
          Stop
        </button>
        <button type="button" className="btn danger" onClick={remove} disabled={!!busy}>Delete</button>
      </div>
      {error && <p role="alert" className="error">{error}</p>}

      <h3>Resources</h3>
      <div className="row">
        <label>
          vCPUs
          <input type="number" min={1} max={32} value={vcpus} onChange={(e) => setVcpus(Number(e.target.value))} />
        </label>
        <label>
          RAM (MB)
          <input type="number" min={64} value={memory} onChange={(e) => setMemory(Number(e.target.value))} />
        </label>
      </div>

      <h3>Variables</h3>
      <label>
        <span className="sr-only">Environment variables</span>
        <textarea rows={5} spellCheck={false} value={envText} onChange={(e) => setEnvText(e.target.value)} placeholder="KEY=value" />
      </label>
      <div className="row">
        <button type="button" className="btn" onClick={save} disabled={!!busy}>
          {busy === "Save" ? "Saving..." : "Save"}
        </button>
        <small>Applies on the next deploy.</small>
      </div>

      <h3>Deployments</h3>
      {deployments.length === 0 ? (
        <p className="muted">No deployments yet.</p>
      ) : (
        <ul className="history">
          {deployments.map((d) => (
            <li key={d.id}>
              <span className={`pill ${d.status}`}>{isInProgress(d) ? `${d.status}...` : d.status}</span>
              <span className="muted">{timeAgo(d.created_at)}</span>
              {d.error && <span className="error">{d.error}</span>}
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}
