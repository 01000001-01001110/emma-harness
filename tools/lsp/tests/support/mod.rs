//! A language server that is not one, and a sandbox to point it at.
//!
//! Everything hard about this crate — readiness, death, request correlation,
//! containment of the paths a server answers with — is a property of *what the
//! server said*, not of rust-analyzer. So the tests that check those drive a
//! fake over a pair of in-memory pipes: deterministic, instant, and able to
//! produce the cases a real server produces once an hour and never on demand,
//! like dying in the middle of a request or reporting an index that never
//! finishes.
//!
//! The real server gets its own file. See `tests/real_server.rs`, and the note
//! there about what it can and cannot assert.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use emma_tools_lsp::client::Client;
use emma_tools_lsp::lang::{self, Language};
use emma_tools_lsp::proto::{self, Frame};
use emma_tools_lsp::server::{Server, Source};
use serde_json::{json, Value};
use tokio::io::BufReader;

// region: The fake server
// ---------------------------------------------------------------------------
// The fake server
//
// A scripted peer: it answers `initialize`, then does whatever the test asked
// about indexing, then answers requests from a table.
// ---------------------------------------------------------------------------

/// What the fake does about indexing after the handshake.
///
/// [`Indexing::Finishes`] and [`Indexing::GapsThenSettles`] are the two that
/// matter, and the difference between them is the bug this crate had. The first
/// mirrors what rust-analyzer 0.3.3008 actually does: progress phases whose
/// open-token count returns to zero several times, and then an
/// `experimental/serverStatus` saying `quiescent: true` when it is genuinely
/// done. The second is a server with no `serverStatus` at all, which is where
/// the progress heuristic and its settle window have to carry the weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indexing {
    /// The real server's shape: three phases with gaps between them where no
    /// token is open, then the authoritative `quiescent: true`. A client that
    /// believes the gaps declares itself ready three times too early.
    Finishes,
    /// Progress only, with the same misleading gaps and no `serverStatus`. The
    /// settle window is the only thing between this and a wrong answer.
    GapsThenSettles,
    /// Announce indexing and never end it — a cold rust-analyzer on a large
    /// workspace, which is the state most answers are wrong in.
    NeverFinishes,
    /// Say nothing at all. A server that does not report progress, which is
    /// indistinguishable from one that is quietly still working.
    Silent,
    /// Quiescent, and broken: the project model could not be loaded. Perfectly
    /// ready, and every answer it gives is empty.
    Unhealthy,
    /// Close the connection right after the handshake.
    DiesAfterHandshake,
}

/// What the fake does about `textDocument/publishDiagnostics` when it is told
/// about a file.
///
/// The three cases are the whole reason `Diagnostics` is safe to ship, and two
/// of them look identical from anywhere except the client's `Option`:
/// [`Publishes::Nothing`] and [`Publishes::Clean`] both produce a result with no
/// diagnostics in it, and they mean opposite things.
#[derive(Debug, Clone)]
pub enum Publishes {
    /// Say nothing on `didOpen`, like a server that is still thinking or that
    /// has no diagnostics engine at all. Not the same as a clean file.
    Nothing,
    /// Publish an empty array: the server looked and found nothing.
    Clean,
    /// Publish these.
    These(Vec<Value>),
}

pub struct Fake {
    indexing: Indexing,
    publishes: Publishes,
    respell_uris: bool,
    responses: HashMap<String, Value>,
    /// Everything the client sent, for the tests that care that a `didOpen`
    /// happened, or that it happened exactly once.
    pub sent: Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Fake {
    pub fn new(indexing: Indexing) -> Self {
        Self {
            indexing,
            publishes: Publishes::Nothing,
            respell_uris: false,
            responses: HashMap::new(),
            sent: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    pub fn publishing(mut self, publishes: Publishes) -> Self {
        self.publishes = publishes;
        self
    }

    /// Publish about a file using the URI spelling a real server uses, rather
    /// than echoing back the one it was handed.
    ///
    /// **The one thing a fake gets wrong for free.** rust-analyzer lower-cases
    /// the Windows drive letter in everything it sends, `file:///c:/…` for the
    /// `file:///C:/…` it was given, and a fake that replies with the client's
    /// own string makes the two spellings identical inside the suite. Measured
    /// 2026-09-06: the real server published four times and the tool reported
    /// silence, over a map lookup that every fake test agreed worked.
    ///
    /// **The respelling is the platform's own, and that is load-bearing.** This
    /// helper lower-cased the whole URI until 2026-09-06, which is right on
    /// Windows and wrong everywhere else: a unix path is case-sensitive, so a
    /// lower-cased one names a different file and `published_key` is correct to
    /// refuse it. On unix the respelling is percent-encoding instead, which is
    /// the thing servers there actually vary: the escape is optional, the hex
    /// case is unspecified, and a client that compares URI strings breaks on
    /// both. Either way the test asks the same question, which is that the
    /// comparison is not a string comparison.
    pub fn spelling_uris_as_a_real_server_does(mut self) -> Self {
        self.respell_uris = true;
        self
    }

    /// The URI a real server would send back for `uri`, on this platform.
    fn respelled(uri: &str) -> String {
        if cfg!(windows) {
            // The drive letter only. Lower-casing the rest would be a claim
            // about the file system that Windows happens to forgive and that
            // no server makes.
            return uri.to_lowercase();
        }
        // Every `-` written as its escape, in lower-case hex. `doc::from_uri`
        // decodes it; a lookup keyed on the raw string does not.
        uri.replace('-', "%2d")
    }

    pub fn answers(mut self, method: &str, result: Value) -> Self {
        self.responses.insert(method.to_string(), result);
        self
    }

    /// Start the fake and hand back a connected, handshaken client, claiming to
    /// be a rust server.
    pub async fn start(self, root: &Path) -> Arc<Client> {
        self.start_as("rust", root).await
    }

    /// The same, wearing one language's identity.
    pub async fn start_as(self, language: &str, root: &Path) -> Arc<Client> {
        // Two duplex pairs: one carrying client→server, one server→client.
        // `duplex` gives a single bidirectional pipe, so a pair of them is how
        // two independent directions are spelled.
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client_side);
        let (server_read, mut server_write) = tokio::io::split(server_side);

        let sent = self.sent.clone();
        let indexing = self.indexing;
        let publishes = self.publishes;
        let respell_uris = self.respell_uris;
        let responses = self.responses;
        tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            loop {
                let message = match proto::read_message(&mut reader).await {
                    Ok(Frame::Message(m)) => m,
                    _ => break,
                };
                sent.lock().expect("sent").push(message.clone());
                let method = message["method"].as_str().unwrap_or_default().to_string();
                let id = message.get("id").cloned();

                if method == "initialize" {
                    let reply =
                        json!({ "jsonrpc": "2.0", "id": id, "result": { "capabilities": {} } });
                    if proto::write_message(&mut server_write, &reply)
                        .await
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
                if method == "initialized" {
                    // The three-phase shape traced from rust-analyzer 0.3.3008:
                    // the open-token count returns to zero between phases, and a
                    // client that treats a zero count as "done" answers from an
                    // index that is not there yet.
                    let phases = [
                        ("rustAnalyzer/Fetching", "rustAnalyzer/Building CrateGraph"),
                        ("rustAnalyzer/Roots Scanned", "rustAnalyzer/Fetching"),
                        (
                            "rustAnalyzer/Building CrateGraph",
                            "rustAnalyzer/cachePriming",
                        ),
                    ];
                    match indexing {
                        Indexing::DiesAfterHandshake => break,
                        Indexing::Silent => {}
                        Indexing::NeverFinishes => {
                            let _ =
                                proto::write_message(&mut server_write, &status(false, "ok")).await;
                            let _ = proto::write_message(
                                &mut server_write,
                                &progress("rustAnalyzer/Indexing", "begin"),
                            )
                            .await;
                        }
                        Indexing::Unhealthy => {
                            let _ = proto::write_message(&mut server_write, &status(true, "error"))
                                .await;
                        }
                        Indexing::Finishes | Indexing::GapsThenSettles => {
                            if indexing == Indexing::Finishes {
                                let _ =
                                    proto::write_message(&mut server_write, &status(false, "ok"))
                                        .await;
                            }
                            for (a, b) in phases {
                                for token in [a, b] {
                                    let _ = proto::write_message(
                                        &mut server_write,
                                        &progress(token, "begin"),
                                    )
                                    .await;
                                    let _ = proto::write_message(
                                        &mut server_write,
                                        &progress(token, "end"),
                                    )
                                    .await;
                                }
                            }
                            if indexing == Indexing::Finishes {
                                let _ =
                                    proto::write_message(&mut server_write, &status(true, "ok"))
                                        .await;
                            }
                        }
                    }
                    continue;
                }
                // A real server publishes diagnostics in response to being told
                // about a file, not in response to a request. The fake does the
                // same, and `Publishes::Nothing` is the case that has to stay
                // possible: it is what silence looks like.
                if method == "textDocument/didOpen" || method == "textDocument/didChange" {
                    let mut uri = message["params"]["textDocument"]["uri"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    if respell_uris {
                        uri = Self::respelled(&uri);
                    }
                    let items = match &publishes {
                        Publishes::Nothing => None,
                        Publishes::Clean => Some(Vec::new()),
                        Publishes::These(items) => Some(items.clone()),
                    };
                    if let Some(items) = items {
                        let note = json!({
                            "jsonrpc": "2.0",
                            "method": "textDocument/publishDiagnostics",
                            "params": { "uri": uri, "diagnostics": items },
                        });
                        if proto::write_message(&mut server_write, &note)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    continue;
                }
                // Notifications have no id and get no reply, exactly as the
                // protocol requires — a fake that replied to `didOpen` would
                // hide a real bug in the client's id handling.
                let Some(id) = id else { continue };
                let result = responses.get(&method).cloned().unwrap_or(Value::Null);
                let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                if proto::write_message(&mut server_write, &reply)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        Client::connect(fake_server_for(language), root, client_read, client_write)
            .await
            .expect("the fake completes a handshake")
    }
}

fn progress(token: &str, kind: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": { "token": token, "value": { "kind": kind, "title": token } },
    })
}

/// `experimental/serverStatus`, spelled exactly as rust-analyzer 0.3.3008 sends
/// it — including `quiescent`, which is the only trustworthy "I have finished"
/// on the wire.
fn status(quiescent: bool, health: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "experimental/serverStatus",
        "params": {
            "health": health,
            "quiescent": quiescent,
            "message": if health == "ok" { Value::Null } else { json!("cargo metadata failed") },
        },
    })
}

pub fn fake_server() -> Server {
    fake_server_for("rust")
}

/// The fake, wearing one language's identity.
///
/// Which language a fake claims to be matters now that the table decides the
/// `languageId` on `didOpen`, the `initializationOptions` on the handshake and
/// half the pool key. A test that wants to prove the bash path is not the rust
/// path asks for `"bash"` here.
pub fn fake_server_for(key: &str) -> Server {
    let language: &'static Language = lang::by_key(key).expect("a language in the table");
    Server {
        program: PathBuf::from("/fake/language-server"),
        args: Vec::new(),
        entry: PathBuf::from("/fake/language-server"),
        version: "0.0.0-fake".into(),
        source: Source::Override,
        language,
    }
}

// endregion: The fake server

// region: A sandbox
// ---------------------------------------------------------------------------
// A sandbox
//
// The same shape `tools/fs` uses: a temporary root, a `ToolCtx` pointing at it,
// and a fingerprint so a test can assert nothing moved.
// ---------------------------------------------------------------------------

pub struct Sandbox {
    pub dir: tempfile::TempDir,
    pub ctx: emma_tool_api::ToolCtx,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = emma_tool_api::ToolCtx {
            cwd: dir.path().to_path_buf(),
            session_id: "test".into(),
            turn_id: "test".into(),
            background: Default::default(),
        };
        Self { dir, ctx }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The canonical root, which is what the tools resolve against and what the
    /// pool keys on. On Windows this is the `\\?\` verbatim form, and a test
    /// comparing against `dir.path()` would silently never match.
    pub fn canonical(&self) -> PathBuf {
        self.dir.path().canonicalize().expect("canonical root")
    }

    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.dir.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, contents).expect("write");
        path
    }
}

/// Every file under `root`, with its bytes. Enough to prove a read-only tool
/// changed nothing.
pub fn fingerprint(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                out.push((
                    path.strip_prefix(base)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/"),
                    std::fs::read(&path).unwrap_or_default(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

// endregion: A sandbox
