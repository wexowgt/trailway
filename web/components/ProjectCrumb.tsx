"use client";

import { useRouter, useSearchParams } from "next/navigation";
import { useEffect, useRef, useState } from "react";
import { api } from "@/lib/api";
import type { Environment, Project } from "@/lib/types";

/** Breadcrumb tail on project pages: project name and the environment switcher. */
export function ProjectCrumb({ projectId }: { projectId: string }) {
  const router = useRouter();
  const params = useSearchParams();
  const [project, setProject] = useState<Project | null>(null);
  const [envs, setEnvs] = useState<Environment[]>([]);
  const [open, setOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    api<Project>(`/projects/${projectId}`).then(setProject).catch(() => setProject(null));
    api<Environment[]>(`/projects/${projectId}/environments`).then(setEnvs).catch(() => setEnvs([]));
  }, [projectId]);

  const current = envs.find((e) => e.id === params.get("env")) ?? envs[0];

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  function pick(id: string) {
    setOpen(false);
    router.push(`/projects/${projectId}?env=${id}`);
  }

  async function createEnv(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const name = String(new FormData(e.currentTarget).get("name") ?? "").trim();
    try {
      const env = await api<Environment>(`/projects/${projectId}/environments`, {
        method: "POST",
        body: JSON.stringify({ name }),
      });
      setEnvs((list) => [...list, env]);
      setCreating(false);
      setError(null);
      pick(env.id);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not create environment");
    }
  }

  return (
    <>
      <span className="sep" aria-hidden="true">/</span>
      <span>{project?.name ?? "..."}</span>
      <span className="sep" aria-hidden="true">/</span>
      <div className="menu" ref={root} onKeyDown={(e) => e.key === "Escape" && setOpen(false)}>
        <button
          type="button"
          className="env-btn"
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={() => setOpen((o) => !o)}
        >
          {current?.name ?? "..."} <span aria-hidden="true">&#9662;</span>
        </button>
        {open && (
          <div className="menu-panel env-panel" role="menu">
            {envs.map((env) => (
              <button
                key={env.id}
                type="button"
                role="menuitemradio"
                aria-checked={env.id === current?.id}
                className="menu-item"
                onClick={() => pick(env.id)}
              >
                {env.id === current?.id ? "✓ " : ""}{env.name}
              </button>
            ))}
            {creating ? (
              <form onSubmit={createEnv} className="env-new">
                <input name="name" required maxLength={100} placeholder="Environment name" autoFocus aria-label="Environment name" />
                <button className="btn primary">Add</button>
                {error && <p role="alert" className="error">{error}</p>}
              </form>
            ) : (
              <button type="button" role="menuitem" className="menu-item" onClick={() => setCreating(true)}>
                + New environment
              </button>
            )}
          </div>
        )}
      </div>
    </>
  );
}
