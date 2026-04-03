use serde_json::Value;
use tokio::net::TcpStream;
use tokio::io::{AsyncWriteExt, AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tokio::task::JoinHandle;

/// Command enum representing the app-level commands.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    RoomCreate,
    RoomDelete(u64),
    RoomList,
    RoomInfo(u64),
    RoomLabel(u64, String),
    RoomFind(String),
    Set { room_id: u64, container: String, key: String, value: Value },
    Del { room_id: u64, container: String, key: String },
    Get { room_id: u64, container: String, key: String },
    /// Server-only operations that operate on the reserved `server_only` container.
    ServerSet { room_id: u64, key: String, value: Value },
    ServerDel { room_id: u64, key: String },
    ServerGet { room_id: u64, key: String },
    Version(u64),
    SetJwtKey(String),
    TxBegin(u64),
    TxEnd(u64),
    TxAbort(u64),
    TokenGen { room_id: u64, containers: Vec<String> },
    Save { room_id: u64 },
    Load { room_id: u64 },
    PersistSet { room_id: u64, container: String, key: String },
    PersistUnset { room_id: u64, container: String, key: String },
    PersistGet { room_id: u64, container: String, key: String },
}

/// Simple response representation. `Ok` may contain an optional payload string.
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    Ok(Option<String>),
    Error(String),
}

/// Parse an incoming command line into `Command`.
/// Errors are returned as server-style strings like `ERROR missing_argument`.
pub fn parse_command(line: &str) -> Result<Command, String> {
    let mut remainder = line.trim_start();

    let (cmd, rest) = match take_token(remainder) {
        Ok((token, rest)) => (token, rest),
        Err(e) => return Err(e),
    };
    remainder = rest;

    match cmd.as_ref() {
        "ROOM.CREATE" => {
            if !remainder.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::RoomCreate)
        }
        "ROOM.DELETE" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::RoomDelete(room_id))
        }
        "ROOM.LIST" => {
            if !remainder.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::RoomList)
        }
        "ROOM.INFO" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::RoomInfo(room_id))
        }
        "ROOM.LABEL" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            let (label, rest) = take_token(rest)?;
            if label.trim().is_empty() {
                return Err("ERROR label_invalid".into());
            }
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::RoomLabel(room_id, label))
        }
        "ROOM.FIND" => {
            let (label, rest) = take_token(remainder)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::RoomFind(label))
        }
        "SET" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            let (container, rest) = take_token(rest)?;
            let (key, rest) = take_token(rest)?;
            let value_json = rest.trim_start();
            if value_json.is_empty() {
                return Err("ERROR missing_value".into());
            }
            let value: Value = match serde_json::from_str(value_json) {
                Ok(v) => v,
                Err(err) => return Err(format!("ERROR invalid_json {}", err)),
            };
            Ok(Command::Set { room_id, container, key, value })
        }
        "DEL" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            let (container, rest) = take_token(rest)?;
            let (key, rest) = take_token(rest)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::Del { room_id, container, key })
        }
        "GET" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            let (container, rest) = take_token(rest)?;
            let (key, rest) = take_token(rest)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::Get { room_id, container, key })
        }
        "SERVER.SET" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            let (key, rest) = take_token(rest)?;
            let value_json = rest.trim_start();
            if value_json.is_empty() {
                return Err("ERROR missing_value".into());
            }
            let value: Value = match serde_json::from_str(value_json) {
                Ok(v) => v,
                Err(err) => return Err(format!("ERROR invalid_json {}", err)),
            };
            Ok(Command::ServerSet { room_id, key, value })
        }
        "SERVER.DEL" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            let (key, rest) = take_token(rest)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::ServerDel { room_id, key })
        }
        "SERVER.GET" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            let (key, rest) = take_token(rest)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::ServerGet { room_id, key })
        }
        "VERSION" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::Version(room_id))
        }
        "SET.JWTKEY" => {
            let (key, rest) = take_token(remainder)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::SetJwtKey(key))
        }
        "TX.BEGIN" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::TxBegin(room_id))
        }
        "TX.END" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::TxEnd(room_id))
        }
        "TX.ABORT" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::TxAbort(room_id))
        }
        "TOKEN.GEN" => {
            let (room_id_str, rest) = take_token(remainder)?;
            let room_id = match room_id_str.parse::<u64>() {
                Ok(id) => id,
                Err(_) => return Err("ERROR invalid_room_id".into()),
            };
            let mut containers = Vec::new();
            let mut leftover = rest;
            while !leftover.trim().is_empty() {
                let (tok, rem) = take_token(leftover)?;
                containers.push(tok);
                leftover = rem;
            }
            Ok(Command::TokenGen { room_id, containers })
        }
        "SAVE" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::Save { room_id })
        }
        "LOAD" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::Load { room_id })
        }
        "PERSIST.SET" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            let (container, rest) = take_token(rest)?;
            let (key, rest) = take_token(rest)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::PersistSet { room_id, container, key })
        }
        "PERSIST.UNSET" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            let (container, rest) = take_token(rest)?;
            let (key, rest) = take_token(rest)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::PersistUnset { room_id, container, key })
        }
        "PERSIST.GET" => {
            let (room_id, rest) = parse_room_id_from_remainder(remainder)?;
            let (container, rest) = take_token(rest)?;
            let (key, rest) = take_token(rest)?;
            if !rest.trim().is_empty() {
                return Err("ERROR extra_arguments".into());
            }
            Ok(Command::PersistGet { room_id, container, key })
        }
        _ => Err("ERROR unknown_command".into()),
    }
}

/// Format a `Command` into a single-line string suitable for sending to the server.
pub fn format_command(cmd: &Command) -> String {
    match cmd {
        Command::RoomCreate => "ROOM.CREATE".into(),
        Command::RoomDelete(id) => format!("ROOM.DELETE {}", id),
        Command::RoomList => "ROOM.LIST".into(),
        Command::RoomInfo(id) => format!("ROOM.INFO {}", id),
        Command::RoomLabel(id, label) => format!("ROOM.LABEL {} {}", id, format_token(label)),
        Command::RoomFind(label) => format!("ROOM.FIND {}", format_token(label)),
        Command::Set { room_id, container, key, value } => {
            let v = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
            format!("SET {} {} {} {}", room_id, format_token(container), format_token(key), v)
        }
        Command::Del { room_id, container, key } => {
            format!("DEL {} {} {}", room_id, format_token(container), format_token(key))
        }
        Command::Get { room_id, container, key } => {
            format!("GET {} {} {}", room_id, format_token(container), format_token(key))
        }
        Command::Version(id) => format!("VERSION {}", id),
        Command::SetJwtKey(k) => format!("SET.JWTKEY {}", format_token(k)),
        Command::ServerSet { room_id, key, value } => {
            let v = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
            format!("SERVER.SET {} {} {}", room_id, format_token(key), v)
        }
        Command::TxBegin(id) => format!("TX.BEGIN {}", id),
        Command::TxEnd(id) => format!("TX.END {}", id),
        Command::TxAbort(id) => format!("TX.ABORT {}", id),
        Command::TokenGen { room_id, containers } => {
            let mut parts = vec![room_id.to_string()];
            parts.extend(containers.iter().map(|c| format_token(c)));
            format!("TOKEN.GEN {}", parts.join(" "))
        }
        Command::Save { room_id } => {
            format!("SAVE {}", room_id)
        }
        Command::Load { room_id } => {
            format!("LOAD {}", room_id)
        }
        Command::PersistSet { room_id, container, key } => {
            format!("PERSIST.SET {} {} {}", room_id, format_token(container), format_token(key))
        }
        Command::PersistUnset { room_id, container, key } => {
            format!("PERSIST.UNSET {} {} {}", room_id, format_token(container), format_token(key))
        }
        Command::PersistGet { room_id, container, key } => {
            format!("PERSIST.GET {} {} {}", room_id, format_token(container), format_token(key))
        }
        Command::ServerDel { room_id, key } => {
            format!("SERVER.DEL {} {}", room_id, format_token(key))
        }
        Command::ServerGet { room_id, key } => {
            format!("SERVER.GET {} {}", room_id, format_token(key))
        }
    }
}

/// Parse a server response line into `Response`.
pub fn parse_response(line: &str) -> Result<Response, String> {
    let s = line.trim();
    if s.starts_with("OK") {
        let rest = s[2..].trim_start();
        if rest.is_empty() {
            Ok(Response::Ok(None))
        } else {
            Ok(Response::Ok(Some(rest.to_string())))
        }
    } else if s.starts_with("ERROR") {
        let rest = s[5..].trim_start();
        Ok(Response::Error(rest.to_string()))
    } else {
        Err("ERROR invalid_response".into())
    }
}

/// Format a `Response` back to a line.
pub fn format_response(resp: &Response) -> String {
    match resp {
        Response::Ok(None) => "OK".into(),
        Response::Ok(Some(p)) => format!("OK {}", p),
        Response::Error(msg) => format!("ERROR {}", msg),
    }
}

// ----------------- helpers -----------------

fn take_token(input: &str) -> Result<(String, &str), String> {
    let input = input.trim_start();
    if input.is_empty() {
        return Err("ERROR missing_argument".into());
    }

    if input.starts_with('"') {
        let mut buf = String::new();
        let mut escaped = false;
        for (i, c) in input[1..].char_indices() {
            if escaped {
                match c {
                    '\\' => buf.push('\\'),
                    '"' => buf.push('"'),
                    'n' => buf.push('\n'),
                    'r' => buf.push('\r'),
                    't' => buf.push('\t'),
                    other => buf.push(other),
                }
                escaped = false;
                continue;
            }

            if c == '\\' {
                escaped = true;
                continue;
            }

            if c == '"' {
                let end = 1 + i + c.len_utf8();
                let rest = &input[end..];
                return Ok((buf, rest));
            }

            buf.push(c);
        }

        Err("ERROR invalid_argument".into())
    } else {
        let mut end = input.len();
        for (i, c) in input.char_indices() {
            if c.is_whitespace() {
                end = i;
                break;
            }
        }
        let token = input[..end].to_string();
        let rest = &input[end..];
        Ok((token, rest))
    }
}

fn parse_room_id_from_remainder(remainder: &str) -> Result<(u64, &str), String> {
    let (room_id, rest) = take_token(remainder)?;
    let parsed = room_id
        .parse::<u64>()
        .map_err(|_| "ERROR invalid_room_id".to_string())?;
    Ok((parsed, rest))
}

/// Attempt to connect to `addr`, send the `api_key` followed by a newline,
/// and return the established `TcpStream` if successful.
pub async fn connect_with_auth(addr: &str, api_key: &str) -> Result<TcpStream, std::io::Error> {
    let mut stream = TcpStream::connect(addr).await?;
    stream.write_all(api_key.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;
    Ok(stream)
}

/// Try connecting with retries and delay between attempts.
pub async fn connect_with_retry(
    addr: &str,
    api_key: &str,
    retries: usize,
    delay: Duration,
) -> Result<TcpStream, std::io::Error> {
    for attempt in 0..=retries {
        match connect_with_auth(addr, api_key).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                if attempt == retries {
                    return Err(e);
                }
                sleep(delay).await;
            }
        }
    }
    unreachable!()
}

/// Send a single `Command` over the provided `TcpStream` (adds a trailing newline).
pub async fn send_command(stream: &mut TcpStream, cmd: &Command) -> Result<(), std::io::Error> {
    let s = format_command(cmd);
    stream.write_all(s.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;
    Ok(())
}

/// Start a background task that maintains a long-lived TCP connection to
/// `addr`, authenticates with `api_key`, forwards received lines to the
/// returned `mpsc::Receiver<String>`, and accepts outgoing `Command`s via
/// the returned `mpsc::Sender<Command>`. The task will attempt reconnection on
/// disconnect.
pub fn start_long_lived_tcp(
    addr: String,
    api_key: String,
) -> (mpsc::Receiver<String>, mpsc::Sender<Command>) {
    let (tx_in, rx_in) = mpsc::channel(128);
    let (tx_cmd, rx_cmd) = mpsc::channel(128);

    tokio::spawn(async move {
        // rx_cmd is the receiver side that will yield outgoing commands.
        let mut rx_cmd = rx_cmd;

        loop {
            match connect_with_retry(&addr, &api_key, 3, Duration::from_secs(1)).await {
                Ok(stream) => {
                    let (read_half, mut write_half) = tokio::io::split(stream);
                    let mut lines = BufReader::new(read_half).lines();

                    loop {
                        tokio::select! {
                            res = lines.next_line() => {
                                match res {
                                    Ok(Some(line)) => {
                                        if tx_in.send(line).await.is_err() {
                                            // receiver dropped — stop background task
                                            return;
                                        }
                                    }
                                    Ok(None) => {
                                        // connection closed by peer
                                        break;
                                    }
                                    Err(_) => {
                                        // read error, break to reconnect
                                        break;
                                    }
                                }
                            }
                            maybe_cmd = rx_cmd.recv() => {
                                match maybe_cmd {
                                    Some(cmd) => {
                                        let s = format_command(&cmd);
                                        if let Err(_) = write_half.write_all(s.as_bytes()).await { break; }
                                        if let Err(_) = write_half.write_all(b"\n").await { break; }
                                        if let Err(_) = write_half.flush().await { break; }
                                    }
                                    None => {
                                        // all senders dropped, no more outgoing commands
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    // loop will try to reconnect
                }
                Err(_) => {
                    // failed to connect after retries — wait before retrying
                    sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });

    (rx_in, tx_cmd)
}

/// A higher-level client wrapper that provides incoming/outgoing channels
/// and a graceful shutdown handle for the background connection task.
pub struct Client {
    /// Incoming lines received from the server.
    pub incoming: mpsc::Receiver<String>,
    /// Send `Command` values to be written to the server.
    pub outgoing: mpsc::Sender<Command>,
    shutdown_tx: mpsc::Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl Client {
    /// Spawn a background task to maintain a long-lived connection to `addr`.
    /// Returns a `Client` that can send commands and receive incoming lines.
    pub fn start(addr: String, api_key: String) -> Self {
        let (tx_in, rx_in) = mpsc::channel(128);
        let (tx_cmd, rx_cmd) = mpsc::channel(128);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);

        let handle = tokio::spawn(async move {
            let mut rx_cmd = rx_cmd;

            loop {
                // If shutdown requested before connect, exit (non-blocking check)
                if shutdown_rx.try_recv().is_ok() {
                    return;
                }

                match connect_with_retry(&addr, &api_key, 3, Duration::from_secs(1)).await {
                    Ok(stream) => {
                        let (read_half, mut write_half) = tokio::io::split(stream);
                        let mut lines = BufReader::new(read_half).lines();

                        loop {
                            tokio::select! {
                                _ = shutdown_rx.recv() => {
                                    return;
                                }
                                res = lines.next_line() => {
                                    match res {
                                        Ok(Some(line)) => {
                                            if tx_in.send(line).await.is_err() {
                                                return;
                                            }
                                        }
                                        Ok(None) => { break; }
                                        Err(_) => { break; }
                                    }
                                }
                                maybe_cmd = rx_cmd.recv() => {
                                    match maybe_cmd {
                                        Some(cmd) => {
                                            let s = format_command(&cmd);
                                            if let Err(_) = write_half.write_all(s.as_bytes()).await { break; }
                                            if let Err(_) = write_half.write_all(b"\n").await { break; }
                                            if let Err(_) = write_half.flush().await { break; }
                                        }
                                        None => {
                                            // All senders dropped, exit background task
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                        // connection closed; try to reconnect
                    }
                    Err(_) => {
                        // failed to connect after retries — wait or exit on shutdown
                        tokio::select! {
                            _ = shutdown_rx.recv() => return,
                            _ = sleep(Duration::from_secs(1)) => {}
                        }
                    }
                }
            }
        });

        Client {
            incoming: rx_in,
            outgoing: tx_cmd,
            shutdown_tx,
            handle: Some(handle),
        }
    }

    /// Asynchronously send a `Command` to the server via the background task.
    pub async fn send(&self, cmd: Command) -> Result<(), mpsc::error::SendError<Command>> {
        self.outgoing.send(cmd).await
    }

    /// Try to synchronously send a `Command` without awaiting.
    pub fn try_send(&self, cmd: Command) -> Result<(), mpsc::error::TrySendError<Command>> {
        self.outgoing.try_send(cmd)
    }

    /// Gracefully shutdown the background task and wait for it to finish.
    pub async fn shutdown(mut self) -> Result<(), tokio::task::JoinError> {
        let _ = self.shutdown_tx.send(()).await;
        if let Some(handle) = self.handle.take() {
            handle.await
        } else {
            Ok(())
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Best-effort: signal shutdown and abort the task so we don't leak it.
        let _ = self.shutdown_tx.try_send(());
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

fn needs_quote(s: &str) -> bool {
    s.is_empty() || s.chars().any(|c| c.is_whitespace() || c == '"' || c == '\\')
}

fn escape_token(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\"") ,
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

fn format_token(s: &str) -> String {
    if needs_quote(s) {
        format!("\"{}\"", escape_token(s))
    } else {
        s.to_string()
    }
}
