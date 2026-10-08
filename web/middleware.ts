import { NextResponse, type NextRequest } from "next/server";

const SESSION_COOKIE = "tw_session";
const PUBLIC_PATHS = ["/login", "/signup"];

// Cheap gate on cookie presence. The (app) layout validates the session
// against the API and redirects to /login when it is stale.
export function middleware(req: NextRequest) {
  const { pathname } = req.nextUrl;
  const hasSession = req.cookies.has(SESSION_COOKIE);
  const isPublic = PUBLIC_PATHS.includes(pathname);

  if (!hasSession && !isPublic) {
    return NextResponse.redirect(new URL("/login", req.url));
  }
  if (hasSession && isPublic) {
    return NextResponse.redirect(new URL("/projects", req.url));
  }
  return NextResponse.next();
}

export const config = {
  matcher: ["/((?!api/|_next/|install.sh|downloads/|favicon.ico).*)"],
};
