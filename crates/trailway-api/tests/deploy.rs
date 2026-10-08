//! Projects, services and deploys, with a scripted agent on the WebSocket.
//! Needs `TEST_DATABASE_URL` like `api.rs`; skipped without it.

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
use trailway_proto::{AgentMessage, ApiMessage, DeploymentReport, DeploymentStatus};
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
    new_stack_in(app, cookie, "shop").await
}

async fn new_stack_in(app: &Router, cookie: &str, project: &str) -> Stack {
    let p = call(
        app,
        Method::POST,
        "/api/v1/projects",
        cookie,
        Some(json!({"name": project})),
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

async fn deployment_status(app: &Router, cookie: &str, id: &str) -> Value {
    let r = call(
        app,
        Method::GET,
        &format!("/api/v1/deployments/{id}"),
        cookie,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    r.json
}

/// Polls until the deployment has `status` (the API applies agent messages asynchronously).
async fn wait_status(app: &Router, cookie: &str, id: &str, status: &str) -> Value {
    for _ in 0..100 {
        let d = deployment_status(app, cookie, id).await;
        if d["status"] == status {
            return d;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("deployment {id} never reached {status}");
}

#[tokio::test]
async fn crud_validation_and_ownership() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let alice = signup(&app).await;
    let bob = signup(&app).await;
    let (server, _) = add_server(&app, &alice, 4000, 8 * GIB).await;
    let (bobs_server, _) = add_server(&app, &bob, 4000, 8 * GIB).await;

    // unauthenticated
    let r = call(&app, Method::GET, "/api/v1/projects", "", None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    let stack = new_stack(&app, &alice).await;
    let dup = call(
        &app,
        Method::POST,
        "/api/v1/projects",
        &alice,
        Some(json!({"name": "shop"})),
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    let list = call(&app, Method::GET, "/api/v1/projects", &alice, None).await;
    assert_eq!(list.json.as_array().unwrap().len(), 1);
    let bad = call(
        &app,
        Method::POST,
        "/api/v1/projects",
        &alice,
        Some(json!({"name": " "})),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);

    // service: defaults, validation, server must be the caller's own
    let svc = new_service(
        &app,
        &alice,
        &stack.env_id,
        server,
        json!({"name": "web", "image": "nginxdemos/hello", "port": 80, "env": {"A": "1"}}),
    )
    .await;
    assert_eq!(svc["vcpus"], 1);
    assert_eq!(svc["memory_mib"], 256);
    assert_eq!(svc["env"]["A"], "1");
    let sid = svc["id"].as_str().unwrap();
    let url = format!("/api/v1/environments/{}/services", stack.env_id);
    for body in [
        json!({"name": "web", "image": "x", "server_id": server}),
        json!({"name": "n", "image": "has space", "server_id": server}),
        json!({"name": "n", "image": "x", "vcpus": 0, "server_id": server}),
        json!({"name": "n", "image": "x", "memory_mib": 8, "server_id": server}),
        json!({"name": "n", "image": "x", "env": {"A=B": "1"}, "server_id": server}),
        json!({"name": "n", "image": "x", "server_id": bobs_server}),
        json!({"name": "n", "image": "x", "server_id": Uuid::new_v4()}),
    ] {
        let r = call(&app, Method::POST, &url, &alice, Some(body.clone())).await;
        assert!(
            r.status == StatusCode::BAD_REQUEST || r.status == StatusCode::CONFLICT,
            "{body} -> {}",
            r.status
        );
    }

    // update and read back
    let up = call(
        &app,
        Method::PATCH,
        &format!("/api/v1/services/{sid}"),
        &alice,
        Some(json!({"vcpus": 2, "env": {"B": "2"}})),
    )
    .await;
    assert_eq!(up.status, StatusCode::OK);
    assert_eq!(up.json["vcpus"], 2);
    assert_eq!(up.json["env"], json!({"B": "2"}));
    assert_eq!(up.json["image"], "nginxdemos/hello");

    // another user sees nothing
    for uri in [
        format!("/api/v1/services/{sid}"),
        format!("/api/v1/environments/{}", stack.env_id),
        format!("/api/v1/environments/{}/services", stack.env_id),
    ] {
        let r = call(&app, Method::GET, &uri, &bob, None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
    }
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/services/{sid}/deploy"),
        &bob,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // delete cascades
    let r = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/services/{sid}"),
        &alice,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(
        &app,
        Method::GET,
        &format!("/api/v1/services/{sid}"),
        &alice,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/environments/{}", stack.env_id),
        &alice,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn deploy_redeploy_stop_and_logs_over_the_agent_socket() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let addr = serve(&app).await;
    let cookie = signup(&app).await;
    let (server, token) = add_server(&app, &cookie, 4000, 8 * GIB).await;
    let stack = new_stack(&app, &cookie).await;
    let svc = new_service(
        &app,
        &cookie,
        &stack.env_id,
        server,
        json!({"name": "web", "image": "nginxdemos/hello", "port": 80, "env": {"GREETING": "one"}}),
    )
    .await;
    let sid = svc["id"].as_str().unwrap().to_string();

    // The socket needs the server token.
    let mut req = format!("ws://{addr}/api/v1/agent/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", "Bearer tw_st_nope".parse().unwrap());
    assert!(connect_async(req).await.is_err());

    let mut agent = Agent::connect(addr, &token).await;
    agent.hello(vec![]).await;

    // Deploy: the agent gets the job, reports progress, and the API follows.
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/services/{sid}/deploy"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    assert_eq!(r.json["status"], "queued");
    let first = r.json["id"].as_str().unwrap().to_string();
    let first_id = Uuid::parse_str(&first).unwrap();
    let ApiMessage::Deploy(job) = agent.next().await else {
        panic!("expected a deploy job");
    };
    assert_eq!(job.deployment_id, first_id);
    assert_eq!(job.spec.image, "nginxdemos/hello");
    assert_eq!(job.spec.port, Some(80));
    assert_eq!(
        job.spec.env,
        vec![("GREETING".to_string(), "one".to_string())]
    );

    agent
        .status(first_id, DeploymentStatus::Building, None)
        .await;
    wait_status(&app, &cookie, &first, "building").await;
    agent
        .status(first_id, DeploymentStatus::Running, Some(30001))
        .await;
    let d = wait_status(&app, &cookie, &first, "running").await;
    assert_eq!(d["host_port"], 30001);

    // Logs arrive in chunks; a resent chunk is not stored twice.
    for (offset, text) in [(0u64, "boot\n"), (5, "listening on 80\n"), (0, "boot\n")] {
        agent
            .send(AgentMessage::Logs {
                deployment_id: first_id,
                offset,
                len: text.len() as u64,
                text: text.into(),
            })
            .await;
    }
    let mut logs = String::new();
    for _ in 0..100 {
        logs = call(
            &app,
            Method::GET,
            &format!("/api/v1/deployments/{first}/logs"),
            &cookie,
            None,
        )
        .await
        .text;
        if logs.contains("listening") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(logs, "boot\nlistening on 80\n");

    // Following ends when the deployment ends.
    let follow_app = app.clone();
    let (follow_cookie, follow_url) = (
        cookie.clone(),
        format!("/api/v1/deployments/{first}/logs?follow=true"),
    );
    let follower = tokio::spawn(async move {
        call(&follow_app, Method::GET, &follow_url, &follow_cookie, None)
            .await
            .text
    });

    // Redeploy with a changed env var: new job, old deployment is replaced.
    let r = call(
        &app,
        Method::PATCH,
        &format!("/api/v1/services/{sid}"),
        &cookie,
        Some(json!({"env": {"GREETING": "two"}})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/services/{sid}/deploy"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    let second = r.json["id"].as_str().unwrap().to_string();
    let second_id = Uuid::parse_str(&second).unwrap();
    let ApiMessage::Deploy(job) = agent.next().await else {
        panic!("expected a deploy job");
    };
    assert_eq!(job.deployment_id, second_id);
    assert_eq!(job.service_id.to_string(), sid);
    assert_eq!(
        job.spec.env,
        vec![("GREETING".to_string(), "two".to_string())]
    );
    // The agent stops the old VM and starts the new one.
    agent
        .status(first_id, DeploymentStatus::Stopped, None)
        .await;
    agent
        .status(second_id, DeploymentStatus::Running, Some(30002))
        .await;
    wait_status(&app, &cookie, &first, "stopped").await;
    let d = wait_status(&app, &cookie, &second, "running").await;
    assert_eq!(d["host_port"], 30002);
    let followed = tokio::time::timeout(Duration::from_secs(5), follower)
        .await
        .expect("follow ends with the deployment")
        .unwrap();
    assert_eq!(followed, "boot\nlistening on 80\n");

    let list = call(
        &app,
        Method::GET,
        &format!("/api/v1/services/{sid}/deployments"),
        &cookie,
        None,
    )
    .await;
    let ids: Vec<&str> = list
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [second.as_str(), first.as_str()]);

    // Stop: the agent is told, the API shows stopped once it confirms.
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/services/{sid}/stop"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    assert_eq!(r.json.as_array().unwrap().len(), 1);
    assert_eq!(
        agent.next().await,
        ApiMessage::Stop {
            deployment_id: second_id
        }
    );
    agent
        .status(second_id, DeploymentStatus::Stopped, None)
        .await;
    let d = wait_status(&app, &cookie, &second, "stopped").await;
    assert!(d["host_port"].is_null());

    // Deleting a running service removes its VM too.
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/services/{sid}/deploy"),
        &cookie,
        None,
    )
    .await;
    let third = Uuid::parse_str(r.json["id"].as_str().unwrap()).unwrap();
    assert!(matches!(agent.next().await, ApiMessage::Deploy(_)));
    agent
        .status(third, DeploymentStatus::Running, Some(30003))
        .await;
    wait_status(&app, &cookie, &third.to_string(), "running").await;
    let r = call(
        &app,
        Method::DELETE,
        &format!("/api/v1/services/{sid}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        agent.next().await,
        ApiMessage::Stop {
            deployment_id: third
        }
    );
    agent.status(third, DeploymentStatus::Stopped, None).await;
    wait_status(&app, &cookie, &third.to_string(), "stopped").await;
}

#[tokio::test]
async fn over_capacity_and_offline_deploys_are_refused() {
    let Some((app, pool)) = setup().await else {
        return;
    };
    let cookie = signup(&app).await;
    let (server, _) = add_server(&app, &cookie, 2000, GIB).await;
    let stack = new_stack(&app, &cookie).await;
    let deploy = |sid: String| {
        let (app, cookie) = (app.clone(), cookie.clone());
        async move {
            call(
                &app,
                Method::POST,
                &format!("/api/v1/services/{sid}/deploy"),
                &cookie,
                None,
            )
            .await
        }
    };

    let too_much_cpu = new_service(
        &app,
        &cookie,
        &stack.env_id,
        server,
        json!({"name": "cpu", "image": "x", "vcpus": 3}),
    )
    .await;
    let r = deploy(too_much_cpu["id"].as_str().unwrap().into()).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json["error"]["code"], "insufficient_capacity");
    assert!(r.json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("3 vCPU"));

    let too_much_mem = new_service(
        &app,
        &cookie,
        &stack.env_id,
        server,
        json!({"name": "mem", "image": "x", "memory_mib": 2048}),
    )
    .await;
    let r = deploy(too_much_mem["id"].as_str().unwrap().into()).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json["error"]["code"], "insufficient_capacity");
    assert!(r.json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("2048 MiB"));

    // Nothing was queued for the refused deploys.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deployments WHERE server_id = $1")
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);

    // Two small services that fit alone but not together: the second is refused
    // while the first is still on its way to the agent.
    let a = new_service(
        &app,
        &cookie,
        &stack.env_id,
        server,
        json!({"name": "a", "image": "x", "memory_mib": 700}),
    )
    .await;
    let b = new_service(
        &app,
        &cookie,
        &stack.env_id,
        server,
        json!({"name": "b", "image": "x", "memory_mib": 700}),
    )
    .await;
    assert_eq!(
        deploy(a["id"].as_str().unwrap().into()).await.status,
        StatusCode::ACCEPTED
    );
    let r = deploy(b["id"].as_str().unwrap().into()).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json["error"]["code"], "insufficient_capacity");
    // The same service can be deployed again: its own VM is replaced, not added.
    assert_eq!(
        deploy(a["id"].as_str().unwrap().into()).await.status,
        StatusCode::ACCEPTED
    );

    // Offline server.
    sqlx::query(
        "UPDATE servers SET last_heartbeat_at = now() - interval '60 seconds' WHERE id = $1",
    )
    .bind(server)
    .execute(&pool)
    .await
    .unwrap();
    let r = deploy(a["id"].as_str().unwrap().into()).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json["error"]["code"], "server_offline");
}

#[tokio::test]
async fn reconnect_reconciles_with_what_the_agent_reports() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let addr = serve(&app).await;
    let cookie = signup(&app).await;
    let (server, token) = add_server(&app, &cookie, 8000, 8 * GIB).await;
    let stack = new_stack(&app, &cookie).await;
    let web = new_service(
        &app,
        &cookie,
        &stack.env_id,
        server,
        json!({"name": "web", "image": "x", "port": 80}),
    )
    .await;
    let db = new_service(
        &app,
        &cookie,
        &stack.env_id,
        server,
        json!({"name": "db", "image": "y"}),
    )
    .await;
    let (web_id, db_id) = (web["id"].as_str().unwrap(), db["id"].as_str().unwrap());

    // No agent connected: the deploy waits as queued.
    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/services/{web_id}/deploy"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let queued = r.json["id"].as_str().unwrap().to_string();

    // The agent connects with an empty state: the queued job is handed over.
    let mut agent = Agent::connect(addr, &token).await;
    agent.hello(vec![]).await;
    let ApiMessage::Deploy(job) = agent.next().await else {
        panic!("expected the queued job");
    };
    assert_eq!(job.deployment_id.to_string(), queued);
    agent
        .status(job.deployment_id, DeploymentStatus::Running, Some(30010))
        .await;
    wait_status(&app, &cookie, &queued, "running").await;

    let r = call(
        &app,
        Method::POST,
        &format!("/api/v1/services/{db_id}/deploy"),
        &cookie,
        None,
    )
    .await;
    let other = Uuid::parse_str(r.json["id"].as_str().unwrap()).unwrap();
    assert!(matches!(agent.next().await, ApiMessage::Deploy(_)));
    agent.status(other, DeploymentStatus::Deploying, None).await;
    wait_status(&app, &cookie, &other.to_string(), "deploying").await;

    // API restart (connection drops). The agent comes back: web still runs,
    // the db deploy was lost with the agent's memory, so it is handed over again.
    drop(agent);
    let mut agent = Agent::connect(addr, &token).await;
    agent
        .hello(vec![DeploymentReport {
            deployment_id: job.deployment_id,
            status: DeploymentStatus::Running,
            vm_id: Some("vm-web".into()),
            host_port: Some(30010),
            error: None,
            commit: None,
        }])
        .await;
    let ApiMessage::Deploy(again) = agent.next().await else {
        panic!("expected the lost job again");
    };
    assert_eq!(again.deployment_id, other);
    assert_eq!(
        deployment_status(&app, &cookie, &queued).await["status"],
        "running"
    );

    // The VM died while the agent was away: reported as failed or simply gone.
    drop(agent);
    let mut agent = Agent::connect(addr, &token).await;
    agent
        .hello(vec![DeploymentReport {
            deployment_id: other,
            status: DeploymentStatus::Failed,
            vm_id: None,
            host_port: None,
            error: Some("image pull failed".into()),
            commit: None,
        }])
        .await;
    let d = wait_status(&app, &cookie, &queued, "failed").await;
    assert!(d["error"]
        .as_str()
        .unwrap()
        .contains("no longer has this VM"));
    let d = wait_status(&app, &cookie, &other.to_string(), "failed").await;
    assert_eq!(d["error"], "image pull failed");

    // A VM the API knows nothing about is removed.
    drop(agent);
    let mut agent = Agent::connect(addr, &token).await;
    let ghost = Uuid::new_v4();
    agent
        .hello(vec![DeploymentReport {
            deployment_id: ghost,
            status: DeploymentStatus::Running,
            vm_id: Some("vm-ghost".into()),
            host_port: None,
            error: None,
            commit: None,
        }])
        .await;
    assert_eq!(
        agent.next().await,
        ApiMessage::Stop {
            deployment_id: ghost
        }
    );
}

async fn heartbeat_with_ip(app: &Router, token: &str, ip: &str) -> StatusCode {
    send(
        app,
        Method::POST,
        "/api/v1/agent/heartbeat",
        ("authorization", &format!("Bearer {token}")),
        Some(json!({
            "agent_version": "0.1.0",
            "cpu": {"total": 4000, "used": 0},
            "memory": {"total": 8 * GIB, "used": 0},
            "disk": {"total": 100_000_000_000u64, "used": 0},
            "kvm": true,
            "public_ip": ip
        })),
    )
    .await
    .status
}

#[tokio::test]
async fn services_get_a_stable_public_url_from_the_server_ip() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let addr = serve(&app).await;
    let cookie = signup(&app).await;
    let (server, token) = add_server(&app, &cookie, 4000, 8 * GIB).await;
    let stack = new_stack(&app, &cookie).await;
    let web = json!({"name": "Hello", "image": "nginxdemos/hello", "port": 80});

    // The IP is not known yet: no URL.
    let svc = new_service(&app, &cookie, &stack.env_id, server, web.clone()).await;
    assert_eq!(svc["url"], Value::Null);

    assert_eq!(
        heartbeat_with_ip(&app, &token, "not an ip").await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        heartbeat_with_ip(&app, &token, "178.104.208.91").await,
        StatusCode::NO_CONTENT
    );
    let sid = svc["id"].as_str().unwrap().to_string();
    let url = "https://hello-production.178-104-208-91.sslip.io";
    let got = call(
        &app,
        Method::GET,
        &format!("/api/v1/services/{sid}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(got.json["url"], url);

    // The same name in another project on the same server gets a suffix.
    let other = new_stack_in(&app, &cookie, "blog").await;
    let twin = new_service(&app, &cookie, &other.env_id, server, web.clone()).await;
    let twin_url = twin["url"].as_str().unwrap();
    assert!(
        twin_url.starts_with("https://hello-production-"),
        "{twin_url}"
    );
    assert_ne!(twin_url, url);

    // No port, no URL.
    let worker = new_service(
        &app,
        &cookie,
        &stack.env_id,
        server,
        json!({"name": "worker", "image": "busybox"}),
    )
    .await;
    assert_eq!(worker["url"], Value::Null);

    // The deploy job carries the domain, and a redeploy and a rename keep it.
    let mut agent = Agent::connect(addr, &token).await;
    agent.hello(vec![]).await;
    for _ in 0..2 {
        let r = call(
            &app,
            Method::POST,
            &format!("/api/v1/services/{sid}/deploy"),
            &cookie,
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
        let ApiMessage::Deploy(job) = agent.next().await else {
            panic!("expected a deploy job");
        };
        assert_eq!(
            job.domain.as_deref(),
            Some("hello-production.178-104-208-91.sslip.io")
        );
        let id = job.deployment_id;
        agent
            .status(id, DeploymentStatus::Running, Some(30001))
            .await;
        wait_status(&app, &cookie, &id.to_string(), "running").await;
        let renamed = call(
            &app,
            Method::PATCH,
            &format!("/api/v1/services/{sid}"),
            &cookie,
            Some(json!({"name": "Renamed"})),
        )
        .await;
        assert_eq!(renamed.json["url"], url);
    }
}

#[tokio::test]
async fn git_service_builds_and_a_failed_build_keeps_the_previous_version() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let addr = serve(&app).await;
    let cookie = signup(&app).await;
    let (server, token) = add_server(&app, &cookie, 4000, 8 * GIB).await;
    let stack = new_stack(&app, &cookie).await;

    // Exactly one source, and the git source must be a plain https repo.
    let path = format!("/api/v1/environments/{}/services", stack.env_id);
    for body in [
        json!({"name": "a", "server_id": server}),
        json!({"name": "a", "image": "x", "git": {"url": "https://github.com/o/r", "branch": "main"}, "server_id": server}),
        json!({"name": "a", "git": {"url": "http://github.com/o/r", "branch": "main"}, "server_id": server}),
        json!({"name": "a", "git": {"url": "https://github.com/o/r", "branch": "-x"}, "server_id": server}),
    ] {
        let r = call(&app, Method::POST, &path, &cookie, Some(body)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    }
    let svc = new_service(
        &app,
        &cookie,
        &stack.env_id,
        server,
        json!({"name": "app", "git": {"url": "https://github.com/o/r", "branch": "main"}, "port": 3000}),
    )
    .await;
    assert_eq!(svc["git"]["branch"], "main");
    assert!(svc["image"].is_null());
    let sid = svc["id"].as_str().unwrap().to_string();

    let mut agent = Agent::connect(addr, &token).await;
    agent.hello(vec![]).await;
    let deploy_uri = format!("/api/v1/services/{sid}/deploy");
    let deploy = || call(&app, Method::POST, &deploy_uri, &cookie, None);

    // First deploy: the job carries the source, the agent reports the commit.
    let r = deploy().await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    assert_eq!(r.json["git"]["url"], "https://github.com/o/r");
    let first = r.json["id"].as_str().unwrap().to_string();
    let first_id = Uuid::parse_str(&first).unwrap();
    let ApiMessage::Deploy(job) = agent.next().await else {
        panic!("expected a deploy job");
    };
    assert_eq!(job.source.unwrap().branch, "main");
    assert!(job.spec.env.contains(&("PORT".into(), "3000".into())));
    agent
        .send(AgentMessage::Status(DeploymentReport {
            deployment_id: first_id,
            status: DeploymentStatus::Running,
            vm_id: Some("vm-1".into()),
            host_port: Some(30001),
            error: None,
            commit: Some("abc123".into()),
        }))
        .await;
    let d = wait_status(&app, &cookie, &first, "running").await;
    assert_eq!(d["commit"], "abc123");

    // Second deploy fails to build: the first stays wanted and running.
    let r = deploy().await;
    let second = r.json["id"].as_str().unwrap().to_string();
    let second_id = Uuid::parse_str(&second).unwrap();
    let ApiMessage::Deploy(_) = agent.next().await else {
        panic!("expected a deploy job");
    };
    agent
        .send(AgentMessage::Logs {
            deployment_id: second_id,
            offset: 0,
            len: 12,
            text: "building...\n".into(),
        })
        .await;
    agent
        .send(AgentMessage::Status(DeploymentReport {
            deployment_id: second_id,
            status: DeploymentStatus::Failed,
            vm_id: None,
            host_port: None,
            error: Some("Build failed".into()),
            commit: None,
        }))
        .await;
    let d = wait_status(&app, &cookie, &second, "failed").await;
    assert_eq!(d["error"], "Build failed");
    let logs = call(
        &app,
        Method::GET,
        &format!("/api/v1/deployments/{second}/logs"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(logs.text, "building...\n");

    assert_eq!(
        deployment_status(&app, &cookie, &first).await["status"],
        "running"
    );
    // Still wanted: a reconnect does not stop it.
    drop(agent);
    let mut agent = Agent::connect(addr, &token).await;
    agent
        .hello(vec![DeploymentReport {
            deployment_id: first_id,
            status: DeploymentStatus::Running,
            vm_id: Some("vm-1".into()),
            host_port: Some(30001),
            error: None,
            commit: Some("abc123".into()),
        }])
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        deployment_status(&app, &cookie, &first).await["status"],
        "running"
    );
    let _ = &mut agent;
}
