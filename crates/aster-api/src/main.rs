use std::time::Duration;

use anyhow::{Context, Result, bail};
use aster_api::{
    config::{DatabaseConfig, ServerConfig},
    router,
    server::serve_with_shutdown,
};
use aster_storage::Store;
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "aster",
    version,
    about = "Aster's private persistent foundation"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Apply migrations and create identity only if it does not already exist.
    Init,
    /// Start the private API; requires explicit prior initialization.
    Serve,
    /// Inspect database readiness without initializing or migrating it.
    Status,
    /// Validate server configuration without accessing PostgreSQL.
    CheckConfig,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // Only load this working directory's .env, never search parent directories.
    match dotenvy::from_path(".env") {
        Ok(()) => {}
        Err(dotenvy::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => bail!("unable to load .env; check its syntax and file permissions"),
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("aster_api=info")),
        )
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Command::CheckConfig => {
            let config = ServerConfig::load()?;
            println!(
                "{}",
                serde_json::json!({ "valid": true, "bind_addr": config.bind_addr.to_string() })
            );
        }
        Command::Init => {
            let config = DatabaseConfig::load()?;
            let store = connect(&config).await?;
            migrate(&store).await?;
            let outcome = store.initialize().await.map_err(|_| {
                anyhow::anyhow!("identity initialization failed; database state has not been reset")
            })?;
            println!(
                "{}",
                serde_json::json!({ "identity_id": outcome.identity.id, "created": outcome.created })
            );
            store.close().await;
        }
        Command::Status => {
            let config = DatabaseConfig::load()?;
            let store = connect(&config).await?;
            let ready = store
                .is_ready()
                .await
                .map_err(|_| anyhow::anyhow!("unable to read database readiness"))?;
            let identity_id = if ready {
                store
                    .identity()
                    .await
                    .map_err(|_| anyhow::anyhow!("unable to read persisted identity"))?
                    .map(|identity| identity.id)
            } else {
                None
            };
            println!(
                "{}",
                serde_json::json!({ "ready": ready, "identity_id": identity_id, "runtime_enabled": false })
            );
            store.close().await;
        }
        Command::Serve => serve(ServerConfig::load()?).await?,
    }
    Ok(())
}

async fn connect(config: &DatabaseConfig) -> Result<Store> {
    Store::connect(&config.url).await.map_err(|_| {
        anyhow::anyhow!("database connection failed; check PostgreSQL and DATABASE_URL")
    })
}

async fn migrate(store: &Store) -> Result<()> {
    store.migrate().await.map_err(|_| {
        anyhow::anyhow!("database migration failed; check schema and migration permissions")
    })
}

async fn serve(config: ServerConfig) -> Result<()> {
    let store = connect(&config.database).await?;
    migrate(&store).await?;
    if !store.is_ready().await.map_err(|_| {
        anyhow::anyhow!("unable to validate persisted identity, state, and audit history")
    })? {
        bail!("Aster is not initialized; run `aster init` explicitly first");
    }
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .context("unable to bind the configured API address")?;
    tracing::info!(bind_addr = %config.bind_addr, phase = 1, "Aster API started; autonomous runtime is not enabled");
    let result = serve_with_shutdown(
        listener,
        router(store.clone(), config.owner_token),
        shutdown_signal(),
        Duration::from_secs(5),
    )
    .await;
    // Bound pool shutdown; do not hang indefinitely on a broken database.
    let _ = tokio::time::timeout(Duration::from_secs(5), store.close()).await;
    result.context("API server failed")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            tracing::error!("unable to register interrupt handler; shutting down safely");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => {
                tracing::error!("unable to register termination handler; shutting down safely")
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown requested");
}
