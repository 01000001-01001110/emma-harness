//! The loopback HTTP stub every provider's wire tests drive.
//!
//! A real socket rather than an injectable transport, and the choice is
//! deliberate. The interesting assertions are about *wire bytes*: where the
//! cache breakpoints land, that the prefix order survives serialisation, that a
//! `retry-after` header is honoured, that reqwest re-sends on a fresh
//! connection. A transport trait would test the assembler while stubbing out
//! exactly the layer those claims live in, and it would put a seam in the
//! public API that exists only for tests.
//!
//! It lives here rather than inside `anthropic.rs`'s test module because
//! `openai_compat.rs` needs the same socket for the same reasons, and two
//! copies of a stub that must agree about chunked reads and `connection: close`
//! is a slower way to have one.

use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct Reply {
    status: u16,
    content_type: &'static str,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    pub fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn sse(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn error(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "application/json",
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn with_header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.to_string(), v.to_string()));
        self
    }

    fn wire(&self) -> String {
        let extra: String = self
            .headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}\r\n"))
            .collect();
        format!(
            "HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n{}\r\n{}",
            self.status,
            self.content_type,
            self.body.len(),
            extra,
            self.body
        )
    }
}

pub struct Stub {
    pub url: String,
    seen: Arc<Mutex<Vec<Value>>>,
}

impl Stub {
    pub fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }

    pub fn last(&self) -> Value {
        self.requests().pop().expect("the stub saw a request")
    }
}

fn headers_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn content_length(head: &[u8]) -> usize {
    String::from_utf8_lossy(head)
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0)
}

/// Serve `replies` in order, one per connection, recording each request
/// body. `connection: close` on every reply means a retry arrives as a new
/// connection, which is what makes the sequence observable.
pub async fn stub(replies: Vec<Reply>) -> Stub {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured = seen.clone();
    tokio::spawn(async move {
        for reply in replies {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut raw: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..n]);
                if let Some(h) = headers_end(&raw) {
                    let len = content_length(&raw[..h]);
                    if raw.len() >= h + len {
                        if let Ok(v) = serde_json::from_slice(&raw[h..h + len]) {
                            captured.lock().unwrap().push(v);
                        }
                        break;
                    }
                }
            }
            let _ = sock.write_all(reply.wire().as_bytes()).await;
            let _ = sock.flush().await;
            let _ = sock.shutdown().await;
        }
    });
    Stub {
        url: format!("http://{addr}/v1/messages"),
        seen,
    }
}
