use std::time::Duration;

use anyhow::{Context, Result, bail};
use aster_api::{
    config::{DatabaseConfig, ServerConfig},
    router_with_runtime,
    server::serve_with_shutdown,
};
use aster_runtime::{AsterConfig, ConfigHandle, Runtime};
use aster_storage::{Store, StoreOptions};
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
    /// Path to the live configuration file (default: $ASTER_CONFIG or ./aster.toml).
    #[arg(long, global = true)]
    config: Option<String>,
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
    /// Validate server configuration and aster.toml without accessing PostgreSQL.
    CheckConfig,
    /// Open the terminal console against a running `aster serve`.
    Console {
        /// Server URL (default: console.server_url from aster.toml).
        #[arg(long)]
        url: Option<String>,
    },
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
    // The console owns the terminal; log lines would corrupt its screen.
    if !matches!(cli.command, Command::Console { .. }) {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new("aster_api=info,aster_runtime=info")),
            )
            .with_writer(std::io::stderr)
            .init();
    }

    let config_path = cli
        .config
        .or_else(|| std::env::var("ASTER_CONFIG").ok())
        .unwrap_or_else(|| "aster.toml".into());

    match cli.command {
        Command::CheckConfig => {
            let config = ServerConfig::load()?;
            let live = ConfigHandle::load(&config_path).context("aster.toml is invalid")?;
            println!(
                "{}",
                serde_json::json!({
                    "valid": true,
                    "bind_addr": config.bind_addr.to_string(),
                    "config_file": config_path,
                    "config_file_present": std::path::Path::new(&config_path).exists(),
                    "cognition": live.get().cognition.engine,
                })
            );
        }
        Command::Console { url } => {
            let live = ConfigHandle::load(&config_path).context("aster.toml is invalid")?;
            let token = std::env::var("ASTER_OWNER_TOKEN").unwrap_or_default();
            aster_api::auth::OwnerToken::new(&token)?;
            let url = url.unwrap_or_else(|| live.get().console.server_url.clone());
            let console = live.get().console.clone();
            aster_console::run(aster_console::Options {
                server_url: url,
                token,
                show_trace: console.show_trace,
                training_prompt_after_reply: console.training_prompt_after_reply,
                history_turns: console.history_turns,
            })
            .await?;
        }
        Command::Init => {
            let config = DatabaseConfig::load()?;
            let store = connect(&config, &AsterConfig::default()).await?;
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
            let store = connect(&config, &AsterConfig::default()).await?;
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
        Command::Serve => {
            let live = ConfigHandle::load(&config_path).context("aster.toml is invalid")?;
            serve(ServerConfig::load()?, live).await?
        }
    }
    Ok(())
}

async fn connect(config: &DatabaseConfig, live: &AsterConfig) -> Result<Store> {
    let options = StoreOptions {
        max_connections: live.database.pool_size,
        acquire_timeout: Duration::from_millis(live.database.acquire_timeout_ms),
        statement_timeout: Duration::from_millis(live.database.statement_timeout_ms),
    };
    Store::connect_with(&config.url, options)
        .await
        .map_err(|_| {
            anyhow::anyhow!("database connection failed; check PostgreSQL and DATABASE_URL")
        })
}

async fn migrate(store: &Store) -> Result<()> {
    store.migrate().await.map_err(|_| {
        anyhow::anyhow!("database migration failed; check schema and migration permissions")
    })
}

async fn serve(config: ServerConfig, live: ConfigHandle) -> Result<()> {
    let store = connect(&config.database, &live.get()).await?;
    migrate(&store).await?;
    if !store.is_ready().await.map_err(|_| {
        anyhow::anyhow!("unable to validate persisted identity, state, and audit history")
    })? {
        bail!("Aster is not initialized; run `aster init` explicitly first");
    }
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .context("unable to bind the configured API address")?;
    let runtime = Runtime::start(store.clone(), live.clone())
        .await
        .map_err(|error| anyhow::anyhow!("unable to start the runtime: {error}"))?;
    tracing::info!(
        bind_addr = %config.bind_addr,
        config = %live.path().display(),
        phase = 3,
        "Aster API and runtime started"
    );
    let result = serve_with_shutdown(
        listener,
        router_with_runtime(store.clone(), config.owner_token, runtime.clone()),
        shutdown_signal(),
        Duration::from_millis(live.get().server.shutdown_drain_ms),
    )
    .await;
    runtime.shutdown().await;
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
