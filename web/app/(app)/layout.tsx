import { cookies } from "next/headers";
import { redirect } from "next/navigation";
import { TopBar } from "@/components/TopBar";
import type { User } from "@/lib/types";

const API_URL = (process.env.API_URL ?? "http://127.0.0.1:8080").replace(/\/$/, "");

async function currentUser(): Promise<User | null> {
  const session = (await cookies()).get("tw_session");
  if (!session) return null;
  try {
    const res = await fetch(`${API_URL}/api/v1/me`, {
      headers: { cookie: `tw_session=${session.value}` },
      cache: "no-store",
    });
    return res.ok ? ((await res.json()) as User) : null;
  } catch {
    return null;
  }
}

export default async function AppLayout({ children }: { children: React.ReactNode }) {
  const user = await currentUser();
  if (!user) redirect("/login");
  return (
    <>
      <TopBar email={user.email} />
      <main className="page">{children}</main>
    </>
  );
}
