"use client";

import {
  CartesianGrid,
  Line,
  LineChart,
  ReferenceLine,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import type { MetricsRange, ServiceSeries } from "@/lib/types";

export const SERIES_COLORS = ["#6366f1", "#4ade80", "#f472b6", "#fbbf24", "#38bdf8", "#fb923c"];
const GRID = "#23233a";
const MUTED = "#8b8ba3";

type Row = { ts: number } & Record<string, number>;

/** One row per timestamp with a column per service, in the chart's unit. */
export function toRows(series: ServiceSeries[], pick: (p: ServiceSeries["points"][number]) => number): Row[] {
  const rows = new Map<number, Row>();
  for (const s of series) {
    for (const p of s.points) {
      const ts = new Date(p.ts).getTime();
      const row = rows.get(ts) ?? ({ ts } as Row);
      row[s.service_id] = pick(p);
      rows.set(ts, row);
    }
  }
  return [...rows.values()].sort((a, b) => a.ts - b.ts);
}

function tickLabel(ts: number, range: MetricsRange): string {
  const d = new Date(ts);
  return range === "7d"
    ? d.toLocaleDateString(undefined, { month: "short", day: "numeric" })
    : d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", hour12: false });
}

type Props = {
  title: string;
  unit: string;
  series: ServiceSeries[];
  range: MetricsRange;
  from: number;
  to: number;
  value: (p: ServiceSeries["points"][number]) => number;
  limit: (s: ServiceSeries) => number;
  digits: number;
};

export function UsageChart({ title, unit, series, range, from, to, value, limit, digits }: Props) {
  const rows = toRows(series, value);
  const fmt = (n: number) => `${n.toFixed(digits)} ${unit}`;
  return (
    <article className="card chart">
      <h2>{title}</h2>
      {rows.length === 0 ? (
        <p className="muted chart-empty">No data in this range yet.</p>
      ) : (
        <div className="chart-box" role="img" aria-label={`${title} per service over time`}>
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={rows} margin={{ top: 8, right: 12, bottom: 0, left: 0 }}>
              <CartesianGrid stroke={GRID} strokeDasharray="2 4" vertical={false} />
              <XAxis
                dataKey="ts"
                type="number"
                scale="time"
                domain={[from, to]}
                tickFormatter={(t: number) => tickLabel(t, range)}
                stroke={MUTED}
                tick={{ fontSize: 11 }}
                tickLine={false}
                axisLine={{ stroke: GRID }}
              />
              <YAxis
                domain={[0, "auto"]}
                tickFormatter={(n: number) => fmt(n)}
                stroke={MUTED}
                tick={{ fontSize: 11 }}
                tickLine={false}
                axisLine={false}
                width={76}
              />
              <Tooltip
                contentStyle={{ background: "#13131d", border: `1px solid ${GRID}`, borderRadius: 8 }}
                labelFormatter={(t) => new Date(Number(t)).toLocaleString()}
                formatter={(v, name) => [fmt(Number(v)), String(name)]}
              />
              {series.map((s, i) => (
                <ReferenceLine
                  key={`limit-${s.service_id}`}
                  y={limit(s)}
                  stroke={SERIES_COLORS[i % SERIES_COLORS.length]}
                  strokeDasharray="2 4"
                  strokeOpacity={0.7}
                  ifOverflow="extendDomain"
                />
              ))}
              {series.map((s, i) => (
                <Line
                  key={s.service_id}
                  name={s.name}
                  dataKey={s.service_id}
                  type="stepAfter"
                  stroke={SERIES_COLORS[i % SERIES_COLORS.length]}
                  strokeWidth={1.75}
                  dot={false}
                  isAnimationActive={false}
                  connectNulls
                />
              ))}
            </LineChart>
          </ResponsiveContainer>
        </div>
      )}
      <ul className="legend">
        {series.map((s, i) => (
          <li key={s.service_id}>
            <span className="swatch" style={{ background: SERIES_COLORS[i % SERIES_COLORS.length] }} aria-hidden="true" />
            {s.name} <span className="muted">limit {fmt(limit(s))}</span>
          </li>
        ))}
      </ul>
    </article>
  );
}
