pub mod protocol;

pub use protocol::{
    Command,
    Response,
    format_command,
    format_response,
    parse_command,
    parse_response,
    connect_with_auth,
    connect_with_retry,
    start_long_lived_tcp,
    Client,
    send_command,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_and_format_room_create() {
        let cmd = parse_command("ROOM.CREATE").unwrap();
        assert_eq!(cmd, Command::RoomCreate);
        let line = format_command(&cmd);
        assert_eq!(line, "ROOM.CREATE");
    }

    #[test]
    fn parse_set_with_json() {
        let line = "SET 1 public foo {\"a\": 123, \"s\": \"x\"}";
        let cmd = parse_command(line).unwrap();
        match cmd {
            Command::Set { room_id, container, key, value } => {
                assert_eq!(room_id, 1);
                assert_eq!(container, "public");
                assert_eq!(key, "foo");
                assert_eq!(value, json!({"a": 123, "s": "x"}));
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parse_and_format_persist_commands() {
        let s = "PERSIST.SET 1 public foo";
        let cmd = parse_command(s).unwrap();
        assert_eq!(cmd, Command::PersistSet { room_id: 1, container: "public".into(), key: "foo".into() });
        assert_eq!(format_command(&cmd), "PERSIST.SET 1 public foo");

        let s2 = "PERSIST.UNSET 1 public foo";
        let cmd2 = parse_command(s2).unwrap();
        assert_eq!(cmd2, Command::PersistUnset { room_id: 1, container: "public".into(), key: "foo".into() });
        assert_eq!(format_command(&cmd2), "PERSIST.UNSET 1 public foo");

        let s3 = "PERSIST.GET 1 public foo";
        let cmd3 = parse_command(s3).unwrap();
        assert_eq!(cmd3, Command::PersistGet { room_id: 1, container: "public".into(), key: "foo".into() });
        assert_eq!(format_command(&cmd3), "PERSIST.GET 1 public foo");
    }

    #[test]
    fn parse_and_format_save_load_commands() {
        let s = "SAVE 1";
        let cmd = parse_command(s).unwrap();
        assert_eq!(cmd, Command::Save { room_id: 1 });
        assert_eq!(format_command(&cmd), "SAVE 1");

        let s2 = "LOAD 1";
        let cmd2 = parse_command(s2).unwrap();
        assert_eq!(cmd2, Command::Load { room_id: 1 });
        assert_eq!(format_command(&cmd2), "LOAD 1");
    }

    #[test]
    fn response_roundtrip() {
        let r = Response::Ok(Some("{\"a\":1}".to_string()));
        let s = format_response(&r);
        assert_eq!(s, "OK {\"a\":1}");
        let parsed = parse_response(&s).unwrap();
        assert_eq!(parsed, r);
    }

    #[tokio::test]
    async fn connect_and_auth_and_receive() {
        use tokio::net::TcpListener;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let api_key = "test-api-key-xyz";

        let (mut rx, _tx) = start_long_lived_tcp(addr.clone(), api_key.to_string());

        // Accept the incoming connection from our background client and validate auth.
        let accept = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(line.trim_end(), api_key);
            socket.write_all(b"HELLO\n").await.unwrap();
            socket.flush().await.unwrap();
        });

        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout waiting for line")
            .expect("channel closed");
        assert_eq!(got, "HELLO");

        accept.await.unwrap();
    }

    #[tokio::test]
    async fn client_wrapper_send_and_shutdown() {
        use tokio::net::TcpListener;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let api_key = "test-api-key-xyz".to_string();

        // Start the client wrapper which spawns a background task.
        let client = Client::start(addr.clone(), api_key.clone());

        // Accept and validate auth + command from client in a background task.
        let accept = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut line = String::new();
            // auth
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(line.trim_end(), api_key);

            // read command sent by client
            let mut cmd_line = String::new();
            reader.read_line(&mut cmd_line).await.unwrap();
            assert_eq!(cmd_line.trim_end(), "ROOM.CREATE");
        });

        // Send a command from the client wrapper.
        client.send(Command::RoomCreate).await.unwrap();

        // Wait for the server task to accept the connection and receive the command,
        // then shut down the client.
        accept.await.unwrap();
        client.shutdown().await.unwrap();
    }
}
