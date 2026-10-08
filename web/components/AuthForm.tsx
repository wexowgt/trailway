"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState } from "react";
import { api } from "@/lib/api";
import { Logo } from "./Logo";

export function AuthForm({ mode }: { mode: "login" | "signup" }) {
  const router = useRouter();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const isLogin = mode === "login";

  async function submit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const form = new FormData(e.currentTarget);
    setBusy(true);
    setError(null);
    try {
      await api(isLogin ? "/auth/login" : "/auth/signup", {
        method: "POST",
        body: JSON.stringify({ email: form.get("email"), password: form.get("password") }),
      });
      router.replace("/projects");
      router.refresh();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Something went wrong");
      setBusy(false);
    }
  }

  return (
    <main className="auth">
      <form className="card auth-card" onSubmit={submit}>
        <Logo />
        <h1>{isLogin ? "Log in to Trailway" : "Create your account"}</h1>
        <label>
          Email
          <input name="email" type="email" autoComplete="email" required autoFocus />
        </label>
        <label>
          Password
          <input
            name="password"
            type="password"
            autoComplete={isLogin ? "current-password" : "new-password"}
            minLength={isLogin ? undefined : 8}
            required
          />
          {!isLogin && <small>At least 8 characters.</small>}
        </label>
        {error && <p role="alert" className="error">{error}</p>}
        <button className="btn primary" disabled={busy}>
          {busy ? "Please wait..." : isLogin ? "Log in" : "Sign up"}
        </button>
        <p className="alt">
          {isLogin ? (
            <>No account? <Link href="/signup">Sign up</Link></>
          ) : (
            <>Have an account? <Link href="/login">Log in</Link></>
          )}
        </p>
      </form>
    </main>
  );
}
