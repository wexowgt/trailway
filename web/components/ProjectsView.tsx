"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useEffect, useState } from "react";
import { api } from "@/lib/api";
import { timeAgo } from "@/lib/format";
import type { Project } from "@/lib/types";

export function ProjectsView() {
  const router = useRouter();
  const [projects, setProjects] = useState<Project[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api<Project[]>("/projects")
      .then(setProjects)
      .catch((e: Error) => setError(e.message));
  }, []);

  async function create(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const name = String(new FormData(e.currentTarget).get("name") ?? "").trim();
    setBusy(true);
    setError(null);
    try {
      const p = await api<Project>("/projects", { method: "POST", body: JSON.stringify({ name }) });
      // Every project starts with a production environment, like Railway.
      await api(`/projects/${p.id}/environments`, { method: "POST", body: JSON.stringify({ name: "production" }) });
      router.push(`/projects/${p.id}`);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not create project");
      setBusy(false);
    }
  }

  return (
    <>
      <div className="page-head">
        <div>
          <h1>Projects</h1>
          <p className="muted">Each project has its own environments and services.</p>
        </div>
      </div>
      <form className="card new-project" onSubmit={create}>
        <label>
          New project
          <input name="name" required maxLength={100} placeholder="e.g. usable-spoon" />
        </label>
        <button className="btn primary" disabled={busy}>{busy ? "Creating..." : "Create project"}</button>
      </form>
      {error && <p role="alert" className="error">{error}</p>}
      {projects === null ? (
        <p className="muted">Loading...</p>
      ) : projects.length === 0 ? (
        <div className="card empty">
          <h2>No projects yet</h2>
          <p className="muted">Create one above to get a canvas for your services.</p>
        </div>
      ) : (
        <ul className="grid">
          {projects.map((p) => (
            <li key={p.id}>
              <Link href={`/projects/${p.id}`} className="card project-link">
                <strong>{p.name}</strong>
                <span className="muted">Created {timeAgo(p.created_at)}</span>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
