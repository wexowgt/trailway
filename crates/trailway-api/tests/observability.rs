//! CPU and memory series, environment logs and the per-server credit
//! breakdown. Needs `TEST_DATABASE_URL` like `api.rs`; skipped without it.

use std::time::Duration;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
    MaybeTlsStream, WebSocketStream,
};
use tower::ServiceExt;
use trailway_api::{router, AppState, Config};
use trailway_proto::{AgentMessage, ApiMessage, DeploymentReport, DeploymentStatus, VmMetric};
use uuid::Uuid;

async fn setup() -> Option<(Router, PgPool)> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("connect to TEST_DATABASE_URL");
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    Some((router(AppState::new(pool.clone(), Config::default())), pool))
}

struct Reply {
    status: StatusCode,
    cookie: Option<String>,
    json: Value,
    text: String,
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    auth: (&str, &str),
    body: Option<Value>,
) -> Reply {
    let mut req = Request::builder().method(method).uri(uri);
    if !auth.1.is_empty() {
        req = req.header(auth.0, auth.1);
    }
    let body = match body {
        Some(b) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        cookie,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn call(app: &Router, method: Method, uri: &str, cookie: &str, body: Option<Value>) -> Reply {
    send(app, method, uri, ("cookie", cookie), body).await
}

async fn signup(app: &Router) -> String {
    let r = send(
        app,
        Method::POST,
        "/api/v1/auth/signup",
        ("", ""),
        Some(
            json!({"email": format!("{}@example.com", Uuid::new_v4()), "password": "password123"}),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    r.cookie.unwrap()
}

/// A registered server owned by the user: (server id, server token).
async fn add_server(
    app: &Router,
    cookie: &str,
    cpu_total: u64,
    memory_total: u64,
) -> (Uuid, String) {
    let key = call(
        app,
        Method::POST,
        "/api/v1/server-keys",
        cookie,
        Some(json!({"name": "k"})),
    )
    .await;
    let secret = key.json["secret"].as_str().unwrap().to_string();
    let reg = send(
        app,
        Method::POST,
        "/api/v1/agent/register",
        ("authorization", &format!("Bearer {secret}")),
        Some(json!({"machine_id": Uuid::new_v4().to_string(), "hostname": "box1", "agent_version": "0.1.0"})),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED);
    let token = reg.json["token"].as_str().unwrap().to_string();
    let id = Uuid::parse_str(reg.json["server_id"].as_str().unwrap()).unwrap();
    let hb = send(
        app,
        Method::POST,
        "/api/v1/agent/heartbeat",
        ("authorization", &format!("Bearer {token}")),
        Some(json!({
            "agent_version": "0.1.0",
            "cpu": {"total": cpu_total, "used": 0},
            "memory": {"total": memory_total, "used": 0},
            "disk": {"total": 100_000_000_000u64, "used": 0},
            "kvm": true
        })),
    )
    .await;
    assert_eq!(hb.status, StatusCode::NO_CONTENT);
    (id, token)
}

const GIB: u64 = 1 << 30;

struct Stack {
    env_id: String,
}

async fn new_stack(app: &Router, cookie: &str) -> Stack {
    let p = call(
        app,
        Method::POST,
        "/api/v1/projects",
        cookie,
        Some(json!({"name": "shop"})),
    )
    .await;
    assert_eq!(p.status, StatusCode::CREATED);
    let e = call(
        app,
        Method::POST,
        &format!(
            "/api/v1/projects/{}/environments",
            p.json["id"].as_str().unwrap()
        ),
        cookie,
        Some(json!({"name": "production"})),
    )
    .await;
    assert_eq!(e.status, StatusCode::CREATED);
    Stack {
        env_id: e.json["id"].as_str().unwrap().to_string(),
    }
}

async fn new_service(app: &Router, cookie: &str, env_id: &str, server: Uuid, body: Value) -> Value {
    let mut body = body;
    body["server_id"] = json!(server);
    let r = call(
        app,
        Method::POST,
        &format!("/api/v1/environments/{env_id}/services"),
        cookie,
        Some(body),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    r.json
}

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// A scripted agent on the real WebSocket route.
struct Agent {
    ws: Ws,
}

impl Agent {
    async fn connect(addr: std::net::SocketAddr, token: &str) -> Self {
        let mut req = format!("ws://{addr}/api/v1/agent/ws")
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        let (ws, _) = connect_async(req).await.expect("websocket handshake");
        Self { ws }
    }

    async fn send(&mut self, msg: AgentMessage) {
        let json = serde_json::to_string(&msg).unwrap();
        self.ws.send(Message::Text(json.into())).await.unwrap();
    }

    async fn hello(&mut self, deployments: Vec<DeploymentReport>) {
        self.send(AgentMessage::Hello { deployments }).await;
    }

    async fn status(&mut self, id: Uuid, status: DeploymentStatus, host_port: Option<u16>) {
        self.send(AgentMessage::Status(DeploymentReport {
            deployment_id: id,
            status,
            vm_id: Some(format!("vm-{id}")),
            host_port,
            error: None,
            commit: None,
        }))
        .await;
    }

    async fn next(&mut self) -> ApiMessage {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), self.ws.next())
                .await
                .expect("message from the API")
                .expect("socket open")
                .expect("no ws error");
            if let Message::Text(t) = msg {
                return serde_json::from_str(t.as_str()).unwrap();
            }
        }
    }
}

async fn serve(app: &Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn wait_for<T>(mut probe: impl AsyncFnMut() -> Option<T>) -> T {
    for _ in 0..100 {
        if let Some(v) = probe().await {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition never held");
}

#[tokio::test]
async fn agent_metrics_and_logs_reach_the_environment_endpoints() {
    let Some((app, pool)) = setup().await else {
        return;
    };
    let cookie = signup(&app).await;
    let other = signup(&app).await;
    let (server, token) = add_server(&app, &cookie, 4000, 8 * GIB).await;
    let stack = new_stack(&app, &cookie).await;
    let svc = new_service(
        &app,
        &cookie,
        &stack.env_id,
        server,
        json!({"name": "web", "image": "nginxdemos/hello", "port": 80, "vcpus": 2, "memory_mib": 512}),
    )
    .await;
    let sid = svc["id"].as_str().unwrap().to_string();
    let addr = serve(&app).await;
    let mut agent = Agent::connect(addr, &token).await;
    agent.hello(vec![]).await;
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/services/{sid}/deploy"),
        &cookie,
        None,
    )
    .await;
    let dep = Uuid::parse_str(r.json["id"].as_str().unwrap()).unwrap();
    let ApiMessage::Deploy(_) = agent.next().await else {
        panic!("expected a deploy job");
    };
    agent
        .status(dep, DeploymentStatus::Running, Some(30001))
        .await;

    agent
        .send(AgentMessage::Logs {
            deployment_id: dep,
            offset: 0,
            len: 26,
            text: "listening\nError: boom\n  at x".into(),
        })
        .await;
    agent
        .send(AgentMessage::Metrics {
            samples: vec![
                VmMetric {
                    deployment_id: dep,
                    cpu_millicores: 400,
                    memory_bytes: 100 * 1024 * 1024,
                },
                // Not a deployment of this server: dropped.
                VmMetric {
                    deployment_id: Uuid::new_v4(),
                    cpu_millicores: 1,
                    memory_bytes: 1,
                },
            ],
        })
        .await;

    // Metrics: one series per service with the configured limits.
    let uri = format!("/api/v1/environments/{}/metrics?range=1h", stack.env_id);
    let m = wait_for(async || {
        let m = call(&app, Method::GET, &uri, &cookie, None).await;
        (m.status == StatusCode::OK && !m.json["services"][0]["points"].as_array()?.is_empty())
            .then_some(m.json)
    })
    .await;
    let series = &m["services"][0];
    assert_eq!(series["name"], "web");
    assert_eq!(series["cpu_limit_millicores"], 2000);
    assert_eq!(series["memory_limit_bytes"], 512 * 1024 * 1024);
    assert_eq!(series["points"][0]["cpu_millicores"], 400.0);
    assert_eq!(series["points"][0]["memory_bytes"], 100.0 * 1024.0 * 1024.0);
    for range in ["24h", "7d"] {
        let uri = format!(
            "/api/v1/environments/{}/metrics?range={range}",
            stack.env_id
        );
        let r = call(&app, Method::GET, &uri, &cookie, None).await;
        assert_eq!(r.json["services"][0]["points"].as_array().unwrap().len(), 1);
    }
    let bad = format!("/api/v1/environments/{}/metrics?range=1y", stack.env_id);
    assert_eq!(
        call(&app, Method::GET, &bad, &cookie, None).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, Method::GET, &uri, &other, None).await.status,
        StatusCode::NOT_FOUND
    );

    // Logs: newest first load, then tailing by cursor, filtered by service.
    let logs_uri = format!("/api/v1/environments/{}/logs", stack.env_id);
    let l = wait_for(async || {
        let l = call(&app, Method::GET, &logs_uri, &cookie, None).await;
        (!l.json["chunks"].as_array()?.is_empty()).then_some(l.json)
    })
    .await;
    assert_eq!(l["chunks"][0]["service"], "web");
    assert!(l["chunks"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Error: boom"));
    let cursor = l["next_after"].as_i64().unwrap();
    let none = call(
        &app,
        Method::GET,
        &format!("{logs_uri}?after={cursor}"),
        &cookie,
        None,
    )
    .await;
    assert!(none.json["chunks"].as_array().unwrap().is_empty());
    assert_eq!(none.json["next_after"], cursor);
    agent
        .send(AgentMessage::Logs {
            deployment_id: dep,
            offset: 26,
            len: 5,
            text: "more\n".into(),
        })
        .await;
    let more = wait_for(async || {
        let r = call(
            &app,
            Method::GET,
            &format!("{logs_uri}?after={cursor}&service={sid}"),
            &cookie,
            None,
        )
        .await;
        (!r.json["chunks"].as_array()?.is_empty()).then_some(r.json)
    })
    .await;
    assert_eq!(more["chunks"][0]["text"], "more\n");
    let elsewhere = call(
        &app,
        Method::GET,
        &format!("{logs_uri}?service={}", Uuid::new_v4()),
        &cookie,
        None,
    )
    .await;
    assert!(elsewhere.json["chunks"].as_array().unwrap().is_empty());
    assert_eq!(
        call(&app, Method::GET, &logs_uri, &other, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Old samples are pruned.
    sqlx::query("UPDATE service_metrics SET ts = now() - interval '9 days'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(trailway_api::prune_metrics(&pool).await.unwrap() >= 1);
}

#[tokio::test]
async fn ledger_servers_adds_up_to_the_balance() {
    let Some((app, pool)) = setup().await else {
        return;
    };
    let cookie = signup(&app).await;
    let me = call(&app, Method::GET, "/api/v1/me", &cookie, None).await;
    let user: Uuid = me.json["id"].as_str().unwrap().parse().unwrap();
    let (a, _) = add_server(&app, &cookie, 4000, 8 * GIB).await;
    let (b, _) = add_server(&app, &cookie, 4000, 8 * GIB).await;
    for (server, kind, millicore_seconds, gib) in [
        (a, "contributed", 3_600_000i64, 7200i64),
        (a, "contributed", 1_800_000, 3600),
        (b, "contributed", 1_000_000, 1000),
        (a, "consumed", 500_000, 100),
    ] {
        sqlx::query(
            "INSERT INTO ledger_entries (user_id, server_id, kind, period_start, period_end, \
             millicore_seconds, byte_seconds) \
             VALUES ($1, $2, $3, now() - interval '1 hour', now(), $4, ($5::numeric * 1073741824))",
        )
        .bind(user)
        .bind(server)
        .bind(kind)
        .bind(millicore_seconds)
        .bind(gib)
        .execute(&pool)
        .await
        .unwrap();
    }
    let per_server = call(&app, Method::GET, "/api/v1/ledger/servers", &cookie, None).await;
    let rows = per_server.json.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let row_a = rows.iter().find(|r| r["server_id"] == json!(a)).unwrap();
    assert_eq!(row_a["hostname"], "box1");
    assert_eq!(row_a["contributed"]["vcpu_seconds"], 5400.0);
    assert_eq!(row_a["contributed"]["gb_seconds"], 10800.0);
    assert_eq!(row_a["consumed"]["vcpu_seconds"], 500.0);
    assert_eq!(row_a["balance"]["vcpu_seconds"], 4900.0);

    let total = call(&app, Method::GET, "/api/v1/ledger/balance", &cookie, None).await;
    for kind in ["contributed", "consumed", "balance"] {
        for unit in ["vcpu_seconds", "gb_seconds"] {
            let sum: f64 = rows.iter().map(|r| r[kind][unit].as_f64().unwrap()).sum();
            assert_eq!(
                sum,
                total.json[kind][unit].as_f64().unwrap(),
                "{kind} {unit}"
            );
        }
    }
}
