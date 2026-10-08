"use client";

import { useEffect, useRef, useState } from "react";
import { api } from "@/lib/api";
import { parseEnv } from "@/lib/service";
import type { Deployment, Server, Service } from "@/lib/types";

type Props = {
  environmentId: string;
  servers: Server[];
  onClose: () => void;
  onCreated: (service: Service) => void;
};

export function CreateServiceDialog({ environmentId, servers, onClose, onCreated }: Props) {
  const ref = useRef<HTMLDialogElement>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const online = servers.filter((s) => s.status === "online");

  useEffect(() => {
    ref.current?.showModal();
  }, []);

  async function submit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const f = new FormData(e.currentTarget);
    const text = (k: string) => String(f.get(k) ?? "").trim();
    const port = text("port");
    setBusy(true);
    setError(null);
    try {
      const service = await api<Service>(`/environments/${environmentId}/services`, {
        method: "POST",
        body: JSON.stringify({
          name: text("name"),
          image: text("image"),
          server_id: text("server"),
          vcpus: Number(text("vcpus")),
          memory_mib: Number(text("memory")),
          port: port ? Number(port) : null,
          env: parseEnv(text("env")),
        }),
      });
      onCreated(service);
      await api<Deployment>(`/services/${service.id}/deploy`, { method: "POST" });
      ref.current?.close();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not create service");
      setBusy(false);
    }
  }

  return (
    <dialog ref={ref} className="dialog" aria-labelledby="create-title" onClose={onClose}>
      <form className="dialog-body" onSubmit={submit}>
        <h2 id="create-title">Create service</h2>
        <p className="muted">Deploy a Docker image to one of your servers.</p>
        <label>
          Docker image
          <input name="image" required placeholder="nginxdemos/hello" autoFocus />
        </label>
        <label>
          Name
          <input name="name" required maxLength={100} placeholder="hello" />
        </label>
        <label>
          Server
          <select name="server" required defaultValue={online[0]?.id ?? ""}>
            {servers.length === 0 && <option value="">No servers yet</option>}
            {servers.map((s) => (
              <option key={s.id} value={s.id} disabled={s.status !== "online"}>
                {s.hostname}{s.status !== "online" ? " (offline)" : ""}
              </option>
            ))}
          </select>
        </label>
        <div className="row">
          <label>
            vCPUs
            <input name="vcpus" type="number" min={1} max={32} defaultValue={1} required />
          </label>
          <label>
            Memory (MB)
            <input name="memory" type="number" min={64} defaultValue={256} required />
          </label>
          <label>
            Port
            <input name="port" type="number" min={1} max={65535} defaultValue={80} />
          </label>
        </div>
        <label>
          Environment variables (one KEY=value per line)
          <textarea name="env" rows={3} spellCheck={false} />
        </label>
        {error && <p role="alert" className="error">{error}</p>}
        <div className="row end">
          <button type="button" className="btn" onClick={() => ref.current?.close()}>Cancel</button>
          <button className="btn primary" disabled={busy || online.length === 0}>
            {busy ? "Deploying..." : "Create and deploy"}
          </button>
        </div>
      </form>
    </dialog>
  );
}
