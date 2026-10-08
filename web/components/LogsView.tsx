"use client";

import { Suspense, useEffect, useMemo, useRef, useState } from "react";
import { api } from "@/lib/api";
import { useCurrentEnvironment } from "@/lib/environment";
import { formatLogTime, toLines } from "@/lib/logs";
import type { EnvironmentLogs, LogChunk, Service } from "@/lib/types";

const POLL_MS = 2000;
const INITIAL_CHUNKS = 500;
const MAX_CHUNKS = 2000;

function Logs({ projectId }: { projectId: string }) {
  const { env, error: envError } = useCurrentEnvironment(projectId);
  const [services, setServices] = useState<Service[]>([]);
  const [service, setService] = useState("");
  const [chunks, setChunks] = useState<LogChunk[]>([]);
  const [live, setLive] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const cursor = useRef(0);
  const box = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const envId = env?.id;

  useEffect(() => {
    if (!envId) return;
    api<Service[]>(`/environments/${envId}/services`).then(setServices).catch(() => setServices([]));
  }, [envId]);

  // Load the latest lines, then tail what comes after the cursor.
  useEffect(() => {
    if (!envId) return;
    let stopped = false;
    cursor.current = 0;
    setChunks([]);
    setLoaded(false);
    const filter = service ? `&service=${service}` : "";
    async function tick(first: boolean) {
      try {
        const query = first ? `limit=${INITIAL_CHUNKS}` : `after=${cursor.current}&limit=${INITIAL_CHUNKS}`;
        const res = await api<EnvironmentLogs>(`/environments/${envId}/logs?${query}${filter}`);
        if (stopped) return;
        cursor.current = res.next_after;
        if (first) setChunks(res.chunks);
        else if (res.chunks.length > 0) setChunks((old) => [...old, ...res.chunks].slice(-MAX_CHUNKS));
        setLoaded(true);
        setError(null);
      } catch (err) {
        if (!stopped) setError(err instanceof Error ? err.message : "Could not load logs");
      }
    }
    tick(true);
    const poll = live ? setInterval(() => tick(false), POLL_MS) : undefined;
    return () => {
      stopped = true;
      clearInterval(poll);
    };
  }, [envId, service, live]);

  const lines = useMemo(() => toLines(chunks), [chunks]);

  useEffect(() => {
    const el = box.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [lines]);

  return (
    <section className="obs">
      <div className="obs-bar">
        <label className="inline">
          <span className="sr-only">Service</span>
          <select value={service} onChange={(e) => setService(e.target.value)} aria-label="Filter by service">
            <option value="">All services</option>
            {services.map((s) => (
              <option key={s.id} value={s.id}>{s.name}</option>
            ))}
          </select>
        </label>
        <button type="button" className={`btn${live ? " primary" : ""}`} onClick={() => setLive((l) => !l)} aria-pressed={live}>
          {live ? "● Live" : "Paused"}
        </button>
      </div>
      {(error ?? envError) && <p role="alert" className="error">{error ?? envError}</p>}
      <div
        className="card logbox"
        ref={box}
        role="log"
        aria-live="off"
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        }}
      >
        {lines.length === 0 && (
          <p className="muted logempty">{loaded ? "No logs yet. Output of the running services shows up here." : "Loading..."}</p>
        )}
        {lines.map((l) => (
          <div key={l.key} className={`logline${l.error ? " err" : ""}`}>
            <time>{formatLogTime(l.ts)}</time>
            <span className="logsvc">{l.service}</span>
            <span className="logtext">{l.text}</span>
          </div>
        ))}
      </div>
    </section>
  );
}

export function LogsView({ projectId }: { projectId: string }) {
  return (
    <Suspense fallback={null}>
      <Logs projectId={projectId} />
    </Suspense>
  );
}
