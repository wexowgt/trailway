"use client";

import { useState } from "react";
import { api } from "@/lib/api";
import type { ServerKey } from "@/lib/types";

export function KeyList({ keys, onChanged }: { keys: ServerKey[]; onChanged: () => void }) {
  const [confirming, setConfirming] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  if (keys.length === 0) return null;

  async function revoke(id: string) {
    try {
      await api(`/server-keys/${id}`, { method: "DELETE" });
      setConfirming(null);
      setError(null);
      onChanged();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not revoke key");
    }
  }

  return (
    <section className="keys" aria-labelledby="keys-title">
      <h2 id="keys-title">Server keys</h2>
      {error && <p role="alert" className="error">{error}</p>}
      <ul className="card key-list">
        {keys.map((k) => (
          <li key={k.id} className="key-row">
            <div>
              <strong>{k.name}</strong>
              <code className="muted"> {k.prefix}...</code>
            </div>
            {k.revoked_at ? (
              <span className="pill bad">Revoked</span>
            ) : confirming === k.id ? (
              <span className="row">
                <button type="button" className="btn danger" onClick={() => revoke(k.id)}>
                  Confirm revoke
                </button>
                <button type="button" className="btn" onClick={() => setConfirming(null)}>
                  Cancel
                </button>
              </span>
            ) : (
              <button
                type="button"
                className="btn"
                aria-label={`Revoke key ${k.name}`}
                onClick={() => setConfirming(k.id)}
              >
                Revoke
              </button>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
