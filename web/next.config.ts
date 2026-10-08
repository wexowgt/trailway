import type { NextConfig } from "next";

// The browser only talks to this origin. API calls, the installer script and
// agent downloads are proxied to the Rust API, so the session cookie is
// same-origin and no CORS is needed.
const apiUrl = (process.env.API_URL ?? "http://127.0.0.1:8080").replace(/\/$/, "");

const nextConfig: NextConfig = {
  async rewrites() {
    return [
      { source: "/api/v1/:path*", destination: `${apiUrl}/api/v1/:path*` },
      { source: "/install.sh", destination: `${apiUrl}/install.sh` },
      { source: "/downloads/:name", destination: `${apiUrl}/downloads/:name` },
    ];
  },
};

export default nextConfig;
