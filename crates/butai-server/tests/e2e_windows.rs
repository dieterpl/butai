//! Native Windows smoke coverage: private IPC, HTTP, framed control and ConPTY.
#![cfg(windows)]
use butai_protocol::framing::{decode, encode, length_codec};
use butai_protocol::local::{LocalListener, LocalStream};
use butai_protocol::{AttachTarget, ClientMsg, Command, Encoding, ServerMsg, PROTOCOL_VERSION};
use butai_server::config::{AgentDef, Config};
use futures::{SinkExt, StreamExt};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn http(socket: &Path, method: &str, path: &str, body: serde_json::Value) -> (u16, String) {
    let body = body.to_string();
    let mut stream = LocalStream::connect(socket).await.unwrap();
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), stream.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let response = String::from_utf8(bytes).unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    (headers.split_whitespace().nth(1).unwrap().parse().unwrap(), body.into())
}
async fn output_until(socket: &Path, pane: u64, needle: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let (status, body) = http(
            socket,
            "GET",
            &format!("/v1/workspaces/1/panes/{pane}/output"),
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(status, 200, "{body}");
        if body.contains(needle) {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "missing {needle}: {body}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn windows_daemon_runs_commands_and_batch_agents_over_both_protocols() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("butai.sock");
    let store = tmp.path().join("session.json");
    // A space in the path catches CreateProcess/CRT quoting regressions.
    let agent = tmp.path().join("fake agent.cmd");
    std::fs::write(
        &agent,
        "@echo off\r\necho BATCH_AGENT_OK\r\necho ARG_%~1\r\nset /p reply=\r\necho REPLY_%reply%\r\nexit /b 3\r\n",
    )
    .unwrap();
    let listener = LocalListener::bind(&socket).unwrap();
    let mut config = Config::default();
    config.general.default_shell = Some("cmd.exe".into());
    config.agents.push(AgentDef {
        name: "fake".into(),
        command: agent.to_string_lossy().into_owned(),
        args: vec!["argument with spaces_日本語".into()],
        resume_args: vec![],
        env: Default::default(),
        waiting_pattern: None,
        busy_pattern: None,
    });
    let daemon = tokio::spawn(butai_server::daemon::serve(listener, config, Some(store.clone())));
    let (status, body) = http(
        &socket,
        "POST",
        "/v1/workspaces",
        serde_json::json!({"name":"windows", "path":tmp.path()}),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let (status, body) =
        http(&socket, "POST", "/v1/workspaces/1/agents", serde_json::json!({"type":"fake"})).await;
    assert_eq!(status, 200, "{body}");
    let (_, body) = http(&socket, "GET", "/v1/workspaces/1/agents", serde_json::Value::Null).await;
    let rows: serde_json::Value = serde_json::from_str(&body).unwrap();
    let pane = rows[0]["pane"].as_u64().unwrap();
    output_until(&socket, pane, "BATCH_AGENT_OK").await;
    output_until(&socket, pane, "ARG_argument with spaces_日本語").await;
    let (status, body) = http(
        &socket,
        "POST",
        &format!("/v1/workspaces/1/panes/{pane}/input"),
        serde_json::json!({"paste":"hello\r"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    output_until(&socket, pane, "REPLY_hello").await;

    let (status, body) = http(&socket, "POST", "/v1/workspaces/1/processes", serde_json::json!({"name":"smoke", "command":r#"echo "MANAGED_PROCESS_OK_日本語" & ping -n 30 127.0.0.1 >nul"#})).await;
    assert_eq!(status, 200, "{body}");
    let (_, body) =
        http(&socket, "GET", "/v1/workspaces/1/processes", serde_json::Value::Null).await;
    let rows: serde_json::Value = serde_json::from_str(&body).unwrap();
    let process = rows[0]["pane"].as_u64().unwrap();
    output_until(&socket, process, "MANAGED_PROCESS_OK_日本語").await;

    // Framed control shares the endpoint with REST and starts with a zero byte.
    let mut control = tokio_util::codec::Framed::new(
        LocalStream::connect(&socket).await.unwrap(),
        length_codec(),
    );
    control
        .send(
            encode(
                &ClientMsg::Hello {
                    proto_version: PROTOCOL_VERSION,
                    encoding: Encoding::Json,
                    cols: 80,
                    rows: 24,
                    target: AttachTarget::Control,
                    cwd: tmp.path().into(),
                },
                Encoding::Json,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(10), control.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        decode::<ServerMsg>(&frame, Encoding::Json).unwrap(),
        ServerMsg::Hello { .. }
    ));
    control
        .send(encode(&ClientMsg::Command(Command::KillServer), Encoding::Json).unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), daemon).await.unwrap().unwrap();
    let state = std::fs::read_to_string(store).unwrap();
    assert!(state.contains("windows"));
    assert!(LocalStream::connect(&socket).await.is_err());
}
