//! Public HTTPS routes for deployed services, kept in Caddy through its admin
//! API. Caddy gets the certificates (Let's Encrypt) and proxies HTTP and
//! WebSockets to the VM's forwarded port on this host.

use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Mutex,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

pub const DEFAULT_ADMIN: &str = "127.0.0.1:2019";
/// Name of the one Caddy HTTP server the agent owns.
const SERVER: &str = "trailway";
/// Prefix of the `@id` of every route the agent owns; others are left alone.
const ID_PREFIX: &str = "tw-";
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// One host name that must reach a port on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Stable id of what the route belongs to (the service).
    pub key: String,
    pub domain: String,
    pub port: u16,
}

/// Makes the proxy serve exactly these routes.
pub trait Proxy {
    fn sync(&self, routes: &[Route]) -> Result<()>;
}

/// Used where there is no Caddy (development with the fake runtime).
pub struct NoProxy;

impl Proxy for NoProxy {
    fn sync(&self, _routes: &[Route]) -> Result<()> {
        Ok(())
    }
}

/// Records what it was asked to serve (tests).
#[derive(Default)]
pub struct RecordingProxy {
    pub routes: Mutex<Vec<Route>>,
}

impl Proxy for RecordingProxy {
    fn sync(&self, routes: &[Route]) -> Result<()> {
        *self.routes.lock().unwrap() = routes.to_vec();
        Ok(())
    }
}

fn route_id(key: &str) -> String {
    format!("{ID_PREFIX}{key}")
}

fn route_json(route: &Route) -> Value {
    json!({
        "@id": route_id(&route.key),
        "match": [{"host": [route.domain]}],
        "handle": [{
            "handler": "reverse_proxy",
            "upstreams": [{"dial": format!("127.0.0.1:{}", route.port)}],
        }],
        "terminal": true,
    })
}

/// What an existing Caddy route says it serves, if it is one of ours.
fn parse_route(v: &Value) -> Option<Route> {
    let id = v.get("@id")?.as_str()?.strip_prefix(ID_PREFIX)?;
    let domain = v.pointer("/match/0/host/0")?.as_str()?;
    let dial = v.pointer("/handle/0/upstreams/0/dial")?.as_str()?;
    let port = dial.rsplit(':').next()?.parse().ok()?;
    Some(Route {
        key: id.into(),
        domain: domain.into(),
        port,
    })
}

pub struct CaddyProxy {
    admin: String,
}

impl CaddyProxy {
    pub fn new(admin: impl Into<String>) -> Self {
        Self {
            admin: admin.into(),
        }
    }

    /// `TRAILWAY_CADDY_ADMIN` overrides the admin address.
    pub fn from_env() -> Self {
        Self::new(std::env::var("TRAILWAY_CADDY_ADMIN").unwrap_or_else(|_| DEFAULT_ADMIN.into()))
    }

    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Vec<u8>)> {
        let addr = self
            .admin
            .to_socket_addrs()?
            .next()
            .with_context(|| format!("cannot resolve {}", self.admin))?;
        let mut stream = TcpStream::connect_timeout(&addr, IO_TIMEOUT)
            .with_context(|| format!("Caddy admin API at {} is not reachable", self.admin))?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let body = body.map(Value::to_string).unwrap_or_default();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            self.admin,
            body.len()
        );
        stream.write_all(format!("{head}{body}").as_bytes())?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw)?;
        parse_response(&raw)
    }

    fn json(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        let (status, raw) = self.request(method, path, body)?;
        let text = String::from_utf8_lossy(&raw);
        // A path that does not exist yet is 404, or 400 when a parent is missing too.
        let missing = status == 404 || (status == 400 && text.contains("invalid traversal path"));
        if status >= 300 && !missing {
            bail!("Caddy {method} {path}: {status} {}", text.trim());
        }
        if missing {
            return Ok((status, Value::Null));
        }
        let value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
        Ok((status, value))
    }

    /// Creates the HTTP server the routes live in when Caddy does not have it
    /// yet (first run, or Caddy restarted from its own config). Returns whether it did.
    fn ensure_server(&self) -> Result<bool> {
        let path = format!("/config/apps/http/servers/{SERVER}");
        let (_, current) = self.json("GET", &path, None)?;
        if !current.is_null() {
            return Ok(false);
        }
        // Host matchers make Caddy get certificates and redirect HTTP to HTTPS on its own.
        let config = json!({"apps": {"http": {"servers": {SERVER: {
            "listen": [":443"],
            "routes": [],
        }}}}});
        self.json("POST", "/load", Some(&config))?;
        Ok(true)
    }

    fn current(&self) -> Result<Vec<Route>> {
        let path = format!("/config/apps/http/servers/{SERVER}/routes");
        let (_, routes) = self.json("GET", &path, None)?;
        Ok(routes
            .as_array()
            .map(|a| a.iter().filter_map(parse_route).collect())
            .unwrap_or_default())
    }
}

impl Proxy for CaddyProxy {
    fn sync(&self, wanted: &[Route]) -> Result<()> {
        self.ensure_server()?;
        let have = self.current()?;
        for old in have
            .iter()
            .filter(|r| !wanted.iter().any(|w| w.key == r.key))
        {
            self.json("DELETE", &format!("/id/{}", route_id(&old.key)), None)?;
        }
        for route in wanted {
            match have.iter().find(|r| r.key == route.key) {
                Some(r) if r == route => {}
                // PATCH swaps the route in place, so a redeploy has no gap.
                Some(_) => {
                    self.json(
                        "PATCH",
                        &format!("/id/{}", route_id(&route.key)),
                        Some(&route_json(route)),
                    )?;
                }
                None => {
                    let path = format!("/config/apps/http/servers/{SERVER}/routes");
                    self.json("POST", &path, Some(&route_json(route)))?;
                }
            }
        }
        Ok(())
    }
}

/// Splits an HTTP/1.1 response into status and (de-chunked) body.
fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>)> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("incomplete response from Caddy")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .context("bad status line from Caddy")?;
    let chunked = head
        .lines()
        .any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.contains("chunked"));
    let body = &raw[split + 4..];
    Ok((
        status,
        if chunked {
            dechunk(body)?
        } else {
            body.to_vec()
        },
    ))
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .context("bad chunked body")?;
        let size = usize::from_str_radix(String::from_utf8_lossy(&body[..eol]).trim(), 16)
            .context("bad chunk size")?;
        body = &body[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 {
            bail!("truncated chunk");
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };

    fn route(key: &str, domain: &str, port: u16) -> Route {
        Route {
            key: key.into(),
            domain: domain.into(),
            port,
        }
    }

    #[test]
    fn route_json_roundtrips() {
        let r = route("svc", "hello-production.1-2-3-4.sslip.io", 30001);
        assert_eq!(parse_route(&route_json(&r)), Some(r));
        // Routes without our id are not ours.
        assert_eq!(parse_route(&json!({"match": [{"host": ["x"]}]})), None);
    }

    #[test]
    fn parses_plain_and_chunked_responses() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]";
        assert_eq!(parse_response(plain).unwrap(), (200, b"[]".to_vec()));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n[1,\r\n2\r\n2]\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked).unwrap(), (200, b"[1,2]".to_vec()));
        assert!(parse_response(b"garbage").is_err());
    }

    /// A tiny Caddy admin API: keeps routes in a list and logs the calls.
    fn fake_caddy(initial: Value) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let log = calls.clone();
        let state = Arc::new(Mutex::new(initial));
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut buf = vec![0u8; 16384];
                let n = stream.read(&mut buf).unwrap();
                let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                let first = text.lines().next().unwrap().to_string();
                let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
                log.lock().unwrap().push(first.clone());
                let mut state = state.lock().unwrap();
                let (status, reply) =
                    if first.starts_with("GET /config/apps/http/servers/trailway/routes") {
                        (200, state.clone())
                    } else if first.starts_with("GET /config/apps/http/servers/trailway") {
                        if state.is_null() {
                            (
                                400,
                                json!({"error": "invalid traversal path at: config/apps/http"}),
                            )
                        } else {
                            (200, json!({"routes": state.clone()}))
                        }
                    } else if first.starts_with("POST /load") {
                        *state = json!([]);
                        (200, Value::Null)
                    } else if first.starts_with("POST /config/apps/http/servers/trailway/routes") {
                        state
                            .as_array_mut()
                            .unwrap()
                            .push(serde_json::from_str(body).unwrap());
                        (200, Value::Null)
                    } else if let Some(rest) = first.strip_prefix("PATCH /id/") {
                        let id = rest.split(' ').next().unwrap();
                        let new: Value = serde_json::from_str(body).unwrap();
                        for r in state.as_array_mut().unwrap() {
                            if r["@id"] == id {
                                *r = new.clone();
                            }
                        }
                        (200, Value::Null)
                    } else if let Some(rest) = first.strip_prefix("DELETE /id/") {
                        let id = rest.split(' ').next().unwrap();
                        state.as_array_mut().unwrap().retain(|r| r["@id"] != id);
                        (200, Value::Null)
                    } else {
                        (500, Value::Null)
                    };
                let reply = reply.to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
        });
        (addr, calls)
    }

    #[test]
    fn sync_adds_swaps_and_removes_routes() {
        let (addr, calls) = fake_caddy(Value::Null);
        let proxy = CaddyProxy::new(addr);
        let a = route("a", "a.example.com", 30000);

        proxy.sync(std::slice::from_ref(&a)).unwrap();
        assert!(calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("POST /load")));
        assert_eq!(proxy.current().unwrap(), std::slice::from_ref(&a));

        // Same again: nothing to change.
        let before = calls.lock().unwrap().len();
        proxy.sync(std::slice::from_ref(&a)).unwrap();
        let after: Vec<String> = calls.lock().unwrap()[before..].to_vec();
        assert!(after.iter().all(|c| c.starts_with("GET")), "{after:?}");

        // Redeploy on another port swaps in place.
        let a2 = route("a", "a.example.com", 30005);
        proxy.sync(std::slice::from_ref(&a2)).unwrap();
        assert!(calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("PATCH /id/tw-a")));
        assert_eq!(proxy.current().unwrap(), [a2]);

        // Stopping removes the route.
        proxy.sync(&[]).unwrap();
        assert!(proxy.current().unwrap().is_empty());
    }

    #[test]
    fn unreachable_caddy_is_an_error() {
        let proxy = CaddyProxy::new("127.0.0.1:1");
        let err = proxy.sync(&[]).unwrap_err().to_string();
        assert!(err.contains("not reachable"), "{err}");
    }
}
