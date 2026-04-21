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
mod rate_limiter;
mod state;
mod upstream;

use crate::downstream::hub::ClientMessage;
use crate::downstream::connection::handle_ws_connection;
use crate::downstream::hub::{DataUpdate, WsHub};
use crate::kernel::KernelSignal;
use crate::rate_limiter::RateLimiter;
use crate::state::{AppState, SharedState};
use anyhow::{Context, Result};
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use std::{fs, net::SocketAddr, path::Path, sync::Arc};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    net::{TcpListener, TcpStream},
    sync::{Mutex, RwLock},
};
use tracing::{error, info};

#[derive(Debug, Deserialize)]
struct SyncpondConfig {
    command_api_key: String,
    ws_addr: Option<String>,
    upstream_grpc_addr: Option<String>,
    health_addr: Option<String>,
    jwt_key: Option<String>,
    jwt_issuer: Option<String>,
    jwt_audience: Option<String>,
    jwt_ttl_seconds: Option<u64>,
    require_tls: Option<bool>,
    health_bind_loopback_only: Option<bool>,
    ws_auth_rate_limit: Option<usize>,
    ws_auth_rate_window_secs: Option<u64>,
    ws_room_rate_limit: Option<usize>,
    ws_room_rate_window_secs: Option<u64>,
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
            base_state.rooms.insert(
                *room_id,
                Arc::new(std::sync::RwLock::new(crate::state::RoomState {
                    buckets: buckets_map,
                    room_counter: 0,
                    tx_buffer: None,
                    io_locked: false,
                    bucket_labels,
                    bucket_flags,
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
    let (notification_tx, notification_rx) = tokio::sync::mpsc::channel::<DataUpdate>(1024);
    let (command_tx, command_rx) = tokio::sync::mpsc::channel::<ClientMessage>(1024);
    let (signal_tx, signal_rx) = tokio::sync::mpsc::channel::<KernelSignal>(1024);

    // Inject senders into WsHub so external components can use them.
    let ws_hub = Arc::new(Mutex::new(WsHub::new(
        notification_rx,
        command_tx.clone(),
        signal_tx.clone(),
    )));
    let ws_auth_rate_limiter = Arc::new(RateLimiter::new());
    let ws_room_rate_limiter = Arc::new(RateLimiter::new());

    let ws_room_rate_limiter_for_ws = ws_room_rate_limiter.clone();

    let ws_auth_rate_limit = config
        .ws_auth_rate_limit
        .unwrap_or(DEFAULT_WS_AUTH_RATE_LIMIT);
    let ws_auth_rate_window_secs = config
        .ws_auth_rate_window_secs
        .unwrap_or(DEFAULT_WS_AUTH_RATE_WINDOW_SECS);
    let ws_room_rate_limit = config
        .ws_room_rate_limit
        .unwrap_or(DEFAULT_WS_ROOM_RATE_LIMIT);
    let ws_room_rate_window_secs = config
        .ws_room_rate_window_secs
        .unwrap_or(DEFAULT_WS_ROOM_RATE_WINDOW_SECS);
    let ws_allowed_origins = config.ws_allowed_origins.clone().unwrap_or_else(|| {
        DEFAULT_WS_ALLOWED_ORIGINS
            .iter()
            .map(|s| s.to_string())
            .collect()
    });

    // Spawn the forwarder task that reads notifications and forwards to connected WS clients.
    ws_hub.lock().await.start().await;

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
    let health_addr = config
        .health_addr
        .unwrap_or_else(|| "127.0.0.1:7070".to_string());
    let health_bind_loopback_only = config.health_bind_loopback_only.unwrap_or(true);

    let ws_addr: SocketAddr = ws_addr
        .parse()
        .with_context(|| format!("invalid ws_addr: {}", ws_addr))?;
    let health_addr: SocketAddr = health_addr
        .parse()
        .with_context(|| format!("invalid health_addr: {}", health_addr))?;

    let grpc_addr = config
        .upstream_grpc_addr
        .unwrap_or_else(|| "127.0.0.1:50051".to_string());
    let grpc_addr: SocketAddr = grpc_addr
        .parse()
        .with_context(|| format!("invalid upstream_grpc_addr: {}", grpc_addr))?;

    if health_bind_loopback_only && !health_addr.ip().is_loopback() {
        anyhow::bail!("health_bind_loopback_only=true but health_addr is not loopback");
    }

    let ws_state = shared_state.clone();
    let ws_hub_for_ws = ws_hub.clone();

    let ws_addr_for_task = ws_addr.clone();

    let ws_server = tokio::spawn(async move {
        let listener = TcpListener::bind(ws_addr_for_task)
            .await
            .context("ws bind failed")?;
        info!(
            "syncpond websocket server listening on {}",
            ws_addr_for_task
        );

        loop {
            let (stream, peer) = listener.accept().await?;
            let state = ws_state.clone();
            let hub = ws_hub_for_ws.clone();
            let auth_limiter = ws_auth_rate_limiter.clone();
            let room_limiter = ws_room_rate_limiter_for_ws.clone();
            let ws_allowed_origins_for_conn = ws_allowed_origins.clone();
            tokio::spawn(async move {
                if let Err(err) = handle_ws_or_docs_connection(
                    stream,
                    peer,
                    state,
                    hub,
                    auth_limiter,
                    room_limiter,
                    ws_auth_rate_limit,
                    ws_auth_rate_window_secs,
                    ws_allowed_origins_for_conn,
                )
                .await
                {
                    error!(%err, peer = %peer, "ws/http connection error");
                }
            });
        }

        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });

    // legacy TCP command server removed; use upstream gRPC for trusted application servers

    let health_state = shared_state.clone();
    let health_addr_for_task = health_addr.clone();
    let health_server = tokio::spawn(async move {
        info!(
            "syncpond health server listening on {}",
            health_addr_for_task
        );
        let listener = TcpListener::bind(health_addr_for_task)
            .await
            .context("health bind failed")?;

        loop {
            let (stream, peer) = listener.accept().await?;
            let state = health_state.clone();
            tokio::spawn(async move {
                if health_bind_loopback_only && !peer.ip().is_loopback() {
                    error!(%peer, "rejected non-loopback health connection");
                    return;
                }

                if let Err(err) = handle_health_connection(stream, state).await {
                    error!(%err, peer = %peer, "health connection error");
                }
            });
        }

        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });

    // build the Kernel which coordinates state and downstream components
    let kernel = Arc::new(crate::kernel::SyncpondKernel::new(
        shared_state.clone(),
        ws_hub.clone(),
        ws_room_rate_limiter.clone(),
        ws_room_rate_limit,
        ws_room_rate_window_secs,
        persistence.clone(),
    ));

    // Start kernel background processors to handle commands and signals from WS.
    let kernel_for_cmds = kernel.clone();
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
        res = ws_server => res??,
        res = health_server => res??,
        res = grpc_server => res??,
    }

    info!("server shutdown complete");
    Ok(())
}

const MAX_COMMAND_LINE_LEN: usize = 8192;

const DEFAULT_WS_AUTH_RATE_LIMIT: usize = 10;
const DEFAULT_WS_AUTH_RATE_WINDOW_SECS: u64 = 60;
const DEFAULT_WS_ROOM_RATE_LIMIT: usize = 1000;
const DEFAULT_WS_ROOM_RATE_WINDOW_SECS: u64 = 60;
const DEFAULT_WS_ALLOWED_ORIGINS: &[&str] = &[];

// legacy command helpers removed; no constant-time compare helper needed here

async fn read_line_with_limit<R>(reader: &mut BufReader<R>, line: &mut String) -> Result<usize>
where
    R: tokio::io::AsyncRead + Unpin,
{
    line.clear();
    let mut total = 0usize;
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            break;
        }
        let newline_pos = buf.iter().position(|&b| b == b'\n');
        let consume_len = newline_pos.map(|p| p + 1).unwrap_or(buf.len());
        total += consume_len;
        if total > MAX_COMMAND_LINE_LEN {
            anyhow::bail!("line_too_long");
        }
        let chunk = std::str::from_utf8(&buf[..consume_len])
            .map_err(|e| anyhow::anyhow!("invalid utf8: {}", e))?;
        line.push_str(chunk);
        reader.consume(consume_len);
        if newline_pos.is_some() {
            break;
        }
    }
    Ok(total)
}

async fn handle_health_connection(stream: TcpStream, state: SharedState) -> Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);
    let mut line = String::new();

    let bytes = read_line_with_limit(&mut reader, &mut line).await?;
    if bytes == 0 {
        return Ok(());
    }

    let parts: Vec<&str> = line.trim_end().split_whitespace().collect();
    let (status, body) = if parts.len() >= 2 && parts[0] == "GET" {
        match parts[1] {
            "/health" => ("200 OK", "ok".to_string()),
            "/metrics" => {
                let app = state.read().await;
                (
                    "200 OK",
                    serde_json::to_string(&app.metrics()).unwrap_or_else(|_| "{}".into()),
                )
            }
            _ => ("404 Not Found", "not found".to_string()),
        }
    } else {
        ("400 Bad Request", "bad request".to_string())
    };

    let response = format!(
        "HTTP/1.1 {}\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\n\r\n{}",
        status,
        body.len(),
        body
    );

    writer.write_all(response.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

static DOC_DIR: Dir = include_dir!("doc");

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn make_doc_index_html() -> String {
    let mut html = String::from(
        "<html><head><title>syncpond docs</title></head><body><h1>syncpond docs</h1><ul>",
    );
    for entry in DOC_DIR.entries() {
        if let Some(path) = entry.path().to_str() {
            let escaped = html_escape(path);
            if entry.as_dir().is_some() {
                html.push_str(&format!(
                    "<li><a href=\"/docs/{}\">{}/</a></li>",
                    escaped, escaped
                ));
            } else if entry.as_file().is_some() {
                html.push_str(&format!(
                    "<li><a href=\"/docs/{}\">{}</a></li>",
                    escaped, escaped
                ));
            }
        }
    }
    html.push_str("</ul></body></html>");
    html
}

async fn serve_docs_connection(mut stream: TcpStream, request: &str) -> Result<()> {
    let mut lines = request.lines();
    let request_line = lines.next().unwrap_or("");
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    let path = if parts.len() >= 2 { parts[1] } else { "/" };

    let (status, content_type, body) = if path == "/" || path == "/docs" || path == "/docs/" {
        ("200 OK", "text/html; charset=utf-8", make_doc_index_html())
    } else if let Some(stripped) = path.strip_prefix("/docs/") {
        if let Some(file) = DOC_DIR.get_file(stripped) {
            let content = file.contents_utf8().unwrap_or_default().to_string();
            let content_type =
                if Path::new(stripped).extension().and_then(|e| e.to_str()) == Some("md") {
                    "text/markdown; charset=utf-8"
                } else {
                    "text/plain; charset=utf-8"
                };
            ("200 OK", content_type, content)
        } else if let Some(dir) = DOC_DIR.get_dir(stripped) {
            let mut html = format!(
                "<html><head><title>{}</title></head><body><h1>Index of {}</h1><ul>",
                path, path
            );
            for entry in dir.entries() {
                if let Some(name) = entry.path().file_name().and_then(|n| n.to_str()) {
                    let escaped = html_escape(name);
                    let href = format!("{}/{}", path.trim_end_matches('/'), escaped);
                    let label = if entry.as_dir().is_some() {
                        format!("{}/", escaped)
                    } else {
                        escaped.clone()
                    };
                    html.push_str(&format!("<li><a href=\"{}\">{}</a></li>", href, label));
                }
            }
            html.push_str("</ul></body></html>");
            ("200 OK", "text/html; charset=utf-8", html)
        } else {
            (
                "404 Not Found",
                "text/plain; charset=utf-8",
                "not found".to_string(),
            )
        }
    } else {
        (
            "404 Not Found",
            "text/plain; charset=utf-8",
            "not found".to_string(),
        )
    };

    let response = format!(
        "HTTP/1.1 {}\r\ncontent-type: {}\r\ncontent-length: {}\r\n\r\n{}",
        status,
        content_type,
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

async fn handle_ws_or_docs_connection(
    buf_stream: TcpStream,
    peer: SocketAddr,
    state: SharedState,
    ws_hub: Arc<Mutex<WsHub>>,
    auth_rate_limiter: Arc<RateLimiter>,
    ws_room_rate_limiter: Arc<RateLimiter>,
    ws_auth_rate_limit: usize,
    ws_auth_rate_window_secs: u64,
    ws_allowed_origins: Vec<String>,
) -> Result<()> {
    let mut peek_buf = [0u8; 16384];
    let n = buf_stream.peek(&mut peek_buf).await?;
    if n == 0 {
        return Ok(());
    }

    let req_text = String::from_utf8_lossy(&peek_buf[..n]);
    let is_http_get = req_text.starts_with("GET ");
    let is_ws_upgrade = req_text.to_lowercase().contains("upgrade: websocket");

    if is_http_get && !is_ws_upgrade {
        return serve_docs_connection(buf_stream, &req_text).await;
    }

    // from here on, assume WebSocket connection.
    handle_ws_connection(
        buf_stream,
        peer,
        state,
        ws_hub,
        auth_rate_limiter,
        ws_room_rate_limiter,
        ws_auth_rate_limit,
        ws_auth_rate_window_secs,
        ws_allowed_origins,
    )
    .await
}
// legacy TCP command connection function removed; use upstream gRPC for trusted application servers
