//! Real HTTP streaming with a virtual clock. No model or Lean pool is needed.
//! Run: cargo test --locked --manifest-path harness/Cargo.toml --test responses

use harness::responses::{Client, Completion, ModelConfig};
use harness::trace::Trace;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const SSE_HEADERS: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";

struct Request {
    socket: TcpStream,
    task: JoinHandle<anyhow::Result<Completion>>,
    dir: PathBuf,
}

impl Request {
    async fn start(idle_timeout: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "responses-test-{}-{}",
            std::process::id(),
            address.port()
        ));
        std::fs::create_dir(&dir).unwrap();
        let trace = Trace::open(&dir.join("trace.jsonl")).unwrap();
        let client = Client::new(ModelConfig {
            base_url: format!("http://{address}/v1"),
            api_key: "local-test-only".into(),
            model: "test".into(),
            effort: "high".into(),
            max_output_tokens: 300_000,
            logprobs: false,
            idle_timeout,
            max_attempts: 1,
        })
        .unwrap();
        let task = tokio::spawn(async move {
            client
                .complete(&json!({}), &trace, "test", Some(&trace))
                .await
        });
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.set_nodelay(true).unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(socket.read_u8().await.unwrap());
        }
        assert!(header.starts_with(b"POST /v1/responses HTTP/1.1\r\n"));
        let mut body = [0; 2];
        socket.read_exact(&mut body).await.unwrap();
        assert_eq!(&body, b"{}");
        // Establish the real TCP connection before controlling elapsed time.
        tokio::time::pause();
        Self { socket, task, dir }
    }

    async fn send(&mut self, event: Value) {
        let path = self.dir.join("trace.jsonl");
        let before = std::fs::read_to_string(&path).unwrap().lines().count();
        self.socket
            .write_all(format!("data: {event}\n\n").as_bytes())
            .await
            .unwrap();
        // Wait until the client has consumed this event before advancing time.
        settle(|| std::fs::read_to_string(&path).unwrap().lines().count() > before).await;
        assert!(!self.task.is_finished() || event["type"] == "response.completed");
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn settle(ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "HTTP client did not make progress"
        );
        // Keep the test runnable so Tokio cannot auto-advance the paused clock
        // while real socket I/O is still being delivered by the OS.
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[tokio::test]
async fn active_reasoning_can_exceed_thirty_minutes() {
    let mut r = Request::start(Duration::from_secs(300)).await;
    r.socket.write_all(SSE_HEADERS).await.unwrap();
    r.send(json!({"type": "response.created"})).await;
    for _ in 0..16 {
        tokio::time::advance(Duration::from_secs(240)).await;
        r.send(json!({"type": "response.reasoning_text.delta", "delta": "thinking"}))
            .await;
    }
    r.send(json!({"type": "response.output_item.done", "item": {
        "type": "function_call", "call_id": "call_1", "name": "read_file", "arguments": "{}"
    }}))
    .await;
    r.send(json!({"type": "response.completed", "response": {}}))
        .await;
    let completion = (&mut r.task).await.unwrap().unwrap();
    assert_eq!(completion.items.len(), 1);
}

#[tokio::test]
async fn silent_headers_and_stream_still_time_out() {
    for phase in ["headers", "stream"] {
        let mut r = Request::start(Duration::from_secs(300)).await;
        if phase == "stream" {
            r.socket.write_all(SSE_HEADERS).await.unwrap();
            r.send(json!({"type": "response.created"})).await;
        }
        tokio::time::advance(Duration::from_secs(301)).await;
        settle(|| r.task.is_finished()).await;
        let error = (&mut r.task).await.unwrap().unwrap_err().to_string();
        assert!(
            error.contains("failed after 1 attempts"),
            "{phase}: {error}"
        );
        tokio::time::resume();
    }
}

#[tokio::test]
async fn silent_error_body_still_times_out() {
    let mut r = Request::start(Duration::from_millis(100)).await;
    // Use real time here: the client does not expose when it starts reading an
    // error body, so advancing immediately could time out the headers instead.
    tokio::time::resume();
    r.socket
        .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 1000\r\n\r\nx")
        .await
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(5), &mut r.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(error.contains("HTTP 503"), "{error}");
}

#[tokio::test]
async fn unfinished_tool_call_is_not_returned_on_disconnect() {
    let mut r = Request::start(Duration::from_secs(300)).await;
    r.socket.write_all(SSE_HEADERS).await.unwrap();
    r.send(json!({"type": "response.function_call_arguments.delta", "delta": "{\"path\":"}))
        .await;
    r.socket.shutdown().await.unwrap();
    settle(|| r.task.is_finished()).await;
    let error = (&mut r.task).await.unwrap().unwrap_err().to_string();
    assert!(
        error.contains("stream closed before response.completed"),
        "{error}"
    );
}
