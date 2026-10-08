import Link from "next/link";
import { AvatarMenu } from "./AvatarMenu";
import { Logo } from "./Logo";

export function TopBar({ email }: { email: string }) {
  return (
    <header className="topbar">
      <nav className="breadcrumb" aria-label="Breadcrumb">
        <Link href="/servers" aria-label="Trailway home">
          <Logo />
        </Link>
        <span className="sep" aria-hidden="true">/</span>
        <span>Personal</span>
        <span className="sep" aria-hidden="true">/</span>
        <span className="env">production <span aria-hidden="true">&#9662;</span></span>
      </nav>
      <nav className="tabs" aria-label="Sections">
        <Link href="/servers" aria-current="page" className="tab active">Servers</Link>
        <AvatarMenu email={email} />
      </nav>
    </header>
  );
}
