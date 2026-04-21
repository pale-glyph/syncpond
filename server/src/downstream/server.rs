use crate::downstream::connection::handle_ws_connection;
use crate::downstream::hub::WsHub;
use crate::rate_limiter::RateLimiter;
use anyhow::{Context, Result};
use include_dir::{include_dir, Dir};
use std::{net::SocketAddr, path::Path, sync::Arc};
use tokio::{net::{TcpListener, TcpStream}, sync::Mutex};
use tokio::io::AsyncWriteExt;
use tracing::{error, info};

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
            let content_type = if Path::new(stripped).extension().and_then(|e| e.to_str()) == Some("md") {
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
    ws_hub: Arc<Mutex<WsHub>>,
    auth_rate_limiter: Arc<RateLimiter>,
    ws_room_rate_limiter: Arc<RateLimiter>,
    ws_auth_rate_limit: usize,
    ws_auth_rate_window_secs: u64,
    ws_allowed_origins: Vec<String>,
    jwt_key: Option<String>,
    jwt_issuer: Option<String>,
    jwt_audience: Option<String>,
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
        ws_hub,
        auth_rate_limiter,
        ws_room_rate_limiter,
        ws_auth_rate_limit,
        ws_auth_rate_window_secs,
        ws_allowed_origins,
        jwt_key,
        jwt_issuer,
        jwt_audience,
    )
    .await
}

/// Spawn the websocket server and return its JoinHandle.
pub fn spawn_ws_server(
    ws_addr: SocketAddr,
    ws_hub: Arc<Mutex<WsHub>>,
    auth_rate_limiter: Arc<RateLimiter>,
    ws_room_rate_limiter: Arc<RateLimiter>,
    ws_auth_rate_limit: usize,
    ws_auth_rate_window_secs: u64,
    ws_allowed_origins: Vec<String>,
    jwt_key: Option<String>,
    jwt_issuer: Option<String>,
    jwt_audience: Option<String>,
) -> tokio::task::JoinHandle<anyhow::Result<(), anyhow::Error>> {
    tokio::spawn(async move {
        let listener = TcpListener::bind(ws_addr).await.context("ws bind failed")?;
        info!("syncpond websocket server listening on {}", ws_addr);

        loop {
            let (stream, peer) = listener.accept().await?;
            let hub = ws_hub.clone();
            let auth_limiter = auth_rate_limiter.clone();
            let room_limiter = ws_room_rate_limiter.clone();
            let ws_allowed_origins_for_conn = ws_allowed_origins.clone();
            let jwt_key_for_conn = jwt_key.clone();
            let jwt_issuer_for_conn = jwt_issuer.clone();
            let jwt_audience_for_conn = jwt_audience.clone();
            tokio::spawn(async move {
                if let Err(err) = handle_ws_or_docs_connection(
                    stream,
                    peer,
                    hub,
                    auth_limiter,
                    room_limiter,
                    ws_auth_rate_limit,
                    ws_auth_rate_window_secs,
                    ws_allowed_origins_for_conn,
                    jwt_key_for_conn,
                    jwt_issuer_for_conn,
                    jwt_audience_for_conn,
                )
                .await
                {
                    error!(%err, peer = %peer, "ws/http connection error");
                }
            });
        }

        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    })
}

/// Start the downstream subsystem: notification forwarder, websocket server,
/// and HTTP health/docs server. Returns `(forwarder_handle, ws_handle, health_handle)`.
pub async fn start_downstream(
    health_addr: SocketAddr,
    ws_hub: Arc<Mutex<WsHub>>,
    health_bind_loopback_only: bool,
) -> anyhow::Result<(
    tokio::task::JoinHandle<anyhow::Result<(), anyhow::Error>>,
    tokio::task::JoinHandle<anyhow::Result<(), anyhow::Error>>,
)> {
    // Take the notification receiver out of the hub so the forwarder can
    // receive updates and only lock the hub while broadcasting.
    let notification_rx = {
        let mut hub = ws_hub.lock().await;
        hub.take_notification_receiver()
    };

    let notification_rx = notification_rx
        .ok_or_else(|| anyhow::anyhow!("notification receiver already taken"))?;

    // Forwarder: receives DataUpdate and calls `broadcast_update` while
    // briefly locking the hub for each update.
    let forwarder_hub = ws_hub.clone();
    let forwarder = tokio::spawn(async move {
        let mut rx = notification_rx;
        while let Some(update) = rx.recv().await {
            let mut hub = forwarder_hub.lock().await;
            hub.broadcast_update(update).await;
        }
        Ok::<(), anyhow::Error>(())
    });

    // Health server (no access to app SharedState anymore)
    let health_addr_for_task = health_addr.clone();
    let health_handle = tokio::spawn(async move {
        info!("syncpond health server listening on {}", health_addr_for_task);
        let listener = TcpListener::bind(health_addr_for_task)
            .await
            .context("health bind failed")?;

        loop {
            let (stream, peer) = listener.accept().await?;
            tokio::spawn(async move {
                if health_bind_loopback_only && !peer.ip().is_loopback() {
                    error!(%peer, "rejected non-loopback health connection");
                    return;
                }

                if let Err(err) = handle_health_connection(stream).await {
                    error!(%err, peer = %peer, "health connection error");
                }
            });
        }

        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });

    Ok((forwarder, health_handle))
}

async fn read_line_with_limit<R>(reader: &mut tokio::io::BufReader<R>, line: &mut String) -> Result<usize>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;

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
        if total > crate::MAX_COMMAND_LINE_LEN {
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

async fn handle_health_connection(stream: TcpStream) -> Result<()> {
    use tokio::io::{AsyncWriteExt, BufReader, BufWriter};

    let (reader, writer) = stream.into_split();
    let mut _reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);

    // Drain the first line (simple HTTP request parsing not required for now).
    let mut _line = String::new();
    let _ = read_line_with_limit(&mut _reader, &mut _line).await;

    // For now always return 200 OK with a simple body. No access to AppState.
    let body = "ok";
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );

    writer.write_all(response.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

