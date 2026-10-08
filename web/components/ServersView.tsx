"use client";

import { useCallback, useEffect, useState } from "react";
import { api } from "@/lib/api";
import type { Server, ServerKey } from "@/lib/types";
import { AddServerDialog } from "./AddServerDialog";
import { KeyList } from "./KeyList";
import { ServerCard } from "./ServerCard";

const POLL_MS = 5000;

export function ServersView() {
  const [servers, setServers] = useState<Server[] | null>(null);
  const [keys, setKeys] = useState<ServerKey[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  const load = useCallback(async () => {
    try {
      const [s, k] = await Promise.all([
        api<Server[]>("/servers"),
        api<ServerKey[]>("/server-keys"),
      ]);
      setServers(s);
      setKeys(k);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not load servers");
    }
  }, []);

  useEffect(() => {
    const first = setTimeout(load, 0);
    const poll = setInterval(() => {
      load();
      setNow(Date.now());
    }, POLL_MS);
    return () => {
      clearTimeout(first);
      clearInterval(poll);
    };
  }, [load]);

  return (
    <>
      <div className="page-head">
        <div>
          <h1>Servers</h1>
          <p className="muted">Machines in your pool. Updates every few seconds.</p>
        </div>
        <button type="button" className="btn primary" onClick={() => setAdding(true)}>
          + Add server
        </button>
      </div>

      {error && <p role="alert" className="error">{error}</p>}

      {servers === null ? (
        <p className="muted">Loading...</p>
      ) : servers.length === 0 ? (
        <div className="card empty">
          <h2>No servers yet</h2>
          <p className="muted">Add a server to get a one-line install command for your Linux host.</p>
        </div>
      ) : (
        <ul className="grid">
          {servers.map((s) => (
            <li key={s.id}>
              <ServerCard server={s} now={now} />
            </li>
          ))}
        </ul>
      )}

      <KeyList keys={keys} onChanged={load} />

      {adding && (
        <AddServerDialog
          onClose={() => {
            setAdding(false);
            load();
          }}
        />
      )}
    </>
  );
}
