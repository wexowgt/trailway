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
    let state = AppState {
        pool: pool.clone(),
        config: Config::default(),
    };
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
