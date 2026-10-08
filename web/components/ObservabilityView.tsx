"use client";

import { Suspense, useEffect, useMemo, useState } from "react";
import { api } from "@/lib/api";
import { useCurrentEnvironment } from "@/lib/environment";
import { formatLogTime, toLines } from "@/lib/logs";
import type { EnvironmentLogs, EnvironmentMetrics, MetricsRange } from "@/lib/types";
import { UsageChart } from "./UsageChart";

const POLL_MS = 10_000;
const GIB = 1024 ** 3;
const MAX_ERROR_ROWS = 100;
const RANGES: { value: MetricsRange; label: string; ms: number }[] = [
  { value: "1h", label: "Last hour", ms: 3_600_000 },
  { value: "24h", label: "Last 24 hours", ms: 86_400_000 },
  { value: "7d", label: "Last 7 days", ms: 7 * 86_400_000 },
];

function Observability({ projectId }: { projectId: string }) {
  const { env, error: envError } = useCurrentEnvironment(projectId);
  const [range, setRange] = useState<MetricsRange>("1h");
  const [metrics, setMetrics] = useState<EnvironmentMetrics | null>(null);
  const [logs, setLogs] = useState<EnvironmentLogs | null>(null);
  const [error, setError] = useState<string | null>(null);
  const envId = env?.id;

  useEffect(() => {
    if (!envId) return;
    let stopped = false;
    const ms = RANGES.find((r) => r.value === range)?.ms ?? 0;
    async function load() {
      try {
        const since = new Date(Date.now() - ms).toISOString();
        const [m, l] = await Promise.all([
          api<EnvironmentMetrics>(`/environments/${envId}/metrics?range=${range}`),
          api<EnvironmentLogs>(`/environments/${envId}/logs?limit=1000&since=${encodeURIComponent(since)}`),
        ]);
        if (stopped) return;
        setMetrics(m);
        setLogs(l);
        setError(null);
      } catch (err) {
        if (!stopped) setError(err instanceof Error ? err.message : "Could not load observability data");
      }
    }
    load();
    const poll = setInterval(load, POLL_MS);
    return () => {
      stopped = true;
      clearInterval(poll);
    };
  }, [envId, range]);

  const errors = useMemo(
    () => toLines(logs?.chunks ?? []).filter((l) => l.error).slice(-MAX_ERROR_ROWS),
    [logs],
  );
  const series = metrics?.services ?? [];
  const from = metrics ? new Date(metrics.from).getTime() : 0;
  const to = metrics ? new Date(metrics.to).getTime() : 0;

  return (
    <section className="obs">
      <div className="obs-bar">
        <label className="inline">
          <span className="sr-only">Time range</span>
          <select value={range} onChange={(e) => setRange(e.target.value as MetricsRange)} aria-label="Time range">
            {RANGES.map((r) => (
              <option key={r.value} value={r.value}>{r.label}</option>
            ))}
          </select>
        </label>
      </div>
      {(error ?? envError) && <p role="alert" className="error">{error ?? envError}</p>}

      <article className="card errlogs">
        <h2>Error logs</h2>
        <table>
          <thead>
            <tr><th>Date</th><th>Service</th><th>Message</th></tr>
          </thead>
          <tbody>
            {errors.length === 0 && (
              <tr><td colSpan={3} className="muted">{logs ? "No errors in this range." : "Loading..."}</td></tr>
            )}
            {errors.map((l) => (
              <tr key={l.key} className="errrow">
                <td>{formatLogTime(l.ts)}</td>
                <td>{l.service}</td>
                <td className="logtext">{l.text}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </article>

      <div className="charts">
        <UsageChart
          title="CPU Usage"
          unit="vCPU"
          series={series}
          range={range}
          from={from}
          to={to}
          value={(p) => p.cpu_millicores / 1000}
          limit={(s) => s.cpu_limit_millicores / 1000}
          digits={2}
        />
        <UsageChart
          title="Memory Usage"
          unit="GB"
          series={series}
          range={range}
          from={from}
          to={to}
          value={(p) => p.memory_bytes / GIB}
          limit={(s) => s.memory_limit_bytes / GIB}
          digits={2}
        />
      </div>
    </section>
  );
}

export function ObservabilityView({ projectId }: { projectId: string }) {
  return (
    <Suspense fallback={null}>
      <Observability projectId={projectId} />
    </Suspense>
  );
}
