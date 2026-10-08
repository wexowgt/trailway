use anyhow::Context;
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL must be set")?;
    let bind_addr = std::env::var("API_BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .context("failed to connect to Postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to run migrations")?;
    tracing::info!("connected to Postgres, migrations applied");

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("trailway-api listening on {bind_addr}");
    let prune_pool = pool.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            if let Err(e) = trailway_api::prune_metrics(&prune_pool).await {
                tracing::warn!("could not prune old metrics: {e}");
            }
        }
    });
    let state = trailway_api::AppState::new(pool, trailway_api::Config::from_env()?);
    axum::serve(listener, trailway_api::router(state)).await?;
    Ok(())
}
