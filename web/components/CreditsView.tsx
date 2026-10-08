"use client";

import { useEffect, useState } from "react";
import { api } from "@/lib/api";
import { formatHours } from "@/lib/format";
import type { Compute, LedgerBalance, ServerLedger } from "@/lib/types";

const POLL_MS = 10_000;

/** vCPU-h and GB-h side by side: the ledger keeps the two units apart. */
function Hours({ c }: { c: Compute }) {
  return (
    <>
      <span className="num">{formatHours(c.vcpu_seconds)}</span> <span className="muted">vCPU-h</span>
      <br />
      <span className="num">{formatHours(c.gb_seconds)}</span> <span className="muted">GB-h</span>
    </>
  );
}

export function CreditsView() {
  const [balance, setBalance] = useState<LedgerBalance | null>(null);
  const [servers, setServers] = useState<ServerLedger[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let stopped = false;
    async function load() {
      try {
        const [b, s] = await Promise.all([
          api<LedgerBalance>("/ledger/balance"),
          api<ServerLedger[]>("/ledger/servers"),
        ]);
        if (stopped) return;
        setBalance(b);
        setServers(s);
        setError(null);
      } catch (err) {
        if (!stopped) setError(err instanceof Error ? err.message : "Could not load credits");
      }
    }
    load();
    const poll = setInterval(load, POLL_MS);
    return () => {
      stopped = true;
      clearInterval(poll);
    };
  }, []);

  const cards: { label: string; hint: string; value: Compute | undefined }[] = [
    { label: "Contributed", hint: "Idle capacity your servers offered", value: balance?.contributed },
    { label: "Consumed", hint: "Capacity you borrowed", value: balance?.consumed },
    { label: "Balance", hint: "Contributed minus consumed", value: balance?.balance },
  ];

  return (
    <>
      <header className="page-head">
        <div>
          <h1>Credits</h1>
          <p className="muted">Earned 1:1 for idle capacity your servers offer to the pool.</p>
        </div>
      </header>
      {error && <p role="alert" className="error">{error}</p>}
      <div className="credit-cards">
        {cards.map((c) => (
          <article key={c.label} className="card credit">
            <h2>{c.label}</h2>
            <p className="credit-value">{c.value ? <Hours c={c.value} /> : "..."}</p>
            <p className="muted">{c.hint}</p>
          </article>
        ))}
      </div>
      <article className="card errlogs">
        <h2>Per server</h2>
        <table>
          <thead>
            <tr><th>Server</th><th>Contributed</th><th>Consumed</th><th>Balance</th></tr>
          </thead>
          <tbody>
            {servers?.length === 0 && (
              <tr><td colSpan={4} className="muted">No credits yet. Connect a server and leave it online.</td></tr>
            )}
            {servers?.map((s) => (
              <tr key={s.server_id}>
                <td>{s.hostname ?? "Removed server"}</td>
                <td><Hours c={s.contributed} /></td>
                <td><Hours c={s.consumed} /></td>
                <td><Hours c={s.balance} /></td>
              </tr>
            ))}
          </tbody>
        </table>
      </article>
    </>
  );
}
