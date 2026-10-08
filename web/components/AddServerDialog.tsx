"use client";

import { useEffect, useRef, useState } from "react";
import { api } from "@/lib/api";
import { installCommand } from "@/lib/format";
import type { CreatedServerKey } from "@/lib/types";
import { CopyButton } from "./CopyButton";

export function AddServerDialog({ onClose }: { onClose: () => void }) {
  const ref = useRef<HTMLDialogElement>(null);
  const [created, setCreated] = useState<CreatedServerKey | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    ref.current?.showModal();
  }, []);

  async function create(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const name = String(new FormData(e.currentTarget).get("name") ?? "").trim();
    setBusy(true);
    setError(null);
    try {
      setCreated(await api<CreatedServerKey>("/server-keys", {
        method: "POST",
        body: JSON.stringify({ name }),
      }));
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not create key");
    } finally {
      setBusy(false);
    }
  }

  const command = created ? installCommand(window.location.origin, created.secret) : "";

  return (
    <dialog ref={ref} className="dialog" aria-labelledby="add-title" onClose={onClose}>
      <div className="dialog-body">
        <h2 id="add-title">Add server</h2>
        {created ? (
          <>
            <p className="muted">
              Run this on your Linux server as a user with sudo. The key is shown only once, so copy it now.
            </p>
            <pre className="command" tabIndex={0}>{command}</pre>
            <div className="row end">
              <CopyButton text={command} label="Copy command" />
              <button type="button" className="btn primary" onClick={() => ref.current?.close()}>
                Done
              </button>
            </div>
          </>
        ) : (
          <form onSubmit={create}>
            <label>
              Name
              <input name="name" required maxLength={100} placeholder="e.g. hetzner-1" autoFocus />
            </label>
            {error && <p role="alert" className="error">{error}</p>}
            <div className="row end">
              <button type="button" className="btn" onClick={() => ref.current?.close()}>
                Cancel
              </button>
              <button className="btn primary" disabled={busy}>
                {busy ? "Creating..." : "Create key"}
              </button>
            </div>
          </form>
        )}
      </div>
    </dialog>
  );
}
