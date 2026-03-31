//! syncpond-server is a small real-time room/key sync server with command/WS APIs.
//!
//! Security assumptions:
//! - `require_tls` requires TLS termination to be handled by an external proxy (nginx/Caddy/traefik).
//! - The command API socket is sensitive and should be bound to loopback or private network.
//! - Command API key and JWT signing keys must be provisioned securely and rotated out of band.
#![deny(missing_docs)]

mod commands;
mod rate_limiter;
mod state;
mod ws;

use crate::commands::process_command;
use crate::rate_limiter::RateLimiter;
use crate::state::{AppState, SharedState};
use crate::ws::{handle_ws_connection, WsHub};
use anyhow::{Context, Result};
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use std::{fs, net::SocketAddr, path::Path, sync::Arc, time::Duration};
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
    command_addr: Option<String>,
    health_addr: Option<String>,
    jwt_key: Option<String>,
    jwt_issuer: Option<String>,
    jwt_audience: Option<String>,
    jwt_ttl_seconds: Option<u64>,
    require_tls: Option<bool>,
    health_bind_loopback_only: Option<bool>,
    command_rate_limit: Option<usize>,
    command_rate_window_secs: Option<u64>,
    ws_auth_rate_limit: Option<usize>,
    ws_auth_rate_window_secs: Option<u64>,
    ws_update_rate_limit: Option<usize>,
    ws_update_rate_window_secs: Option<u64>,
    ws_room_rate_limit: Option<usize>,
    ws_room_rate_window_secs: Option<u64>,
    ws_allowed_origins: Option<Vec<String>>,
    command_auth_rate_limit: Option<usize>,
    command_auth_rate_window_secs: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let config_path = std::env::var("SYNCPOND_CONFIG").unwrap_or_else(|_| "config.yaml".to_string());
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

    let shared_state = Arc::new(RwLock::new(base_state));
    let ws_hub = Arc::new(Mutex::new(WsHub::new()));
    let ws_auth_rate_limiter = Arc::new(RateLimiter::new());
    let ws_update_rate_limiter = Arc::new(RateLimiter::new());
    let ws_room_rate_limiter = Arc::new(RateLimiter::new());
    let command_rate_limiter = Arc::new(RateLimiter::new());
    let command_auth_rate_limiter = Arc::new(RateLimiter::new());

    let ws_update_rate_limiter_for_ws = ws_update_rate_limiter.clone();
    let ws_update_rate_limiter_for_cmd = ws_update_rate_limiter.clone();
    let ws_room_rate_limiter_for_ws = ws_room_rate_limiter.clone();
    let ws_room_rate_limiter_for_cmd = ws_room_rate_limiter.clone();

    let cmd_rate_limit = config.command_rate_limit.unwrap_or(DEFAULT_CMD_RATE_LIMIT);
    let cmd_rate_window_secs = config.command_rate_window_secs.unwrap_or(DEFAULT_CMD_RATE_WINDOW_SECS);
    let ws_auth_rate_limit = config.ws_auth_rate_limit.unwrap_or(DEFAULT_WS_AUTH_RATE_LIMIT);
    let ws_auth_rate_window_secs = config.ws_auth_rate_window_secs.unwrap_or(DEFAULT_WS_AUTH_RATE_WINDOW_SECS);
    let ws_update_rate_limit = config.ws_update_rate_limit.unwrap_or(DEFAULT_WS_UPDATE_RATE_LIMIT);
    let ws_update_rate_window_secs = config.ws_update_rate_window_secs.unwrap_or(DEFAULT_WS_UPDATE_RATE_WINDOW_SECS);
    let ws_room_rate_limit = config.ws_room_rate_limit.unwrap_or(DEFAULT_WS_ROOM_RATE_LIMIT);
    let ws_room_rate_window_secs = config.ws_room_rate_window_secs.unwrap_or(DEFAULT_WS_ROOM_RATE_WINDOW_SECS);
    let ws_allowed_origins = config
        .ws_allowed_origins
        .clone()
        .unwrap_or_else(|| DEFAULT_WS_ALLOWED_ORIGINS.iter().map(|s| s.to_string()).collect());

    let command_auth_rate_limit = config
        .command_auth_rate_limit
        .unwrap_or(DEFAULT_COMMAND_AUTH_RATE_LIMIT);
    let command_auth_rate_window_secs = config
        .command_auth_rate_window_secs
        .unwrap_or(DEFAULT_COMMAND_AUTH_RATE_WINDOW_SECS);

    if config.command_api_key.trim().is_empty() {
        anyhow::bail!("command_api_key must be configured and non-empty");
    }

    let require_tls = config.require_tls.unwrap_or(false);
    if require_tls {
        anyhow::bail!("TLS transport required in config, but this binary does not terminate TLS; use reverse proxy for TLS termination");
    }

    let ws_addr = config.ws_addr.unwrap_or_else(|| "127.0.0.1:8080".to_string());
    let command_addr = config.command_addr.unwrap_or_else(|| "127.0.0.1:9090".to_string());
    let health_addr = config.health_addr.unwrap_or_else(|| "127.0.0.1:7070".to_string());
    let health_bind_loopback_only = config.health_bind_loopback_only.unwrap_or(true);

    let ws_addr: SocketAddr = ws_addr
        .parse()
        .with_context(|| format!("invalid ws_addr: {}", ws_addr))?;
    let command_addr: SocketAddr = command_addr
        .parse()
        .with_context(|| format!("invalid command_addr: {}", command_addr))?;
    let health_addr: SocketAddr = health_addr
        .parse()
        .with_context(|| format!("invalid health_addr: {}", health_addr))?;

    if health_bind_loopback_only && !health_addr.ip().is_loopback() {
        anyhow::bail!("health_bind_loopback_only=true but health_addr is not loopback");
    }

    let ws_state = shared_state.clone();
    let ws_hub_for_ws = ws_hub.clone();
    let command_state = shared_state.clone();
    let ws_hub_for_cmd = ws_hub.clone();
    let command_rate_limiter_for_cmd = command_rate_limiter.clone();
    let command_auth_rate_limiter_for_cmd = command_auth_rate_limiter.clone();

    let ws_addr_for_task = ws_addr.clone();
    let command_addr_for_task = command_addr.clone();

    let ws_server = tokio::spawn(async move {
        let listener = TcpListener::bind(ws_addr_for_task).await.context("ws bind failed")?;
        info!("syncpond websocket server listening on {}", ws_addr_for_task);

        loop {
            let (stream, peer) = listener.accept().await?;
            let state = ws_state.clone();
            let hub = ws_hub_for_ws.clone();
            let auth_limiter = ws_auth_rate_limiter.clone();
            let update_limiter = ws_update_rate_limiter_for_ws.clone();
            let room_limiter = ws_room_rate_limiter_for_ws.clone();
            let ws_allowed_origins_for_conn = ws_allowed_origins.clone();
            tokio::spawn(async move {
                if let Err(err) = handle_ws_or_docs_connection(
                    stream,
                    peer,
                    state,
                    hub,
                    auth_limiter,
                    update_limiter,
                    room_limiter,
                    ws_auth_rate_limit,
                    ws_auth_rate_window_secs,
                    ws_update_rate_limit,
                    ws_update_rate_window_secs,
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

    let command_server = tokio::spawn(async move {
        let listener = TcpListener::bind(command_addr_for_task).await.context("cmd bind failed")?;
        info!("syncpond command socket listening on {}", command_addr_for_task);

        loop {
            let (stream, peer) = listener.accept().await?;
            let state = command_state.clone();
            let hub = ws_hub_for_cmd.clone();
            let rate_limiter = command_rate_limiter_for_cmd.clone();
            let ws_update_rate_limiter = ws_update_rate_limiter_for_cmd.clone();
            let ws_room_rate_limiter = ws_room_rate_limiter_for_cmd.clone();
            let command_auth_rate_limiter_for_conn = command_auth_rate_limiter_for_cmd.clone();
            tokio::spawn(async move {
                if let Err(err) = handle_command_connection(
                    stream,
                    peer,
                    state,
                    hub,
                    rate_limiter,
                    command_auth_rate_limiter_for_conn,
                    ws_update_rate_limiter,
                    ws_room_rate_limiter,
                    ws_update_rate_limit,
                    ws_update_rate_window_secs,
                    ws_room_rate_limit,
                    ws_room_rate_window_secs,
                    cmd_rate_limit,
                    cmd_rate_window_secs,
                    command_auth_rate_limit,
                    command_auth_rate_window_secs,
                )
                .await
                {
                    error!(%err, peer = %peer, "command connection error");
                }
            });
        }

        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });

    let health_state = shared_state.clone();
    let health_addr_for_task = health_addr.clone();
    let health_server = tokio::spawn(async move {
        info!("syncpond health server listening on {}", health_addr_for_task);
        let listener = TcpListener::bind(health_addr_for_task).await.context("health bind failed")?;

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

    let shutdown = async {
        tokio::signal::ctrl_c().await.context("failed to listen for ctrl-c")?;
        info!("shutdown signal received");
        Ok::<(), anyhow::Error>(())
    };

    tokio::select! {
        res = shutdown => res?,
        res = ws_server => res??,
        res = command_server => res??,
        res = health_server => res??,
    }

    info!("server shutdown complete");
    Ok(())
}

const MAX_COMMAND_LINE_LEN: usize = 8192;

const DEFAULT_CMD_RATE_LIMIT: usize = 120;
const DEFAULT_CMD_RATE_WINDOW_SECS: u64 = 60;
const DEFAULT_WS_AUTH_RATE_LIMIT: usize = 10;
const DEFAULT_WS_AUTH_RATE_WINDOW_SECS: u64 = 60;
const DEFAULT_WS_UPDATE_RATE_LIMIT: usize = 240;
const DEFAULT_WS_UPDATE_RATE_WINDOW_SECS: u64 = 60;
const DEFAULT_WS_ROOM_RATE_LIMIT: usize = 1000;
const DEFAULT_WS_ROOM_RATE_WINDOW_SECS: u64 = 60;
const DEFAULT_WS_ALLOWED_ORIGINS: &[&str] = &[];
const DEFAULT_COMMAND_AUTH_RATE_LIMIT: usize = 5;
const DEFAULT_COMMAND_AUTH_RATE_WINDOW_SECS: u64 = 60;

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn read_line_with_limit<R>(reader: &mut BufReader<R>, line: &mut String) -> Result<usize>
where
    R: tokio::io::AsyncRead + Unpin,
{
    line.clear();
    let bytes = reader.read_line(line).await?;
    if line.len() > MAX_COMMAND_LINE_LEN {
        anyhow::bail!("line_too_long");
    }
    Ok(bytes)
}

async fn handle_health_connection(
    stream: TcpStream,
    state: SharedState,
) -> Result<()> {
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
                ("200 OK", serde_json::to_string(&app.metrics()).unwrap_or_else(|_| "{}".into()))
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
    let mut html = String::from("<html><head><title>syncpond docs</title></head><body><h1>syncpond docs</h1><ul>");
    for entry in DOC_DIR.entries() {
        if let Some(path) = entry.path().to_str() {
            let escaped = html_escape(path);
            if entry.as_dir().is_some() {
                html.push_str(&format!("<li><a href=\"/docs/{}\">{}/</a></li>", escaped, escaped));
            } else if entry.as_file().is_some() {
                html.push_str(&format!("<li><a href=\"/docs/{}\">{}</a></li>", escaped, escaped));
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
            let content_type = if Path::new(stripped).extension().and_then(|e| e.to_str()) == Some("md") {
                "text/markdown; charset=utf-8"
            } else {
                "text/plain; charset=utf-8"
            };
            ("200 OK", content_type, content)
        } else if let Some(dir) = DOC_DIR.get_dir(stripped) {
            let mut html = format!("<html><head><title>{}</title></head><body><h1>Index of {}</h1><ul>", path, path);
            for entry in dir.entries() {
                if let Some(name) = entry.path().file_name().and_then(|n| n.to_str()) {
                    let escaped = html_escape(name);
                    let href = format!("{}/{}", path.trim_end_matches('/'), escaped);
                    let label = if entry.as_dir().is_some() { format!("{}/", escaped) } else { escaped.clone() };
                    html.push_str(&format!("<li><a href=\"{}\">{}</a></li>", href, label));
                }
            }
            html.push_str("</ul></body></html>");
            ("200 OK", "text/html; charset=utf-8", html)
        } else {
            ("404 Not Found", "text/plain; charset=utf-8", "not found".to_string())
        }
    } else {
        ("404 Not Found", "text/plain; charset=utf-8", "not found".to_string())
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
    ws_update_rate_limiter: Arc<RateLimiter>,
    ws_room_rate_limiter: Arc<RateLimiter>,
    ws_auth_rate_limit: usize,
    ws_auth_rate_window_secs: u64,
    ws_update_rate_limit: usize,
    ws_update_rate_window_secs: u64,
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
        ws_update_rate_limiter,
        ws_room_rate_limiter,
        ws_auth_rate_limit,
        ws_auth_rate_window_secs,
        ws_update_rate_limit,
        ws_update_rate_window_secs,
        ws_allowed_origins,
    )
    .await
}

async fn handle_command_connection(
    stream: TcpStream,
    peer: SocketAddr,
    state: SharedState,
    ws_hub: Arc<Mutex<WsHub>>,
    rate_limiter: Arc<RateLimiter>,
    command_auth_rate_limiter: Arc<RateLimiter>,
    ws_update_rate_limiter: Arc<RateLimiter>,
    ws_room_rate_limiter: Arc<RateLimiter>,
    ws_update_rate_limit: usize,
    ws_update_rate_window_secs: u64,
    ws_room_rate_limit: usize,
    ws_room_rate_window_secs: u64,
    cmd_rate_limit: usize,
    cmd_rate_window_secs: u64,
    command_auth_rate_limit: usize,
    command_auth_rate_window_secs: u64,
) -> Result<()> {
    let key = peer.ip().to_string();
    let allowed = rate_limiter
        .allow(&key, cmd_rate_limit, Duration::from_secs(cmd_rate_window_secs))
        .await;
    if !allowed {
        info!(%peer, "command rate limit exceeded");
        return Ok(());
    }

    let auth_allowed = command_auth_rate_limiter
        .allow(&key, command_auth_rate_limit, Duration::from_secs(command_auth_rate_window_secs))
        .await;
    if !auth_allowed {
        info!(%peer, "command auth rate limit exceeded");
        return Ok(());
    }

    info!(%peer, "command connection established");

    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);
    let mut line = String::new();

    // first message must be API key
    line.clear();
    let first_bytes = read_line_with_limit(&mut reader, &mut line).await?;
    if first_bytes == 0 {
        return Ok(());
    }
    let provided_key = line.trim();

    let expected_key = {
        let app = state.read().await;
        app.command_api_key.clone()
    };

    if let Some(expected_key) = expected_key {
        if !constant_time_eq(provided_key, &expected_key) {
            {
                let mut app = state.write().await;
                app.command_auth_failure = app.command_auth_failure.saturating_add(1);
            }
            writer.write_all(b"ERROR invalid_api_key\n").await?;
            writer.flush().await?;
            return Ok(());
        }
    } else {
        {
            let mut app = state.write().await;
            app.command_auth_failure = app.command_auth_failure.saturating_add(1);
        }
        writer.write_all(b"ERROR api_key_not_configured\n").await?;
        writer.flush().await?;
        return Ok(());
    }

    loop {
        line.clear();
        let bytes = match read_line_with_limit(&mut reader, &mut line).await {
            Ok(n) => n,
            Err(_) => {
                let msg = "ERROR malformed_request\n";
                writer.write_all(msg.as_bytes()).await?;
                writer.flush().await?;
                let mut app = state.write().await;
                app.invalid_command_count = app.invalid_command_count.saturating_add(1);
                return Ok(());
            }
        };
        if bytes == 0 {
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        {
            let mut app = state.write().await;
            app.total_command_requests = app.total_command_requests.saturating_add(1);
        }

        let (resp, updates) = process_command(trimmed, &state).await;

        if resp.starts_with("ERROR") {
            let mut app = state.write().await;
            app.command_error_count = app.command_error_count.saturating_add(1);
            if resp.starts_with("ERROR invalid_") || resp == "ERROR unknown_command" || resp == "ERROR missing_argument" {
                app.invalid_command_count = app.invalid_command_count.saturating_add(1);
            }
        }
        writer.write_all(resp.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;

        let mut room_deleted: Option<u64> = None;
        if trimmed.starts_with("ROOM.DELETE") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(id) = parts[1].parse::<u64>() {
                    if resp == "OK" {
                        room_deleted = Some(id);
                    }
                }
            }
        }

        if let Some(room_id) = room_deleted {
            let mut hub = ws_hub.lock().await;
            hub.remove_room(room_id);
            info!(room_id, "ws hub cleanup on room delete");
        }

        if !updates.is_empty() {
            for update in updates {
                let room_key = format!("room:{}", update.room_id);
                let room_allowed = ws_room_rate_limiter
                    .allow(&room_key, ws_room_rate_limit, std::time::Duration::from_secs(ws_room_rate_window_secs))
                    .await;

                if !room_allowed {
                    info!(room_id = update.room_id, "room-level ws update rate limit exceeded");
                    continue;
                }

                let mut hub = ws_hub.lock().await;
                hub.broadcast_update(
                    update,
                    &ws_update_rate_limiter,
                    ws_update_rate_limit,
                    ws_update_rate_window_secs,
                )
                .await;
            }
        }
    }

    info!(%peer, "command disconnected");
    Ok(())
}


