"use client";

import { useSearchParams } from "next/navigation";
import { useEffect, useState } from "react";
import { api } from "./api";
import type { Environment } from "./types";

/** The environment picked in the breadcrumb (`?env=`), else the project's first. */
export function useCurrentEnvironment(projectId: string): { env: Environment | null; error: string | null } {
  const wanted = useSearchParams().get("env");
  const [envs, setEnvs] = useState<Environment[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api<Environment[]>(`/projects/${projectId}/environments`)
      .then(setEnvs)
      .catch((err) => setError(err instanceof Error ? err.message : "Could not load environments"));
  }, [projectId]);

  const env = envs ? (envs.find((e) => e.id === wanted) ?? envs[0] ?? null) : null;
  return { env, error };
}
