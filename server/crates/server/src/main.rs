//! syncpond-server is a small real-time room/key sync server with command/WS APIs.
//!
//! Security assumptions:
//! - `require_tls` requires TLS termination to be handled by an external proxy (nginx/Caddy/traefik).
//! - The command API socket is sensitive and should be bound to loopback or private network.
//! - Command API key and JWT signing keys must be provisioned securely and rotated out of band.
#![deny(missing_docs)]

mod command;

use crate::command::Commands;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use sp_downstream::{Downstream, DownstreamMessage};
use sp_kernel::{AppState, PersistenceManager, RoomState, SyncpondKernel, SERVER_ONLY_BUCKET_ID};
use sp_upstream::{CommandHandler, CommandServer, GrpcServer};
use std::{fs, net::SocketAddr, sync::Arc};

use tokio::sync::RwLock;
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

#[derive(Clone)]
struct KernelCommandHandler {
    kernel: Arc<SyncpondKernel>,
}

#[async_trait]
impl CommandHandler for KernelCommandHandler {
    async fn new_room(&self, name: String) -> u64 {
        match self.kernel.handle_command(Commands::NewRoom(name)).await {
            crate::command::CommandResponse::NewRoomResponse(id) => id,
            _ => 0,
        }
    }

    async fn list_rooms(&self) -> Vec<String> {
        let app = self.kernel.state.read().await;
        let ids = app.list_rooms();
        ids.into_iter()
            .map(|id| {
                let label = app.get_room_label(id).unwrap_or_default();
                format!("{}:{}", id, label)
            })
            .collect()
    }

    async fn delete_room(&self, room_id: u64) -> Result<(), String> {
        self.kernel.delete_room(room_id).await
    }

    async fn issue_jwt(
        &self,
        room_id: u64,
        sub: String,
        buckets: Vec<u64>,
    ) -> Result<String, String> {
        let mut app = self.kernel.state.write().await;
        app.create_room_token(room_id, &sub, &buckets)
            .map_err(|e| e.to_string())
    }

    async fn new_bucket(&self, room_id: u64, bucket_id: u64, label: String) -> Result<(), String> {
        match self
            .kernel
            .handle_command(Commands::NewBucket(room_id, bucket_id, label))
            .await
        {
            crate::command::CommandResponse::NewBucketResponse => Ok(()),
            crate::command::CommandResponse::LoadRoomResponse(Err(e)) => Err(e),
            _ => Err("new_bucket_failed".to_string()),
        }
    }

    async fn delete_bucket(&self, room_id: u64, bucket_id: u64) -> Result<(), String> {
        match self
            .kernel
            .handle_command(Commands::DeleteBucket(room_id, bucket_id))
            .await
        {
            crate::command::CommandResponse::DeleteBucketResponse => Ok(()),
            crate::command::CommandResponse::LoadRoomResponse(Err(e)) => Err(e),
            _ => Err("delete_bucket_failed".to_string()),
        }
    }

    async fn new_member(&self, room_id: u64, member: String) -> Result<(), String> {
        match self
            .kernel
            .handle_command(Commands::NewMember(room_id, member))
            .await
        {
            crate::command::CommandResponse::NewMemberResponse => Ok(()),
            crate::command::CommandResponse::LoadRoomResponse(Err(e)) => Err(e),
            _ => Err("new_member_failed".to_string()),
        }
    }

    async fn delete_member(&self, room_id: u64, member: String) -> Result<(), String> {
        match self
            .kernel
            .handle_command(Commands::DeleteMember(room_id, member))
            .await
        {
            crate::command::CommandResponse::DeleteMemberResponse => Ok(()),
            crate::command::CommandResponse::LoadRoomResponse(Err(e)) => Err(e),
            _ => Err("delete_member_failed".to_string()),
        }
    }

    async fn list_members(&self, room_id: u64) -> Vec<String> {
        let app = self.kernel.state.read().await;
        match app.list_members(room_id) {
            Ok(members) => members,
            Err(_) => Vec::new(),
        }
    }

    async fn list_buckets(&self, room_id: u64) -> Vec<String> {
        let app = self.kernel.state.read().await;
        if let Some(room_arc) = app.rooms.get(&room_id) {
            if let Ok(room) = room_arc.read() {
                let mut ids: Vec<u64> = room.buckets.keys().copied().collect();
                ids.sort_unstable();
                return ids
                    .into_iter()
                    .map(|id| {
                        let label = room.bucket_labels.get(&id).cloned().unwrap_or_default();
                        format!("{}:{}", id, label)
                    })
                    .collect();
            }
        }

        Vec::new()
    }

    async fn read_fragment(
        &self,
        room_id: u64,
        bucket_id: u64,
        key: String,
    ) -> Result<Option<Vec<u8>>, String> {
        match self
            .kernel
            .handle_command(Commands::ReadFragment(room_id, bucket_id, key))
            .await
        {
            crate::command::CommandResponse::FragmentReadResponse(data) => Ok(data),
            crate::command::CommandResponse::LoadRoomResponse(Err(e)) => Err(e),
            _ => Ok(None),
        }
    }

    async fn write_fragment(
        &self,
        room_id: u64,
        bucket_id: u64,
        key: String,
        data: Vec<u8>,
    ) -> Result<(), String> {
        match self
            .kernel
            .handle_command(Commands::WriteFragment(room_id, bucket_id, key, data))
            .await
        {
            crate::command::CommandResponse::FragmentWriteResponse => Ok(()),
            crate::command::CommandResponse::LoadRoomResponse(Err(e)) => Err(e),
            _ => Err("write_fragment_failed".to_string()),
        }
    }

    async fn load_room(&self, room_id: u64) -> Result<(), String> {
        match self
            .kernel
            .handle_command(Commands::LoadRoom(room_id))
            .await
        {
            crate::command::CommandResponse::LoadRoomResponse(result) => result,
            _ => Err("load_room_failed".to_string()),
        }
    }

    async fn unload_room(&self, room_id: u64) -> Result<(), String> {
        match self
            .kernel
            .handle_command(Commands::UnloadRoom(room_id))
            .await
        {
            crate::command::CommandResponse::UnloadRoomResponse(result) => result,
            _ => Err("unload_room_failed".to_string()),
        }
    }
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
    let persistence = std::sync::Arc::new(PersistenceManager::new(base_state.save_dir.clone()));
    persistence.open_existing_room_dbs().ok();

    // Register any existing room DBs as unloaded stubs in memory.
    // Rooms must be explicitly loaded via the LoadRoom RPC before they can be used.
    let existing_rooms = persistence.list_room_ids();
    if !existing_rooms.is_empty() {
        for room_id in &existing_rooms {
            base_state.rooms.insert(
                *room_id,
                Arc::new(std::sync::RwLock::new(RoomState {
                    buckets: std::collections::HashMap::new(),
                    room_counter: 0,
                    tx_buffer: None,
                    io_locked: false,
                    loaded: false,
                    bucket_labels: std::collections::HashMap::new(),
                    bucket_flags: std::collections::HashMap::new(),
                    members: std::collections::HashSet::new(),
                })),
            );
        }
        // ensure next_room_id is greater than any existing room id
        if let Some(max_id) = existing_rooms.iter().max().copied() {
            if base_state.next_room_id <= max_id {
                base_state.next_room_id = max_id + 1;
            }
        }
        info!(
            rooms = existing_rooms.len(),
            "registered existing rooms as unloaded stubs; use LoadRoom to activate"
        );
    }

    let shared_state = Arc::new(RwLock::new(base_state));

    // Channels for upstream commands and downstream websocket messages.
    let (_command_tx, command_rx) = tokio::sync::mpsc::channel::<Commands>(1024);
    let (downstream_tx, downstream_rx) = tokio::sync::mpsc::channel::<DownstreamMessage>(1024);

    let downstream = Downstream::new(downstream_tx.clone());

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
    let ws_server = downstream
        .spawn_ws_server(
            ws_addr,
            downstream_tx.clone(),
            ws_allowed_origins.clone(),
            SERVER_ONLY_BUCKET_ID,
        )
        .await;

    // Start the downstream forwarder (no docs/health HTTP server).
    let forwarder = downstream.start_forwarder().await?;

    // build the Kernel which coordinates state and downstream components
    let kernel = Arc::new(SyncpondKernel::new(
        shared_state.clone(),
        downstream.clone(),
        persistence.clone(),
    ));

    // Start kernel background processors to handle WS commands and signals.
    let kernel_for_commands = kernel.clone();
    kernel_for_commands.start_command_processor(command_rx);

    let kernel_for_downstream = kernel.clone();
    kernel_for_downstream.start_downstream_message_processor(downstream_rx);

    // start upstream gRPC server for trusted application servers
    let grpc_addr_for_task = grpc_addr.clone();
    let grpc_handler = Arc::new(KernelCommandHandler {
        kernel: kernel.clone(),
    });
    let grpc_server = tokio::spawn(async move {
        info!(
            "syncpond upstream gRPC server listening on {}",
            grpc_addr_for_task
        );
        let server = GrpcServer::new(
            CommandServer::new(grpc_handler),
            Some(config.command_api_key.clone()),
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

    async fn join_to_anyhow(
        handle: tokio::task::JoinHandle<Result<(), anyhow::Error>>,
    ) -> anyhow::Result<()> {
        match handle.await {
            Ok(result) => result,
            Err(join_error) => Err(anyhow::anyhow!(join_error)),
        }
    }

    let result = tokio::select! {
        res = shutdown => res,
        res = join_to_anyhow(forwarder) => res,
        res = join_to_anyhow(ws_server) => res,
        res = join_to_anyhow(grpc_server) => res,
    };

    result?;

    info!("server shutdown complete");
    Ok(())
}

const DEFAULT_WS_ALLOWED_ORIGINS: &[&str] = &[];

// legacy command helpers removed; no constant-time compare helper needed here

// `read_line_with_limit` and health/docs HTTP handlers live under `downstream` now.
