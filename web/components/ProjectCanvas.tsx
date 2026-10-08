"use client";

import {
  Background,
  BackgroundVariant,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  type NodeTypes,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "@/lib/api";
import { serviceUrl } from "@/lib/service";
import type { Deployment, Environment, Server, Service } from "@/lib/types";
import { CreateServiceDialog } from "./CreateServiceDialog";
import { ServiceNode, type ServiceFlowNode } from "./ServiceNode";
import { ServicePanel } from "./ServicePanel";

const POLL_MS = 3000;
const nodeTypes: NodeTypes = { service: ServiceNode };
type Positions = Record<string, { x: number; y: number }>;

const storageKey = (projectId: string) => `trailway:positions:${projectId}`;

function loadPositions(projectId: string): Positions {
  try {
    return JSON.parse(localStorage.getItem(storageKey(projectId)) ?? "{}") as Positions;
  } catch {
    return {};
  }
}

function savePositions(projectId: string, positions: Positions) {
  try {
    localStorage.setItem(storageKey(projectId), JSON.stringify(positions));
  } catch {
    // positions are a convenience; ignore blocked storage
  }
}

const defaultPosition = (i: number) => ({ x: 120 + (i % 3) * 360, y: 80 + Math.floor(i / 3) * 240 });

function Canvas({ projectId }: { projectId: string }) {
  const params = useSearchParams();
  const flow = useReactFlow();
  const [env, setEnv] = useState<Environment | null>(null);
  const [services, setServices] = useState<Service[]>([]);
  const [deployments, setDeployments] = useState<Record<string, Deployment[]>>({});
  const [servers, setServers] = useState<Server[]>([]);
  const [positions, setPositions] = useState<Positions>({});
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

  const envParam = params.get("env");

  useEffect(() => {
    setPositions(loadPositions(projectId));
  }, [projectId]);

  const load = useCallback(async () => {
    try {
      const envs = await api<Environment[]>(`/projects/${projectId}/environments`);
      const current = envs.find((e) => e.id === envParam) ?? envs[0] ?? null;
      setEnv(current);
      const [svcs, srvs] = await Promise.all([
        current ? api<Service[]>(`/environments/${current.id}/services`) : Promise.resolve([]),
        api<Server[]>("/servers"),
      ]);
      const lists = await Promise.all(
        svcs.map(async (s) => [s.id, await api<Deployment[]>(`/services/${s.id}/deployments`)] as const),
      );
      setServices(svcs);
      setServers(srvs);
      setDeployments(Object.fromEntries(lists));
      setNow(Date.now());
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not load project");
    }
  }, [projectId, envParam]);

  useEffect(() => {
    setServices([]);
    setSelected(null);
    const first = setTimeout(load, 0);
    const poll = setInterval(load, POLL_MS);
    return () => {
      clearTimeout(first);
      clearInterval(poll);
    };
  }, [load]);

  const nodes: ServiceFlowNode[] = useMemo(
    () =>
      services.map((s, i) => {
        const list = deployments[s.id] ?? [];
        return {
          id: s.id,
          type: "service",
          position: positions[s.id] ?? defaultPosition(i),
          selected: s.id === selected,
          data: { service: s, deployment: list[0], url: serviceUrl(list[0], servers), now },
        };
      }),
    [services, deployments, servers, positions, selected, now],
  );

  const selectedService = services.find((s) => s.id === selected);

  return (
    <div className="canvas-wrap">
      <ReactFlow
        nodes={nodes}
        edges={[]}
        nodeTypes={nodeTypes}
        fitView
        fitViewOptions={{ maxZoom: 1, padding: 0.3 }}
        minZoom={0.2}
        maxZoom={2}
        nodesConnectable={false}
        proOptions={{ hideAttribution: true }}
        onNodeClick={(_, n) => setSelected(n.id)}
        onPaneClick={() => setSelected(null)}
        onNodeDragStop={(_, n) => {
          const next = { ...positions, [n.id]: n.position };
          setPositions(next);
          savePositions(projectId, next);
        }}
      >
        <Background variant={BackgroundVariant.Dots} gap={24} size={1.5} color="#26263a" />
      </ReactFlow>

      <div className="zoom" role="group" aria-label="Zoom controls">
        <button type="button" onClick={() => flow.zoomIn()} aria-label="Zoom in">+</button>
        <button type="button" onClick={() => flow.zoomOut()} aria-label="Zoom out">&minus;</button>
        <button type="button" onClick={() => flow.fitView({ maxZoom: 1, padding: 0.3 })} aria-label="Fit view">&#9974;</button>
      </div>

      <div className="canvas-actions">
        <button type="button" className="btn" onClick={load}>&#8635; Sync</button>
        <button type="button" className="btn" onClick={() => setCreating(true)} disabled={!env}>+ Create</button>
      </div>

      {error && <p role="alert" className="error canvas-error">{error}</p>}
      {env && services.length === 0 && !error && (
        <div className="canvas-empty">
          <h2>No services in {env.name}</h2>
          <p className="muted">Press Create to deploy a Docker image.</p>
        </div>
      )}

      {selectedService && (
        <ServicePanel
          key={selectedService.id}
          service={selectedService}
          deployments={deployments[selectedService.id] ?? []}
          servers={servers}
          onClose={() => setSelected(null)}
          onChanged={load}
        />
      )}

      {creating && env && (
        <CreateServiceDialog
          environmentId={env.id}
          servers={servers}
          onClose={() => {
            setCreating(false);
            load();
          }}
          onCreated={(s) => setSelected(s.id)}
        />
      )}
    </div>
  );
}

export function ProjectCanvas({ projectId }: { projectId: string }) {
  return (
    <ReactFlowProvider>
      <Canvas projectId={projectId} />
    </ReactFlowProvider>
  );
}
