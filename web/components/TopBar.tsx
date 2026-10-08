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
        <AvatarMenu email={email} />
      </nav>
    </header>
  );
}

function ProjectTab({ projectId }: { projectId: string }) {
  const env = useSearchParams().get("env");
  return (
    <Link
      href={`/projects/${projectId}${env ? `?env=${env}` : ""}`}
      aria-current="page"
      className="tab active"
    >
      Architecture
    </Link>
  );
}
