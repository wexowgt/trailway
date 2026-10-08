export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

/** Same-origin call to the API (proxied by Next); the session cookie rides along. */
export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(`/api/v1${path}`, {
    ...init,
    headers: init.body ? { "Content-Type": "application/json", ...init.headers } : init.headers,
    cache: "no-store",
  });
  if (!res.ok) {
    let message = `Request failed (${res.status})`;
    try {
      const body = await res.json();
      if (body?.error?.message) message = body.error.message;
    } catch {
      // keep the generic message
    }
    if (res.status === 401 && typeof window !== "undefined") {
      window.location.assign("/login");
    }
    throw new ApiError(res.status, message);
  }
  return res.status === 204 ? (undefined as T) : ((await res.json()) as T);
}
