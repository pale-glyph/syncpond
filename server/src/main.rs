//! syncpond-server is a small real-time room/key sync server with command/WS APIs.
//!
//! Security assumptions:
//! - `require_tls` requires TLS termination to be handled by an external proxy (nginx/Caddy/traefik).
//! - The command API socket is sensitive and should be bound to loopback or private network.
//! - Command API key and JWT signing keys must be provisioned securely and rotated out of band.
#![deny(missing_docs)]

mod command;
mod downstream;
mod kernel;
mod persistance;
mod state;
mod upstream;

use crate::command::Commands;
use crate::downstream::hub::UpdateChannelMessage;
use crate::downstream::hub::WsHub;
use crate::kernel::KernelSignal;
use crate::state::AppState;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::{fs, net::SocketAddr, sync::Arc};

use tokio::sync::{Mutex, RwLock};
use tracing::info;

#[derive(Debug, Deserialize)]
struct SyncpondConfig {
    command_api_key: String,
    ws_addr: Option<String>,
    upstream_grpc_addr: Option<String>,
    jwt_key: Option<String>,
    jwt_issuer: Option<String>,
    jwt_audience: Option<String>,
    jwt_ttl_seconds: Option<u64>,
    require_tls: Option<bool>,
    ws_allowed_origins: Option<Vec<String>>,
    save_dir: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Configure logging from environment. Prefer `SYNCPOND_LOG`, fall back to `RUST_LOG`,
    // defaulting to `info` when neither is set.
    let log_env = std::env::var("SYNCPOND_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(log_env.clone())
        .init();
    info!(%log_env, "logging initialized");

    let config_path =
        std::env::var("SYNCPOND_CONFIG").unwrap_or_else(|_| "config.yaml".to_string());
    let config_text = fs::read_to_string(config_path)?;
    let config: SyncpondConfig = serde_yaml::from_str(&config_text)?;

    let mut base_state = AppState::new();
    base_state
        .set_command_api_key(config.command_api_key.clone())
        .context("command_api_key must be configured and non-empty")?;
    if let Some(jwt) = config.jwt_key.clone() {
        base_state.set_jwt_key(jwt);
    }
    if let Some(issuer) = config.jwt_issuer.clone() {
        base_state.set_jwt_issuer(issuer);
    }
    if let Some(audience) = config.jwt_audience.clone() {
        base_state.set_jwt_audience(audience);
    }
    if let Some(ttl) = config.jwt_ttl_seconds {
        base_state.set_jwt_ttl(ttl);
    }

    // Configure persistence directory (required)
    match config.save_dir.clone() {
        Some(dir) if !dir.trim().is_empty() => base_state.set_save_dir(dir),
        _ => anyhow::bail!("save_dir must be configured and non-empty"),
    }

    // Ensure save directory exists for per-room RocksDB instances.
    fs::create_dir_all(&base_state.save_dir).context("failed to create save_dir")?;
    info!(path = %base_state.save_dir.display(), "save_dir ready for per-room RocksDBs");

    // Initialize persistence manager and attempt to open existing room DBs.
    let persistence = std::sync::Arc::new(crate::persistance::PersistenceManager::new(
        base_state.save_dir.clone(),
    ));
    persistence.open_existing_room_dbs().ok();

    // Populate in-memory rooms for any existing room DBs and load bucket flags.
    let existing_rooms = persistence.list_room_ids();
    if !existing_rooms.is_empty() {
        for room_id in &existing_rooms {
            let buckets = match persistence.room(*room_id) {
                Some(rp) => rp.load_buckets().unwrap_or_default(),
                None => std::collections::HashMap::new(),
            };
            let mut buckets_map = std::collections::HashMap::new();
            let mut bucket_labels = std::collections::HashMap::new();
            let mut bucket_flags = std::collections::HashMap::new();
            for (id, rec) in buckets {
                buckets_map.insert(id, std::collections::HashMap::new());
                if let Some(lbl) = rec.label {
                    bucket_labels.insert(id, lbl);
                }
                bucket_flags.insert(id, rec.flags);
            }
            let members = match persistence.room(*room_id) {
                Some(rp) => rp.load_members().unwrap_or_default(),
                None => std::collections::HashSet::new(),
            };
            base_state.rooms.insert(
                *room_id,
                Arc::new(std::sync::RwLock::new(crate::state::RoomState {
                    buckets: buckets_map,
                    room_counter: 0,
                    tx_buffer: None,
                    io_locked: false,
                    bucket_labels,
                    bucket_flags,
                    members,
                })),
            );
        }
        // ensure next_room_id is greater than any existing room id
        if let Some(max_id) = existing_rooms.iter().max().copied() {
            if base_state.next_room_id <= max_id {
                base_state.next_room_id = max_id + 1;
            }
        }
    }

    let shared_state = Arc::new(RwLock::new(base_state));

    // Channels for notification, commands (from WS clients -> kernel), and signals (to kernel).
    let (notification_tx, notification_rx) = tokio::sync::mpsc::channel::<UpdateChannelMessage>(1024);
    let (command_tx, command_rx) = tokio::sync::mpsc::channel::<Commands>(1024);
    let (signal_tx, signal_rx) = tokio::sync::mpsc::channel::<KernelSignal>(1024);

    // Inject senders into WsHub so external components can use them.
    let ws_hub = Arc::new(Mutex::new(WsHub::new(
        notification_rx,
        command_tx.clone(),
    )));

    let ws_allowed_origins = config.ws_allowed_origins.clone().unwrap_or_else(|| {
        DEFAULT_WS_ALLOWED_ORIGINS
            .iter()
            .map(|s| s.to_string())
            .collect()
    });

    // Downstream subsystem (forwarder + ws + health) will be started below.

    if config.command_api_key.trim().is_empty() {
        anyhow::bail!("command_api_key must be configured and non-empty");
    }

    let require_tls = config.require_tls.unwrap_or(false);
    if require_tls {
        anyhow::bail!(
            "TLS transport required in config, but this binary does not terminate TLS; use reverse proxy for TLS termination"
        );
    }

    let ws_addr = config
        .ws_addr
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());

    let ws_addr: SocketAddr = ws_addr
        .parse()
        .with_context(|| format!("invalid ws_addr: {}", ws_addr))?;

    let grpc_addr = config
        .upstream_grpc_addr
        .unwrap_or_else(|| "127.0.0.1:50051".to_string());
    let grpc_addr: SocketAddr = grpc_addr
        .parse()
        .with_context(|| format!("invalid upstream_grpc_addr: {}", grpc_addr))?;

    // Spawn the websocket server; pass JWT config so downstream doesn't need SharedState.
    let ws_server = crate::downstream::server::spawn_ws_server(
        ws_addr,
        ws_hub.clone(),
        signal_tx.clone(),
        ws_allowed_origins.clone(),
        config.jwt_key.clone(),
        config.jwt_issuer.clone(),
        config.jwt_audience.clone(),
    );

    // Start the downstream forwarder (no docs/health HTTP server).
    let forwarder = crate::downstream::server::start_downstream(ws_hub.clone()).await?;

    // build the Kernel which coordinates state and downstream components
    let kernel = Arc::new(crate::kernel::SyncpondKernel::new(
        shared_state.clone(),
        ws_hub.clone(),
        persistence.clone(),
        notification_tx.clone(),
    ));

    // Start kernel background processors to handle WS commands and signals.
    let kernel_for_commands = kernel.clone();
    kernel_for_commands.start_command_processor(command_rx);

    let kernel_for_signals = kernel.clone();
    kernel_for_signals.start_signal_processor(signal_rx);

    // start upstream gRPC server for trusted application servers
    let grpc_state = shared_state.clone();
    let grpc_addr_for_task = grpc_addr.clone();
    let kernel_for_grpc = kernel.clone();
    let grpc_server = tokio::spawn(async move {
        info!(
            "syncpond upstream gRPC server listening on {}",
            grpc_addr_for_task
        );
        let server = crate::upstream::GrpcServer::new(
            crate::upstream::CommandServer::new(kernel_for_grpc),
            grpc_state,
        );
        server
            .serve(grpc_addr_for_task)
            .await
            .map_err(|e| anyhow::anyhow!("grpc bind failed: {}", e))?;

        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });

    let shutdown = async {
        tokio::signal::ctrl_c()
            .await
            .context("failed to listen for ctrl-c")?;
        info!("shutdown signal received");
        Ok::<(), anyhow::Error>(())
    };

    tokio::select! {
        res = shutdown => res?,
        res = forwarder => res??,
        res = ws_server => res??,
        res = grpc_server => res??,
    }

    info!("server shutdown complete");
    Ok(())
}

const DEFAULT_WS_ALLOWED_ORIGINS: &[&str] = &[];

// legacy command helpers removed; no constant-time compare helper needed here

// `read_line_with_limit` and health/docs HTTP handlers live under `downstream` now.
