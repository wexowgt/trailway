"use client";

import { Handle, Position, type Node, type NodeProps } from "@xyflow/react";
import type { Deployment, Service } from "@/lib/types";
import { isInProgress, statusLine } from "@/lib/service";

export type ServiceNodeData = {
  service: Service;
  deployment: Deployment | undefined;
  url: string | null;
  now: number;
};
export type ServiceFlowNode = Node<ServiceNodeData, "service">;

function StatusIcon({ d }: { d: Deployment | undefined }) {
  if (isInProgress(d)) return <span className="spin" aria-hidden="true" />;
  const kind = d?.status === "running" ? "ok" : d?.status === "failed" ? "bad" : "idle";
  return (
    <svg className={`status-icon ${kind}`} width="18" height="18" viewBox="0 0 18 18" aria-hidden="true">
      <circle cx="9" cy="9" r="7.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
      {kind === "ok" && <path d="M5.5 9.2l2.4 2.4 4.6-5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />}
      {kind === "bad" && <path d="M6.5 6.5l5 5M11.5 6.5l-5 5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />}
    </svg>
  );
}

export function ServiceNode({ data, selected }: NodeProps<ServiceFlowNode>) {
  const { service, deployment, url, now } = data;
  const hue = [...service.name].reduce((h, c) => (h * 31 + c.charCodeAt(0)) % 360, 7);
  return (
    <div className={`svc${selected ? " selected" : ""}`} aria-label={`${service.name}, ${statusLine(deployment, now)}`}>
      <Handle type="target" position={Position.Left} className="svc-handle" isConnectable={false} />
      <div className="svc-main">
        <div className="svc-head">
          <span className="svc-icon" style={{ background: `linear-gradient(135deg, hsl(${hue} 70% 62%), hsl(${(hue + 60) % 360} 65% 55%))` }} aria-hidden="true" />
          <span className="svc-name">{service.name}</span>
        </div>
        {url ? (
          <a className="svc-url nodrag" href={url} target="_blank" rel="noreferrer" onClick={(e) => e.stopPropagation()}>
            {url.replace(/^https?:\/\//, "")}
          </a>
        ) : (
          <span className="svc-url dim">{service.image}</span>
        )}
        <div className="svc-status">
          <StatusIcon d={deployment} />
          <span>{statusLine(deployment, now)}</span>
        </div>
      </div>
      <div className="svc-foot">
        <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
          <rect x="2.5" y="4.5" width="9" height="9" rx="1.5" fill="none" stroke="currentColor" />
          <path d="M5 4V3a1 1 0 011-1h7a1 1 0 011 1v7a1 1 0 01-1 1h-1" fill="none" stroke="currentColor" />
        </svg>
        <span>{service.vcpus} vCPU, {service.memory_mib} MB</span>
      </div>
      <Handle type="source" position={Position.Right} className="svc-handle" isConnectable={false} />
    </div>
  );
}
