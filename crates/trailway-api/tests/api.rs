//! Integration tests against Postgres. Set `TEST_DATABASE_URL` (CI does, see
//! `.github/workflows/ci.yml`); without it every test is skipped so plain
//! `cargo test` keeps working without a database.

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tower::ServiceExt;
use trailway_api::{router, AppState, Config};

async fn setup() -> Option<(Router, PgPool)> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("connect to TEST_DATABASE_URL");
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool.clone(), Config::default());
    Some((router(state), pool))
}

fn unique_email() -> String {
    format!("{}@example.com", uuid::Uuid::new_v4())
}

struct Reply {
    status: StatusCode,
    cookie: Option<String>,
    json: Value,
}

async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> Reply {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
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
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Reply {
        status,
        cookie,
        json,
    }
}

async fn signup(app: &Router, email: &str) -> String {
    let r = call(
        app,
        Method::POST,
        "/api/v1/auth/signup",
        None,
        Some(json!({"email": email, "password": "password123"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    r.cookie.expect("session cookie")
}

#[tokio::test]
async fn signup_me_logout_login_flow() {
    let Some((app, pool)) = setup().await else {
        return;
    };
    let email = unique_email();
    let cookie = signup(&app, &email).await;
    assert!(cookie.starts_with("tw_session="));

    let me = call(&app, Method::GET, "/api/v1/me", Some(&cookie), None).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json["email"], email);
    assert!(me.json.get("password_hash").is_none());

    // password stored as argon2, never in clear
    let stored: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE email = $1")
        .bind(&email)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stored.starts_with("$argon2"));

    let out = call(
        &app,
        Method::POST,
        "/api/v1/auth/logout",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    let me = call(&app, Method::GET, "/api/v1/me", Some(&cookie), None).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);

    let login = call(
        &app,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(json!({"email": email.to_uppercase(), "password": "password123"})),
    )
    .await;
    assert_eq!(login.status, StatusCode::OK);
    let me = call(
        &app,
        Method::GET,
        "/api/v1/me",
        login.cookie.as_deref(),
        None,
    )
    .await;
    assert_eq!(me.status, StatusCode::OK);
}

#[tokio::test]
async fn signup_validation_and_duplicates() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let email = unique_email();
    for (body, status) in [
        (json!({"email": "nope", "password": "password123"}), 400),
        (json!({"email": email, "password": "short"}), 400),
        (json!({"email": email}), 400),
    ] {
        let r = call(&app, Method::POST, "/api/v1/auth/signup", None, Some(body)).await;
        assert_eq!(r.status.as_u16(), status);
        assert_eq!(r.json["error"]["code"], "invalid_request");
    }
    signup(&app, &email).await;
    let dup = call(
        &app,
        Method::POST,
        "/api/v1/auth/signup",
        None,
        Some(json!({"email": email.to_uppercase(), "password": "password123"})),
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    assert_eq!(dup.json["error"]["code"], "conflict");
}

#[tokio::test]
async fn login_rejects_bad_credentials_uniformly() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let email = unique_email();
    signup(&app, &email).await;
    let wrong = call(
        &app,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(json!({"email": email, "password": "not-the-password"})),
    )
    .await;
    let unknown = call(
        &app,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(json!({"email": unique_email(), "password": "password123"})),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.json, unknown.json);
}

#[tokio::test]
async fn expired_session_is_rejected() {
    let Some((app, pool)) = setup().await else {
        return;
    };
    let email = unique_email();
    let cookie = signup(&app, &email).await;
    sqlx::query(
        "UPDATE sessions SET expires_at = now() - interval '1 second' \
         WHERE user_id = (SELECT id FROM users WHERE email = $1)",
    )
    .bind(&email)
    .execute(&pool)
    .await
    .unwrap();
    let me = call(&app, Method::GET, "/api/v1/me", Some(&cookie), None).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn server_keys_require_auth() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let r = call(&app, Method::GET, "/api/v1/server-keys", None, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn server_key_create_list_revoke() {
    let Some((app, pool)) = setup().await else {
        return;
    };
    let cookie = signup(&app, &unique_email()).await;

    let bad = call(
        &app,
        Method::POST,
        "/api/v1/server-keys",
        Some(&cookie),
        Some(json!({"name": "  "})),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);

    let created = call(
        &app,
        Method::POST,
        "/api/v1/server-keys",
        Some(&cookie),
        Some(json!({"name": "home lab"})),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let secret = created.json["secret"].as_str().unwrap().to_string();
    let id = created.json["id"].as_str().unwrap().to_string();
    assert!(secret.starts_with("tw_sk_"));
    assert_eq!(created.json["name"], "home lab");

    // stored hashed, not in clear
    let (hash, prefix): (String, String) =
        sqlx::query_as("SELECT key_hash, prefix FROM server_keys WHERE id = $1::uuid")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(hash, secret);
    assert!(secret.starts_with(&prefix));
    assert!(prefix.len() < secret.len());

    let list = call(
        &app,
        Method::GET,
        "/api/v1/server-keys",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(list.status, StatusCode::OK);
    let items = list.json.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].get("secret").is_none());
    assert!(items[0]["revoked_at"].is_null());
    assert!(!list.json.to_string().contains(&secret));

    let uri = format!("/api/v1/server-keys/{id}");
    let revoked = call(&app, Method::DELETE, &uri, Some(&cookie), None).await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    let again = call(&app, Method::DELETE, &uri, Some(&cookie), None).await;
    assert_eq!(again.status, StatusCode::NO_CONTENT);
    let list = call(
        &app,
        Method::GET,
        "/api/v1/server-keys",
        Some(&cookie),
        None,
    )
    .await;
    assert!(!list.json[0]["revoked_at"].is_null());
}

#[tokio::test]
async fn server_keys_are_scoped_to_their_owner() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let alice = signup(&app, &unique_email()).await;
    let bob = signup(&app, &unique_email()).await;
    let created = call(
        &app,
        Method::POST,
        "/api/v1/server-keys",
        Some(&alice),
        Some(json!({"name": "alice"})),
    )
    .await;
    let id = created.json["id"].as_str().unwrap();

    let list = call(&app, Method::GET, "/api/v1/server-keys", Some(&bob), None).await;
    assert_eq!(list.json.as_array().unwrap().len(), 0);
    let uri = format!("/api/v1/server-keys/{id}");
    let del = call(&app, Method::DELETE, &uri, Some(&bob), None).await;
    assert_eq!(del.status, StatusCode::NOT_FOUND);
    let missing = format!("/api/v1/server-keys/{}", uuid::Uuid::new_v4());
    let del = call(&app, Method::DELETE, &missing, Some(&alice), None).await;
    assert_eq!(del.status, StatusCode::NOT_FOUND);
}

// ---- agent register / heartbeat / servers ----

async fn call_bearer(
    app: &Router,
    method: Method,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> Reply {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    let body = match body {
        Some(b) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        cookie: None,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

async fn create_key(app: &Router, cookie: &str) -> (String, String) {
    let r = call(
        app,
        Method::POST,
        "/api/v1/server-keys",
        Some(cookie),
        Some(json!({"name": "box"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    (
        r.json["id"].as_str().unwrap().to_string(),
        r.json["secret"].as_str().unwrap().to_string(),
    )
}

fn register_body(machine_id: &str) -> Value {
    json!({"machine_id": machine_id, "hostname": "box1", "agent_version": "0.1.0"})
}

fn heartbeat_body() -> Value {
    json!({
        "agent_version": "0.1.0",
        "cpu": {"total": 4000, "used": 500},
        "memory": {"total": 8_000_000_000u64, "used": 2_000_000_000u64},
        "disk": {"total": 100_000_000_000u64, "used": 40_000_000_000u64},
        "kvm": true
    })
}

async fn list_servers(app: &Router, cookie: &str) -> Vec<Value> {
    let r = call(app, Method::GET, "/api/v1/servers", Some(cookie), None).await;
    assert_eq!(r.status, StatusCode::OK);
    r.json.as_array().unwrap().clone()
}

#[tokio::test]
async fn register_heartbeat_and_list() {
    let Some((app, pool)) = setup().await else {
        return;
    };
    let cookie = signup(&app, &unique_email()).await;
    let (_, secret) = create_key(&app, &cookie).await;
    let machine = uuid::Uuid::new_v4().to_string();

    let reg = call_bearer(
        &app,
        Method::POST,
        "/api/v1/agent/register",
        &secret,
        Some(register_body(&machine)),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED);
    let token = reg.json["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("tw_st_"));

    // registered but silent: offline, no capacity yet
    let servers = list_servers(&app, &cookie).await;
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["status"], "offline");
    assert!(servers[0]["last_heartbeat_at"].is_null());

    let hb = call_bearer(
        &app,
        Method::POST,
        "/api/v1/agent/heartbeat",
        &token,
        Some(heartbeat_body()),
    )
    .await;
    assert_eq!(hb.status, StatusCode::NO_CONTENT);
    let servers = list_servers(&app, &cookie).await;
    assert_eq!(servers[0]["status"], "online");
    assert_eq!(servers[0]["hostname"], "box1");
    assert_eq!(servers[0]["cpu"]["total"], 4000);
    assert_eq!(servers[0]["memory"]["used"], 2_000_000_000u64);
    assert_eq!(servers[0]["kvm"], true);
    assert!(servers[0]["last_heartbeat_at"].is_string());

    // token stored hashed
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM servers WHERE machine_id = $1")
        .bind(&machine)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(stored, token);

    // 31 s without a heartbeat: offline
    sqlx::query(
        "UPDATE servers SET last_heartbeat_at = now() - interval '31 seconds' WHERE machine_id = $1",
    )
    .bind(&machine)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(list_servers(&app, &cookie).await[0]["status"], "offline");

    // bad payloads
    let mut bad = heartbeat_body();
    bad["cpu"] = json!({"total": 1, "used": 2});
    let r = call_bearer(
        &app,
        Method::POST,
        "/api/v1/agent/heartbeat",
        &token,
        Some(bad),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn servers_are_scoped_to_their_owner() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let alice = signup(&app, &unique_email()).await;
    let bob = signup(&app, &unique_email()).await;
    let (_, secret) = create_key(&app, &alice).await;
    let reg = call_bearer(
        &app,
        Method::POST,
        "/api/v1/agent/register",
        &secret,
        Some(register_body(&uuid::Uuid::new_v4().to_string())),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED);
    assert_eq!(list_servers(&app, &alice).await.len(), 1);
    assert!(list_servers(&app, &bob).await.is_empty());

    let anon = call(&app, Method::GET, "/api/v1/servers", None, None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoked_or_unknown_key_cannot_register() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let cookie = signup(&app, &unique_email()).await;
    let (id, secret) = create_key(&app, &cookie).await;
    let uri = format!("/api/v1/server-keys/{id}");
    let r = call(&app, Method::DELETE, &uri, Some(&cookie), None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);

    for token in [secret.as_str(), "tw_sk_doesnotexist", "garbage"] {
        let r = call_bearer(
            &app,
            Method::POST,
            "/api/v1/agent/register",
            token,
            Some(register_body("m")),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{token}");
    }
    assert!(list_servers(&app, &cookie).await.is_empty());
}

#[tokio::test]
async fn token_kinds_are_not_interchangeable() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let cookie = signup(&app, &unique_email()).await;
    let (_, secret) = create_key(&app, &cookie).await;
    let reg = call_bearer(
        &app,
        Method::POST,
        "/api/v1/agent/register",
        &secret,
        Some(register_body("m1")),
    )
    .await;
    let token = reg.json["token"].as_str().unwrap().to_string();

    // a server token is not a user credential
    for uri in ["/api/v1/me", "/api/v1/servers", "/api/v1/server-keys"] {
        let r = call_bearer(&app, Method::GET, uri, &token, None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
        let r = call(
            &app,
            Method::GET,
            uri,
            Some(&format!("tw_session={token}")),
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri} as cookie");
    }
    // a server key cannot heartbeat, a server token cannot register, a cookie neither
    let r = call_bearer(
        &app,
        Method::POST,
        "/api/v1/agent/heartbeat",
        &secret,
        Some(heartbeat_body()),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = call_bearer(
        &app,
        Method::POST,
        "/api/v1/agent/register",
        &token,
        Some(register_body("m1")),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = call(
        &app,
        Method::POST,
        "/api/v1/agent/heartbeat",
        Some(&cookie),
        Some(heartbeat_body()),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reinstall_does_not_duplicate_the_server() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let cookie = signup(&app, &unique_email()).await;
    let (_, secret) = create_key(&app, &cookie).await;
    let (_, second_key) = create_key(&app, &cookie).await;
    let machine = uuid::Uuid::new_v4().to_string();
    let register = |key: String| {
        let app = app.clone();
        let body = register_body(&machine);
        async move {
            call_bearer(
                &app,
                Method::POST,
                "/api/v1/agent/register",
                &key,
                Some(body),
            )
            .await
        }
    };

    let first = register(secret).await;
    // same host, another key of the same owner
    let second = register(second_key).await;
    assert_eq!(first.json["server_id"], second.json["server_id"]);
    assert_eq!(list_servers(&app, &cookie).await.len(), 1);

    // old token is invalidated by the re-register, the new one works
    let old = first.json["token"].as_str().unwrap();
    let new = second.json["token"].as_str().unwrap();
    let r = call_bearer(
        &app,
        Method::POST,
        "/api/v1/agent/heartbeat",
        old,
        Some(heartbeat_body()),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = call_bearer(
        &app,
        Method::POST,
        "/api/v1/agent/heartbeat",
        new,
        Some(heartbeat_body()),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);

    // the same machine id under another user is a separate server
    let other = signup(&app, &unique_email()).await;
    let (_, other_key) = create_key(&app, &other).await;
    let r = register(other_key).await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_ne!(r.json["server_id"], first.json["server_id"]);
    assert_eq!(list_servers(&app, &other).await.len(), 1);
    assert_eq!(list_servers(&app, &cookie).await.len(), 1);
}

#[tokio::test]
async fn install_script_is_served() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let res = app
        .clone()
        .oneshot(Request::get("/install.sh").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.starts_with(b"#!/bin/sh"));
    // downloads are off without API_AGENT_DIST_DIR
    let res = app
        .oneshot(
            Request::get("/downloads/trailway-agent-linux-x86_64")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

// ---- ledger ----

struct Box {
    cookie: String,
    token: String,
    server_id: String,
}

async fn online_box(app: &Router) -> Box {
    let cookie = signup(app, &unique_email()).await;
    let (_, secret) = create_key(app, &cookie).await;
    let reg = call_bearer(
        app,
        Method::POST,
        "/api/v1/agent/register",
        &secret,
        Some(register_body(&uuid::Uuid::new_v4().to_string())),
    )
    .await;
    let token = reg.json["token"].as_str().unwrap().to_string();
    let hb = call_bearer(
        app,
        Method::POST,
        "/api/v1/agent/heartbeat",
        &token,
        Some(heartbeat_body()),
    )
    .await;
    assert_eq!(hb.status, StatusCode::NO_CONTENT);
    Box {
        cookie,
        token,
        server_id: reg.json["server_id"].as_str().unwrap().to_string(),
    }
}

/// Sample ending `ago` seconds ago and lasting `len` seconds.
fn sample_body(ago: i64, len: i64, cpu_used: u64, kvm: bool) -> Value {
    let end = chrono::Utc::now() - chrono::Duration::seconds(ago);
    json!({
        "period_start": end - chrono::Duration::seconds(len),
        "period_end": end,
        "cpu": {"total": 4000, "used": cpu_used},
        "memory": {"total": 8u64 << 30, "used": 2u64 << 30},
        "kvm": kvm
    })
}

async fn send_sample(app: &Router, b: &Box, body: Value) -> Reply {
    call_bearer(
        app,
        Method::POST,
        "/api/v1/agent/usage",
        &b.token,
        Some(body),
    )
    .await
}

async fn balance(app: &Router, b: &Box) -> Value {
    let r = call(
        app,
        Method::GET,
        "/api/v1/ledger/balance",
        Some(&b.cookie),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    r.json
}

#[tokio::test]
async fn idle_capacity_is_credited_and_own_usage_is_not() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let b = online_box(&app).await;
    // 10 s with 1 of 4 cores and 2 of 8 GiB in use: 3 vCPU and 6 GiB idle.
    let r = send_sample(&app, &b, sample_body(0, 10, 1000, true)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["outcome"], "credited");
    let bal = balance(&app, &b).await;
    assert_eq!(bal["contributed"]["vcpu_seconds"], 30.0);
    assert_eq!(bal["contributed"]["gb_seconds"], 60.0);
    assert_eq!(bal["consumed"]["vcpu_seconds"], 0.0);
    assert_eq!(bal["balance"]["vcpu_seconds"], 30.0);

    // Fully busy server earns nothing more.
    let r = send_sample(&app, &b, sample_body(20, 10, 4000, true)).await;
    assert_eq!(r.json["outcome"], "credited", "memory is still idle");
    let bal = balance(&app, &b).await;
    assert_eq!(bal["contributed"]["vcpu_seconds"], 30.0, "no idle cpu");
    assert_eq!(
        bal["contributed"]["gb_seconds"], 120.0,
        "idle memory still counts"
    );
}

#[tokio::test]
async fn offline_stale_and_no_kvm_earn_nothing() {
    let Some((app, pool)) = setup().await else {
        return;
    };
    let b = online_box(&app).await;
    let no_kvm = send_sample(&app, &b, sample_body(0, 60, 0, false)).await;
    assert_eq!(no_kvm.json["outcome"], "uncredited");
    let stale = send_sample(&app, &b, sample_body(600, 60, 0, true)).await;
    assert_eq!(stale.json["outcome"], "uncredited");

    sqlx::query(
        "UPDATE servers SET last_heartbeat_at = now() - interval '31 seconds' WHERE id = $1::uuid",
    )
    .bind(&b.server_id)
    .execute(&pool)
    .await
    .unwrap();
    let offline = send_sample(&app, &b, sample_body(120, 60, 0, true)).await;
    assert_eq!(offline.json["outcome"], "uncredited");
    let bal = balance(&app, &b).await;
    assert_eq!(bal["contributed"]["vcpu_seconds"], 0.0);
    assert_eq!(bal["contributed"]["gb_seconds"], 0.0);
}

#[tokio::test]
async fn duplicate_and_overlapping_samples_count_once_out_of_order_ok() {
    let Some((app, _)) = setup().await else {
        return;
    };
    let b = online_box(&app).await;
    let later = sample_body(0, 20, 0, true);
    let earlier = sample_body(25, 20, 0, true);
    assert_eq!(
        send_sample(&app, &b, later.clone()).await.json["outcome"],
        "credited"
    );
    assert_eq!(
        send_sample(&app, &b, later.clone()).await.json["outcome"],
        "duplicate"
    );
    // Shifted by 10 s: overlaps the stored one.
    let overlap = sample_body(10, 20, 0, true);
    assert_eq!(
        send_sample(&app, &b, overlap).await.json["outcome"],
        "duplicate"
    );
    // An older, non-overlapping sample arriving afterwards is credited once.
    assert_eq!(
        send_sample(&app, &b, earlier.clone()).await.json["outcome"],
        "credited"
    );
    assert_eq!(
        send_sample(&app, &b, earlier).await.json["outcome"],
        "duplicate"
    );
    // 2 x 20 s x 4 vCPU
    assert_eq!(
        balance(&app, &b).await["contributed"]["vcpu_seconds"],
        160.0
    );
}

#[tokio::test]
async fn entries_page_and_series_and_ownership() {
    let Some((app, pool)) = setup().await else {
        return;
    };
    let b = online_box(&app).await;
    for i in 0..3 {
        let r = send_sample(&app, &b, sample_body(i * 12, 10, 0, true)).await;
        assert_eq!(r.json["outcome"], "credited");
    }
    let p1 = call(
        &app,
        Method::GET,
        "/api/v1/ledger/entries?limit=2",
        Some(&b.cookie),
        None,
    )
    .await;
    assert_eq!(p1.json["entries"].as_array().unwrap().len(), 2);
    assert_eq!(p1.json["entries"][0]["kind"], "contributed");
    let cursor = p1.json["next_before"].as_i64().unwrap();
    let p2 = call(
        &app,
        Method::GET,
        &format!("/api/v1/ledger/entries?limit=2&before={cursor}"),
        Some(&b.cookie),
        None,
    )
    .await;
    assert_eq!(p2.json["entries"].as_array().unwrap().len(), 1);
    assert!(p2.json["next_before"].is_null());

    let series = call(
        &app,
        Method::GET,
        &format!("/api/v1/servers/{}/usage", b.server_id),
        Some(&b.cookie),
        None,
    )
    .await;
    assert_eq!(series.status, StatusCode::OK);
    let pts = series.json.as_array().unwrap();
    assert_eq!(pts.len(), 3);
    assert!(
        pts[0]["period_end"].as_str() < pts[2]["period_end"].as_str(),
        "oldest first"
    );
    assert_eq!(pts[0]["credited"], true);

    // Another user can neither see the series nor the ledger entries.
    let other = signup(&app, &unique_email()).await;
    let r = call(
        &app,
        Method::GET,
        &format!("/api/v1/servers/{}/usage", b.server_id),
        Some(&other),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(
        &app,
        Method::GET,
        "/api/v1/ledger/entries",
        Some(&other),
        None,
    )
    .await;
    assert_eq!(r.json["entries"].as_array().unwrap().len(), 0);

    // The ledger is append-only.
    let upd = sqlx::query("UPDATE ledger_entries SET millicore_seconds = 0")
        .execute(&pool)
        .await;
    assert!(upd.is_err());

    // Auth is required.
    let r = call(&app, Method::GET, "/api/v1/ledger/balance", None, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = send_sample(&app, &b, sample_body(0, 600, 0, true)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}
