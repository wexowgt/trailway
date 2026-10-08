"use client";

import Link from "next/link";
import { useParams, usePathname, useSearchParams } from "next/navigation";
import { Suspense } from "react";
import { AvatarMenu } from "./AvatarMenu";
import { Logo } from "./Logo";
import { ProjectCrumb } from "./ProjectCrumb";

export function TopBar({ email }: { email: string }) {
  const pathname = usePathname();
  const { id } = useParams<{ id?: string }>();
  const inProject = pathname.startsWith("/projects/") && !!id;
  const onServers = pathname.startsWith("/servers");
  return (
    <header className="topbar">
      <nav className="breadcrumb" aria-label="Breadcrumb">
        <Link href="/projects" aria-label="Trailway home">
          <Logo />
        </Link>
        {inProject ? (
          <Suspense fallback={null}>
            <ProjectCrumb projectId={id} />
          </Suspense>
        ) : (
          <>
            <span className="sep" aria-hidden="true">/</span>
            <span>Personal</span>
          </>
        )}
      </nav>
      <nav className="tabs" aria-label="Sections">
        {inProject && (
          <Suspense fallback={null}>
            <ProjectTab projectId={id} />
          </Suspense>
        )}
        <Link href="/projects" className={`tab${pathname === "/projects" ? " active" : ""}`}>Projects</Link>
        <Link href="/servers" className={`tab${onServers ? " active" : ""}`}>Servers</Link>
        <Link href="/credits" className={`tab${pathname.startsWith("/credits") ? " active" : ""}`}>Credits</Link>
        <AvatarMenu email={email} />
      </nav>
    </header>
  );
}

const PROJECT_TABS = [
  { path: "", label: "Architecture" },
  { path: "/observability", label: "Observability" },
  { path: "/logs", label: "Logs" },
];

function ProjectTab({ projectId }: { projectId: string }) {
  const env = useSearchParams().get("env");
  const pathname = usePathname();
  const base = `/projects/${projectId}`;
  return (
    <>
      {PROJECT_TABS.map((t) => {
        const active = pathname === `${base}${t.path}`;
        return (
          <Link
            key={t.label}
            href={`${base}${t.path}${env ? `?env=${env}` : ""}`}
            aria-current={active ? "page" : undefined}
            className={`tab${active ? " active" : ""}`}
          >
            {t.label}
          </Link>
        );
      })}
    </>
  );
}
