//! One long-lived language server, and the two hard things about owning one:
//! knowing when its answers mean anything, and making sure it dies.
//!
//! # Readiness, which is the whole problem
//!
//! rust-analyzer answers `textDocument/references` the moment the handshake
//! completes. For the first ten to sixty seconds of a session the answer is
//! `[]`. That is the worst possible failure for this crate, because **"no
//! references" and "not indexed yet" are the same shape**: an empty list, an
//! `Ok`, and a model that concludes nothing uses this function and deletes it.
//!
//! So this file tracks readiness explicitly rather than sleeping and hoping,
//! and it took two goes, because the obvious mechanism is wrong in a way that
//! only a real server shows you.
//!
//! **The signal that works: `experimental/serverStatus`.** rust-analyzer sends
//! it to any client that declares `experimental.serverStatusNotification`, and
//! `quiescent: true` means precisely "I have finished everything I was doing".
//! It also carries `health`, which is how a broken project model — a
//! `cargo metadata` that failed, a `Cargo.toml` that does not parse — becomes
//! something the model is told about rather than something it infers from thin
//! answers.
//!
//! **The mechanism that does not work, recorded because it is the obvious
//! one.** LSP work-done progress: count the open `rustAnalyzer/*` tokens, call
//! it ready when the count returns to zero. Traced against rust-analyzer
//! 0.3.3008 on a two-file crate, that count reaches zero **three times** before
//! the index exists:
//!
//! ```text
//! 0.01  Fetching begin        0.68  Fetching end / Fetching begin
//! 0.34  Fetching end          0.68  cachePriming begin, end     ← zero again
//! 0.34  Building CrateGraph   0.99  Fetching end
//! 0.36  …end        ← zero    1.01  Building CrateGraph begin, end ← zero
//! 0.36  Roots Scanned begin   1.01  cachePriming begin
//! 0.54  Roots Scanned end     1.83  cachePriming end
//!                             1.84  serverStatus quiescent: true
//! ```
//!
//! The phases are sequential handoffs with gaps of milliseconds between them,
//! and the counter is momentarily zero inside each gap. The first version of
//! this file declared `Ready` at 0.36s, asked, and got `[]` — the exact
//! confidently-wrong empty result the whole crate exists to prevent, produced by
//! the machinery built to prevent it. It was caught by `tests/real_server.rs`
//! and by nothing else: every fake passes, because a fake emits the tokens the
//! author expected.
//!
//! Progress is still tracked, as the fallback for a server that does not send
//! `serverStatus` — but with a **settle window**: all tokens closed *and*
//! nothing new for [`SETTLE`]. That is a heuristic and is treated as one. When
//! the authoritative signal is available it wins outright.
//!
//! The state machine:
//!
//! - `Handshaking` — nothing has been heard yet.
//! - `Indexing` — work is in progress, or has been and has not settled.
//! - `Ready` — `quiescent: true`, or progress settled with no status support.
//! - `Unknown` — nothing arrived within [`FIRST_PROGRESS_GRACE`].
//!
//! `Unknown` is the other subtlety, and the reason a naive implementation is
//! wrong even with the right signal. "No work is currently in progress" is true
//! both of a finished index and of one that has not been announced yet, so a
//! request in the first fifty milliseconds would be declared ready. A request
//! therefore waits for the *first* notification of any kind before believing
//! anything, and if none comes it does not upgrade to `Ready` — it stays
//! `Unknown` and says so. That is the honest answer for a server that reports
//! nothing, and it is indistinguishable from one quietly still working.
//!
//! **What a request before readiness returns.** It runs anyway — a partial
//! answer is often the right one and refusing would make the first tool call of
//! every session useless — and [`Answer::readiness`] travels back with the
//! result so the tool layer can put it in front of the model. The rule the tool
//! layer implements, and the reason the flag exists: an **empty** result from a
//! server that is not `Ready` is never rendered as "none found". See
//! `render::locations`.
//!
//! It stays `Ok` rather than becoming `Failed`, and that is a deliberate ruling
//! rather than an oversight. `Failed` would be defensible — nobody answered the
//! question — but Emma's loop will not repeat a call that already failed with
//! nothing changed since, and "wait and ask again" is exactly the move the model
//! should make here. An error would forbid the one correct response.
//!
//! # Lifecycle
//!
//! Starting rust-analyzer per request is unusable: seconds of spawn plus a full
//! re-index for one question. So the server is long-lived, shared, and owned by
//! [`crate::pool::Pool`], which the tools hold behind an `Arc` exactly as
//! `tools/fs` holds its `ReadTracker`.
//!
//! **Cleanup lives in `Drop`, not in a shutdown path somebody has to call.**
//! `tools/web` shipped a real leak of exactly this class — teardown ran in
//! `main`, which is correct for a binary and wrong for a library, so every use
//! that was not the binary leaked a Chrome. The mistake is not "forgot to clean
//! up"; it is putting cleanup where only one of the callers goes. Here the child
//! is spawned `kill_on_drop(true)` **and** `Drop` calls `start_kill` and aborts
//! the two pump tasks, so a dropped `Client` takes its process with it whether
//! or not anybody remembered anything. `tests/lifecycle.rs` asserts it against a
//! real process rather than trusting the flag.
//!
//! **If it dies mid-request**, the reader loop hits end-of-stream, records the
//! exit status and the tail of stderr, and resolves *every* pending request with
//! that message — rather than leaving them to time out one by one at sixty
//! seconds each. The model gets "the server exited" promptly, which is a fact it
//! can route on. The dead client is not reused: the pool notices and starts a
//! fresh one on the next call, up to a crash ceiling, so a server that dies on
//! startup does not become an invisible per-call retry loop.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use emma_tool_api::ToolError;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::doc;
use crate::proto::{self, Frame};
use crate::server::Server;

// region: The numbers
// ---------------------------------------------------------------------------
// The numbers
//
// Every timeout in the crate, in one place, with what each is protecting
// against. All of them are ceilings on waiting, never on correctness: nothing
// here decides an answer, only how long Emma is willing to hang before saying
// what happened.
// ---------------------------------------------------------------------------

/// How long to wait for the *first* progress notification before concluding the
/// server does not report progress. Generous: rust-analyzer emits within a
/// second or so, and the cost of being wrong in this direction is answering a
/// question against a half-built index while claiming to be sure.
pub const FIRST_PROGRESS_GRACE: Duration = Duration::from_secs(10);

/// How long to wait for indexing to finish once it has started. A cold
/// rust-analyzer on a large workspace takes minutes; this is the point at which
/// Emma stops waiting and asks anyway, saying that it did.
pub const READY_TIMEOUT: Duration = Duration::from_secs(180);

/// Overrides [`READY_TIMEOUT`]. Present because the right number is a property
/// of the repository, not of Emma, and the cost of getting it wrong is either a
/// three-minute stall or a wrong-shaped answer.
pub const READY_TIMEOUT_ENV: &str = "EMMA_LSP_READY_TIMEOUT_MS";

/// How long the progress *fallback* waits, after every indexing token has
/// closed, for another one to open before calling the index settled.
///
/// Only reached when a server does not send `experimental/serverStatus`.
/// Measured against the trace in the module doc: the gaps between rust-analyzer's
/// phases are 0–320ms, so this is roughly five times the largest observed one.
/// It is a heuristic and is confined to the path where there is nothing better.
pub const SETTLE: Duration = Duration::from_millis(1_500);

/// How long any one LSP request may take once the server is ready.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long `initialize` may take. Longer than a request because it is the one
/// that loads a project model.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(90);

/// How much of the server's stderr to keep for the death message. The end
/// rather than the beginning: a panic message is the last thing printed.
const STDERR_TAIL_BYTES: usize = 4096;

/// How long to wait for a server to publish diagnostics for a file it has just
/// been told about.
///
/// Twenty seconds, and it is a guess rather than a measurement, which is why it
/// is overridable and why the tool prints the number it waited. The servers this
/// matters most for (bash, ansible, terraform) publish within a second of
/// `didOpen`; the ceiling is there for a cold project model, and running into it
/// produces the honest third outcome rather than a wrong one.
pub const DIAGNOSTICS_WAIT: Duration = Duration::from_secs(20);

/// Overrides [`DIAGNOSTICS_WAIT`], in milliseconds. The tests use it to make a
/// twenty-second ceiling a fifty-millisecond one.
pub const DIAGNOSTICS_WAIT_ENV: &str = "EMMA_LSP_DIAGNOSTICS_MS";

fn diagnostics_wait() -> Duration {
    std::env::var(DIAGNOSTICS_WAIT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(DIAGNOSTICS_WAIT)
}

fn ready_timeout() -> Duration {
    std::env::var(READY_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(READY_TIMEOUT)
}

// endregion: The numbers

// region: Readiness
// ---------------------------------------------------------------------------
// Readiness
//
// The four states and the sentence each of them puts in front of the model.
// The sentences are here rather than at the render site because they are the
// contract — the render layer decides where they go, not what they say.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Handshaking,
    Indexing,
    Ready,
    Unknown,
}

impl Readiness {
    pub fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// What the model is told when the answer was produced in this state.
    /// `None` for `Ready`, because a caveat on every correct answer is noise
    /// that teaches the model to skip caveats.
    pub fn caveat(self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::Indexing | Self::Handshaking => Some(
                "The language server was still indexing when this was asked, and did not \
                 finish within the wait. This answer may be incomplete — an empty or short \
                 result here is not evidence that there is nothing to find. Ask again in a \
                 few seconds.",
            ),
            Self::Unknown => Some(
                "The language server never reported that it had finished indexing, so Emma \
                 cannot tell whether this answer is complete. An empty or short result here \
                 is not evidence that there is nothing to find.",
            ),
        }
    }
}

/// What the reader has heard, which is not quite the same question as
/// [`Readiness`] — the difference is [`Phase::Quiet`], which means "progress
/// says nothing is running" and is only *evidence* of readiness. Turning it into
/// an answer takes the settle wait in [`Client::wait_ready`], and only when the
/// server has not offered the authoritative signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Nothing heard at all.
    Handshaking,
    /// Work is in progress.
    Working,
    /// Every indexing token has closed, having been open. Not `Ready`: see the
    /// trace in the module doc, where this is true three times before the index
    /// exists.
    Quiet,
    /// `experimental/serverStatus` said `quiescent: true`. Authoritative.
    Quiescent,
}

// endregion: Readiness

// region: What a call gets back
// ---------------------------------------------------------------------------
// What a call gets back
//
// A result and the readiness it was produced under, travelling together. They
// are one type on purpose: separating them is how a caller ends up rendering an
// empty list without the caveat that makes it honest.
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct Answer {
    pub value: Value,
    pub readiness: Readiness,
    /// What the server said about its own health, when it was not `ok`.
    ///
    /// This exists because a broken project model — `cargo metadata` failed, a
    /// `Cargo.toml` that does not parse, a workspace that could not be loaded —
    /// produces a server that is perfectly *quiescent* and answers every
    /// question with nothing. Readiness alone would report that as a clean
    /// `Ready`, and the empty answer would read as fact. This is the one channel
    /// that can say otherwise, so it is carried rather than logged.
    pub health: Option<String>,
}

/// What a `Diagnostics` call gets back.
///
/// Separate from [`Answer`] because the interesting field is an `Option` and
/// the whole point of the tool is that the two empty cases are different. See
/// [`Client::diagnostics`].
#[derive(Debug)]
pub struct Diagnosis {
    /// The server's latest publication for this file. `None` means it published
    /// nothing at all inside the wait, which is **not** a clean result.
    pub items: Option<Vec<Value>>,
    /// How long the wait actually took, so the refusal can name a number the
    /// user can change rather than a number they have to guess.
    pub waited: Duration,
    pub readiness: Readiness,
    pub health: Option<String>,
}

// endregion: What a call gets back

// region: The client
// ---------------------------------------------------------------------------
// The client
//
// Spawn, handshake, request, and the document sync that has to happen before
// any position-based question means anything.
// ---------------------------------------------------------------------------

/// What rust-analyzer is told at `initialize`.
///
/// Moved into [`crate::lang::RUST_INIT`], where it sits beside the other six
/// languages' options, and kept under its old name here because it is the
/// switch that makes `read_only: true` and `reaches_network: false` true rather
/// than convenient, and everything written about this crate names it. The
/// argument for each switch is on `RUST_INIT` itself.
///
/// Every language now carries its own `initializationOptions`; the handshake
/// reads [`crate::lang::Language::init_options`] and this constant is rust's.
pub const INIT_OPTIONS: &str = crate::lang::RUST_INIT;

pub struct Client {
    server: Server,
    root: PathBuf,
    outgoing: mpsc::UnboundedSender<Value>,
    pending: Pending,
    phase: watch::Receiver<Phase>,
    /// Set once a wait has already burned [`FIRST_PROGRESS_GRACE`] with nothing
    /// arriving, so the second call of the session does not pay it again.
    no_progress: Arc<AtomicBool>,
    /// Whether the server has ever sent `experimental/serverStatus`. When it
    /// has, the progress fallback is switched off entirely rather than merely
    /// ranked below it — a heuristic running alongside an authoritative signal
    /// can only ever contradict it.
    speaks_status: Arc<AtomicBool>,
    health: Health,
    death: Death,
    next_id: AtomicI64,
    /// Files this server has been told about, and a hash of what it was told.
    /// Re-sent as `didChange` when the bytes on disk have moved underneath —
    /// which they do constantly, because the whole point of `Diagnostics`-shaped
    /// work is asking right after an `Edit`.
    documents: Mutex<HashMap<PathBuf, Document>>,
    /// What the server has pushed, and a counter that ticks on every push.
    ///
    /// Diagnostics are the one thing in LSP that is not a request. The server
    /// publishes when it feels like it, so a caller cannot ask; it can only
    /// arrange to be told. The counter is what turns "be told" into something
    /// with a deadline: a waiter reads the map, and if the URI is not in it,
    /// waits for the counter to move rather than polling.
    published: Published,
    publish_tick: watch::Receiver<u64>,
    child: Mutex<Option<tokio::process::Child>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

struct Document {
    version: i64,
    hash: u64,
}

/// Every `textDocument/publishDiagnostics` the server has sent, keyed by
/// [`published_key`], keeping only the latest for each. A map rather than a
/// stream because the protocol is last-write-wins: a second publication for a
/// file replaces the first, and a queue of superseded ones would let a stale set
/// be read as current.
type Published = Arc<Mutex<HashMap<String, Vec<Value>>>>;

/// The key a publication is filed and looked up under, and the reason it is not
/// the URI string.
///
/// **Measured against the real server, 2026-09-06, and it is a defect this port
/// would otherwise have shipped.** rust-analyzer sent four
/// `textDocument/publishDiagnostics` notifications for a fixture with an
/// unresolved name in it, and `Diagnostics` reported silence for the full
/// twenty-second wait, twice. The notifications had arrived and been filed; the
/// lookup missed. rust-analyzer spells the drive letter **lower case** in
/// everything it sends back — `file:///c:/…` against the `file:///C:/…` that
/// [`doc::to_uri`] produced — so a `HashMap<String, _>` keyed on the raw URI has
/// two different keys for one file.
///
/// `doc`'s module doc has said since the crate was written that "the comparison
/// in the other direction cannot be a string comparison". This map was the one
/// place that had not read it.
///
/// **No fake could have caught this**, which is why the fix comes with
/// `Fake::spelling_uris_as_a_real_server_does`: a fake echoes back the URI it
/// was handed, so both spellings are the same string inside the test suite and
/// every case passes over a lookup that cannot work.
///
/// The parse goes through [`doc::from_uri`], so percent-encoding and the `\\?\`
/// prefix are handled once, where they already are. Case folding is applied on
/// Windows only, because there a path is case-insensitive and on Unix it is not.
fn published_key(uri: &str) -> String {
    match doc::from_uri(uri) {
        // Lossy is correct here: this is a comparison key, never a path that
        // gets opened. A file whose name does not survive UTF-8 would collide
        // with a sibling that differs only in the unmappable bytes, and that is
        // a better failure than dropping the publication entirely.
        Some(path) if cfg!(windows) => path.to_string_lossy().to_lowercase(),
        Some(path) => path.to_string_lossy().into_owned(),
        // Not a `file://` URI at all. Kept verbatim rather than dropped: a
        // server that publishes about something else is saying something, and
        // an untranslatable key still matches itself.
        None => uri.to_string(),
    }
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, ToolError>>>>>;
type Death = Arc<Mutex<Option<String>>>;
type Health = Arc<Mutex<Option<String>>>;

impl Client {
    /// Spawn a server and complete the handshake.
    ///
    /// Returns as soon as `initialize` has been answered — *not* when indexing
    /// is done. Waiting for the index here would move a three-minute stall into
    /// the pool lock, where it would block every other call; readiness is waited
    /// for per request instead, by [`Client::request`], which is also where the
    /// answer about it belongs.
    pub async fn start(server: &Server, root: &Path) -> Result<Arc<Self>, ToolError> {
        // `program` and `args` rather than the entry point alone. Four of the
        // seven servers are not executables: two are JavaScript run by `node`,
        // one is a .NET assembly run by `dotnet`, and one is a `.ps1`
        // bootstrapper run by `pwsh`. `server::launch_args` built the line.
        let mut child = tokio::process::Command::new(&server.program)
            .args(&server.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Belt to `Drop`'s braces. See the module doc: the leak this is
            // guarding against is not "somebody forgot to call teardown", it is
            // "teardown existed in one caller".
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                ToolError::Unavailable(format!(
                    "{} could not be started: {e}",
                    server.program.display()
                ))
            })?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let stderr_tail: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let tail = stderr_tail.clone();
        let stderr_task = tokio::spawn(async move { pump_stderr(stderr, tail).await });

        let client = Self::over_streams(server.clone(), root, stdout, stdin, stderr_tail);
        client.tasks.lock().expect("tasks").push(stderr_task);
        *client.child.lock().expect("child") = Some(child);

        client.handshake().await?;
        Ok(client)
    }

    /// The seam a fake server is tested through: everything [`Client::start`]
    /// does except owning a process.
    ///
    /// Public, and documented as such, because the alternative is that every
    /// interesting behaviour in this file — readiness, death, request
    /// correlation, document sync — is reachable only by installing
    /// rust-analyzer, and a test suite that needs a language server installed is
    /// a test suite that does not run. The behaviours that genuinely need a
    /// process (that it dies with the `Client`, that a real rust-analyzer
    /// answers) are tested against one; everything else is tested against both
    /// ends of a pipe, deterministically and in milliseconds.
    pub async fn connect<R, W>(
        server: Server,
        root: &Path,
        reader: R,
        writer: W,
    ) -> Result<Arc<Self>, ToolError>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let client = Self::over_streams(
            server,
            root,
            reader,
            writer,
            Arc::new(Mutex::new(String::new())),
        );
        client.handshake().await?;
        Ok(client)
    }

    fn over_streams<R, W>(
        server: Server,
        root: &Path,
        reader: R,
        writer: W,
        stderr_tail: Arc<Mutex<String>>,
    ) -> Arc<Self>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (out_tx, out_rx) = mpsc::unbounded_channel::<Value>();
        let (phase_tx, phase_rx) = watch::channel(Phase::Handshaking);
        let (publish_tx, publish_rx) = watch::channel(0u64);
        let published: Published = Arc::new(Mutex::new(HashMap::new()));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let death: Death = Arc::new(Mutex::new(None));
        let health: Health = Arc::new(Mutex::new(None));
        let speaks_status = Arc::new(AtomicBool::new(false));

        let writer_task = tokio::spawn(pump_writer(writer, out_rx));
        let reader_task = tokio::spawn(pump_reader(
            reader,
            pending.clone(),
            death.clone(),
            phase_tx,
            out_tx.clone(),
            stderr_tail,
            health.clone(),
            speaks_status.clone(),
            published.clone(),
            publish_tx,
        ));

        Arc::new(Self {
            server,
            root: root.to_path_buf(),
            outgoing: out_tx,
            pending,
            phase: phase_rx,
            no_progress: Arc::new(AtomicBool::new(false)),
            speaks_status,
            health,
            death,
            next_id: AtomicI64::new(1),
            documents: Mutex::new(HashMap::new()),
            published,
            publish_tick: publish_rx,
            child: Mutex::new(None),
            tasks: Mutex::new(vec![writer_task, reader_task]),
        })
    }

    pub fn server(&self) -> &Server {
        &self.server
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Why the server is unusable, if it is. The pool consults this rather than
    /// handing out a client that will fail every call.
    pub fn death(&self) -> Option<String> {
        self.death.lock().expect("death").clone()
    }

    async fn handshake(&self) -> Result<(), ToolError> {
        // Per language, from the table. `expect` rather than a fallback: a
        // malformed entry is a bug in this repository, and starting a server
        // with silently-dropped options is how `read_only` stops being true.
        let init_options: Value = serde_json::from_str(self.server.language.init_options)
            .expect("every Language::init_options is valid JSON");
        let params = json!({
            // Sent so the server exits if Emma is killed without unwinding.
            // The one piece of cleanup that survives `kill -9`.
            "processId": std::process::id(),
            "rootUri": doc::to_uri(&self.root),
            "workspaceFolders": [{
                "uri": doc::to_uri(&self.root),
                "name": self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into()),
            }],
            "initializationOptions": init_options,
            "capabilities": {
                // The declaration that makes readiness knowable *correctly*.
                // Without it rust-analyzer does not send
                // `experimental/serverStatus`, and this crate falls back to the
                // progress heuristic — which the module doc shows reaching zero
                // three times before the index exists. This one line is the
                // difference between an answer and a plausible one.
                "experimental": { "serverStatusNotification": true },
                "window": {
                    // The fallback signal, for a server that does not implement
                    // the above. Without it there is nothing at all and every
                    // answer is `Unknown` — a permanent shrug.
                    "workDoneProgress": true,
                },
                "textDocument": {
                    "synchronization": { "dynamicRegistration": false },
                    // Declared because the client now consumes them.
                    // **It is not what makes `Diagnostics` work**, and saying so
                    // is the point: it was added first as the suspected fix for
                    // a real server publishing nothing, and measured on
                    // 2026-09-06 against rust-analyzer 1.94.1 it changed
                    // nothing at all — the server pushes `publishDiagnostics`
                    // with or without it. The actual defect was the URI key,
                    // recorded on [`published_key`]. Kept because a client that
                    // reads a notification should say that it does.
                    "publishDiagnostics": {
                        "relatedInformation": true,
                        "versionSupport": false,
                        "codeDescriptionSupport": true,
                        "dataSupport": true,
                    },
                    "references": { "dynamicRegistration": false },
                    "definition": { "dynamicRegistration": false, "linkSupport": true },
                    "hover": {
                        "dynamicRegistration": false,
                        "contentFormat": ["markdown", "plaintext"],
                    },
                    "documentSymbol": {
                        "dynamicRegistration": false,
                        "hierarchicalDocumentSymbolSupport": true,
                    },
                    // **What is claimed here changes what comes back**, which
                    // makes this block a behaviour rather than a formality.
                    // `snippetSupport: false` is the load-bearing line: with it
                    // true, rust-analyzer sends `push(${1:value})` and a client
                    // that cannot expand a placeholder inserts those braces
                    // into somebody's source. Emma carries the snippet flag
                    // through so a consumer can refuse one, and says here that
                    // it would rather have plain text.
                    //
                    // `insertReplaceSupport` is true because the two ranges
                    // answer different questions and Emma uses the replacing
                    // one: a completion accepted in the middle of a word should
                    // overwrite the word, not leave its tail behind.
                    "completion": {
                        "dynamicRegistration": false,
                        "contextSupport": true,
                        "completionItem": {
                            "snippetSupport": false,
                            "insertReplaceSupport": true,
                            "documentationFormat": ["markdown", "plaintext"],
                            "labelDetailsSupport": true,
                        },
                    },
                    // The other half of typing help. `activeParameterSupport`
                    // is what lets the caller mark which argument the cursor is
                    // in; without it a signature is a line of text with no
                    // indication of where you are in it.
                    "signatureHelp": {
                        "dynamicRegistration": false,
                        "signatureInformation": {
                            "documentationFormat": ["markdown", "plaintext"],
                            "parameterInformation": { "labelOffsetSupport": true },
                            "activeParameterSupport": true,
                        },
                    },
                },
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true,
                },
                // Position encoding is deliberately not negotiated. Not
                // declaring `general.positionEncodings` leaves the protocol
                // default of UTF-16 in force, which `doc` implements exactly
                // once, in one place, with tests. Offering UTF-8 would mean two
                // encodings in the codebase and a silent column error on any
                // line with a non-ASCII character in it — the class of bug that
                // shows up as an answer about the wrong symbol.
            },
        });

        let result = self
            .raw_request("initialize", params, HANDSHAKE_TIMEOUT)
            .await?;
        // Not inspected beyond existing. Emma asks for four things every LSP
        // server implements; refusing to start over a missing capability would
        // trade a working tool for a strict one.
        let _ = result;
        self.notify("initialized", json!({}));
        Ok(())
    }

    /// The current readiness without waiting for anything.
    pub fn readiness(&self) -> Readiness {
        match *self.phase.borrow() {
            Phase::Quiescent => Readiness::Ready,
            Phase::Handshaking => Readiness::Handshaking,
            // `Quiet` is deliberately reported as `Indexing` here. It is
            // evidence, not an answer, and only [`Client::wait_ready`] is
            // entitled to promote it — after the settle wait, and only when the
            // server offers nothing better.
            Phase::Working | Phase::Quiet => Readiness::Indexing,
        }
    }

    /// What the server last said about its own health, when it was not `ok`.
    pub fn health(&self) -> Option<String> {
        self.health.lock().expect("health").clone()
    }

    /// Wait for the server to finish indexing, up to the ceilings.
    ///
    /// Never fails. Every outcome — ready, timed out, never reported — is a
    /// [`Readiness`] the caller travels onward with, because "I waited and it is
    /// still not ready" is not a failure of the call; it is a fact about the
    /// answer that follows.
    pub async fn wait_ready(&self) -> Readiness {
        let mut rx = self.phase.clone();
        if *rx.borrow_and_update() == Phase::Quiescent {
            return Readiness::Ready;
        }

        // Phase one: has anything at all been heard? Until something has, "no
        // work is in progress" is indistinguishable from "work has not been
        // announced yet", and treating the second as ready is the bug this whole
        // file exists to avoid.
        if *rx.borrow() == Phase::Handshaking && !self.no_progress.load(Ordering::Relaxed) {
            match tokio::time::timeout(FIRST_PROGRESS_GRACE, rx.changed()).await {
                Ok(Ok(())) => {}
                // The channel closing means the reader task is gone, which means
                // the server died. Nothing more will be reported, ever.
                Ok(Err(_)) => return self.readiness(),
                Err(_) => {
                    self.no_progress.store(true, Ordering::Relaxed);
                    return Readiness::Unknown;
                }
            }
        }
        if self.no_progress.load(Ordering::Relaxed) && *rx.borrow() != Phase::Quiescent {
            return Readiness::Unknown;
        }

        // Phase two: wait it out, until the ceiling.
        let deadline = tokio::time::Instant::now() + ready_timeout();
        loop {
            let now = *rx.borrow_and_update();
            if now == Phase::Quiescent {
                return Readiness::Ready;
            }

            // The settle window, and the whole of the correction this file's
            // module doc records. `Quiet` means every indexing token has closed
            // — which happens three times during a rust-analyzer startup, in the
            // millisecond gaps between one phase ending and the next beginning.
            // Waiting to see whether anything else starts is what tells a gap
            // from an ending. Only reached when the server has not offered
            // `serverStatus`, because a heuristic beside an authoritative signal
            // can only contradict it.
            if now == Phase::Quiet && !self.speaks_status.load(Ordering::Relaxed) {
                let settle = tokio::time::Instant::now() + SETTLE;
                return match tokio::time::timeout_at(settle.min(deadline), rx.changed()).await {
                    // Nothing started in the settle window: as settled as this
                    // server is going to tell us it is.
                    Err(_) if tokio::time::Instant::now() < deadline => Readiness::Ready,
                    Err(_) => Readiness::Indexing,
                    Ok(Err(_)) => self.readiness(),
                    // Something started again — it was a gap, not an ending.
                    Ok(Ok(())) => continue,
                };
            }

            match tokio::time::timeout_at(deadline, rx.changed()).await {
                Ok(Ok(())) => continue,
                Ok(Err(_)) | Err(_) => return self.readiness(),
            }
        }
    }

    /// Tell the server what is in a file, if it does not already know.
    ///
    /// `didOpen` once, then `didChange` whenever the bytes have moved. The
    /// re-sync is not an optimisation: an agent's normal rhythm is `Edit` then
    /// ask, and a server answering from the version it opened five edits ago
    /// returns positions that no longer exist in the file the model is looking
    /// at.
    pub fn sync_document(&self, path: &Path, text: &str) {
        let hash = hash_of(text);
        let notification = {
            let mut docs = self.documents.lock().expect("documents");
            match docs.get_mut(path) {
                None => {
                    docs.insert(path.to_path_buf(), Document { version: 1, hash });
                    Some((
                        "textDocument/didOpen",
                        json!({
                            "textDocument": {
                                "uri": doc::to_uri(path),
                                "languageId": self.server.language.language_id,
                                "version": 1,
                                "text": text,
                            }
                        }),
                    ))
                }
                Some(existing) if existing.hash != hash => {
                    existing.version += 1;
                    existing.hash = hash;
                    Some((
                        "textDocument/didChange",
                        json!({
                            "textDocument": { "uri": doc::to_uri(path), "version": existing.version },
                            // Full sync. Incremental would mean computing a diff
                            // to describe a file Emma just read in its entirety,
                            // to save bytes on a local pipe.
                            "contentChanges": [{ "text": text }],
                        }),
                    ))
                }
                Some(_) => None,
            }
        };
        if let Some((method, params)) = notification {
            self.notify(method, params);
        }
    }

    /// Tell the server the buffer was written to disk.
    ///
    /// Sent only for a file this client has already opened; a `didSave` for a
    /// document the server was never told about is a notification about
    /// nothing. Some servers only re-lint on save, which is the whole reason a
    /// second consumer of this client (the Code page's editor) needs it.
    pub fn did_save(&self, path: &Path) {
        let known = self.documents.lock().expect("documents").contains_key(path);
        if !known {
            return;
        }
        self.notify(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": doc::to_uri(path) } }),
        );
    }

    /// Tell the server the buffer is gone, and forget what it was told.
    ///
    /// The forgetting is the load-bearing half. `sync_document` sends `didOpen`
    /// exactly once per path and `didChange` after that; without dropping the
    /// entry here, a file closed and reopened would be re-synced as a change to
    /// a document the server no longer has. Any diagnostics it published for
    /// the URI go too, because they describe a version nothing is looking at.
    pub fn did_close(&self, path: &Path) {
        let known = self
            .documents
            .lock()
            .expect("documents")
            .remove(path)
            .is_some();
        if !known {
            return;
        }
        let uri = doc::to_uri(path);
        self.published
            .lock()
            .expect("published")
            .remove(&published_key(&uri));
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        );
    }

    /// The server's latest publication for `uri`, without waiting for one.
    ///
    /// [`Client::diagnostics`] is the model's shape: ask, and block until the
    /// answer or the deadline. An editor's shape is the opposite one, and it is
    /// the shape LSP actually has: the server publishes when it has something
    /// to say, and a reader takes what is there. Same `Option` rule either way,
    /// and for the same reason: `None` is "nothing has been published", which
    /// is not "the file is clean".
    pub fn published_for(&self, uri: &str) -> Option<Vec<Value>> {
        self.published
            .lock()
            .expect("published")
            .get(&published_key(uri))
            .cloned()
    }

    /// A receiver that ticks every time the server publishes anything.
    ///
    /// What turns [`Client::published_for`] from a poll into a subscription. A
    /// second consumer gets its own `Receiver` from the same `watch`, so an
    /// editor watching for pushes and a tool waiting inside
    /// [`Client::diagnostics`] neither block nor starve each other.
    pub fn publish_tick(&self) -> watch::Receiver<u64> {
        self.publish_tick.clone()
    }

    /// A request, waited for readiness first, with the readiness attached.
    ///
    /// This is the only entry point the tools use, and the reason it is the only
    /// one is that it makes forgetting to wait impossible.
    pub async fn request(&self, method: &str, params: Value) -> Result<Answer, ToolError> {
        let readiness = self.wait_ready().await;
        let value = self.raw_request(method, params, REQUEST_TIMEOUT).await?;
        Ok(Answer {
            value,
            readiness,
            health: self.health(),
        })
    }

    /// Wait, bounded, for the server to publish diagnostics for one file.
    ///
    /// **This is the only pull over a push in the whole crate, and the reason
    /// it is safe is the third outcome.** `textDocument/publishDiagnostics` is
    /// a notification: nothing was requested, so nothing is owed, and a server
    /// that has nothing to say says nothing. Silence and a clean file are the
    /// same shape on the wire, and a tool that reported both as "no problems"
    /// would be inventing a clean bill of health out of a timeout.
    ///
    /// So [`Diagnosis::items`] is an `Option`, and `None` is not an empty list.
    /// One means the server said the file is clean; the other means it did not
    /// answer, and [`crate::render`] gives them different sentences.
    ///
    /// Readiness is waited for first, exactly as [`Client::request`] does, and
    /// travels back with the answer for the same reason.
    pub async fn diagnostics(&self, uri: &str) -> Result<Diagnosis, ToolError> {
        let readiness = self.wait_ready().await;
        let started = tokio::time::Instant::now();
        let deadline = started + diagnostics_wait();
        let mut rx = self.publish_tick.clone();
        let key = published_key(uri);
        let peek = |c: &Self| c.published.lock().expect("published").get(&key).cloned();

        let items = loop {
            if let Some(items) = peek(self) {
                break Some(items);
            }
            // A dead server is reported as a failure rather than waited out.
            // Twenty seconds of silence from a process that has already exited
            // is the slowest possible way to learn nothing.
            if let Some(why) = self.death() {
                return Err(ToolError::Failed(why));
            }
            match tokio::time::timeout_at(deadline, rx.changed()).await {
                Ok(Ok(())) => continue,
                // The sender is gone, which means the reader task has ended.
                // One last look, then give up.
                Ok(Err(_)) => break peek(self),
                Err(_) => break None,
            }
        };

        Ok(Diagnosis {
            items,
            waited: started.elapsed(),
            readiness,
            health: self.health(),
        })
    }

    async fn raw_request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ToolError> {
        if let Some(why) = self.death() {
            return Err(ToolError::Failed(why));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().expect("pending").insert(id, tx);

        let sent = self.outgoing.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        if sent.is_err() {
            self.pending.lock().expect("pending").remove(&id);
            return Err(ToolError::Failed(self.death().unwrap_or_else(|| {
                "the language server connection is closed".into()
            })));
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            // The sender was dropped without answering: the reader task ended
            // without draining, which only happens if it panicked. Reported as
            // a death because from here it is indistinguishable from one.
            Ok(Err(_)) => {
                Err(ToolError::Failed(self.death().unwrap_or_else(|| {
                    "the language server stopped answering".into()
                })))
            }
            Err(_) => {
                self.pending.lock().expect("pending").remove(&id);
                Err(ToolError::Failed(format!(
                    "the language server did not answer {method} within {}s. {}",
                    timeout.as_secs(),
                    self.server.banner()
                )))
            }
        }
    }

    fn notify(&self, method: &str, params: Value) {
        // Deliberately ignoring the send failure. A notification has no reply to
        // wait on, so the only thing that could be done with the error is report
        // it against a call that has not been made yet; the death that caused it
        // will surface on the next request, with the exit status attached.
        let _ = self.outgoing.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    /// The polite shutdown, for a caller that has one to spare.
    ///
    /// Emphatically **not** where cleanup lives — see the module doc and
    /// [`Drop`]. This exists so a harness that is exiting cleanly can let
    /// rust-analyzer flush its caches, and nothing depends on it being called.
    pub async fn shutdown(&self) {
        let _ = self
            .raw_request("shutdown", json!(null), Duration::from_secs(5))
            .await;
        self.notify("exit", json!(null));
    }
}

/// The cleanup that actually runs.
///
/// `tools/web`'s leak is the reason this is a `Drop` impl and not a method: that
/// teardown was correct, complete, and reachable only from `main`, so every
/// caller that was not the binary leaked a browser. Cleanup belongs where the
/// value ends, not where one caller happens to finish.
impl Drop for Client {
    fn drop(&mut self) {
        for task in self.tasks.lock().expect("tasks").drain(..) {
            task.abort();
        }
        if let Some(mut child) = self.child.lock().expect("child").take() {
            // `kill_on_drop` would do this when `child` falls out of scope at
            // the end of this function; doing it explicitly means the process is
            // signalled even if a future edit stores the child somewhere that
            // outlives this, and it costs one line.
            let _ = child.start_kill();
        }
    }
}

// endregion: The client

// region: The pumps
// ---------------------------------------------------------------------------
// The pumps
//
// Three tasks: one writing, one reading and dispatching, one keeping the tail
// of stderr so a death has a cause attached. The reader is where readiness is
// decided and where a death is turned into an answer for everyone waiting.
// ---------------------------------------------------------------------------

async fn pump_writer<W: AsyncWrite + Unpin>(mut writer: W, mut rx: mpsc::UnboundedReceiver<Value>) {
    while let Some(message) = rx.recv().await {
        if proto::write_message(&mut writer, &message).await.is_err() {
            // The pipe is gone, which means the child is gone. The reader task
            // is about to see end-of-stream and produce the message that
            // explains it; duplicating that here would race it.
            break;
        }
    }
}

async fn pump_stderr<R: AsyncRead + Unpin>(reader: R, tail: Arc<Mutex<String>>) {
    use tokio::io::AsyncBufReadExt;
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut buf = tail.lock().expect("stderr tail");
        buf.push_str(&line);
        buf.push('\n');
        // Kept from the end: a panic message is the last thing a dying process
        // prints, and the first four kilobytes are startup chatter.
        if buf.len() > STDERR_TAIL_BYTES {
            let cut = buf.len() - STDERR_TAIL_BYTES;
            let cut = buf
                .char_indices()
                .map(|(i, _)| i)
                .find(|i| *i >= cut)
                .unwrap_or(buf.len());
            *buf = buf[cut..].to_string();
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn pump_reader<R: AsyncRead + Unpin>(
    reader: R,
    pending: Pending,
    death: Death,
    ready: watch::Sender<Phase>,
    outgoing: mpsc::UnboundedSender<Value>,
    stderr_tail: Arc<Mutex<String>>,
    health: Health,
    speaks_status: Arc<AtomicBool>,
    published: Published,
    publish_tick: watch::Sender<u64>,
) {
    let mut reader = BufReader::new(reader);
    // Tokens for work the server calls indexing. Counted rather than flagged:
    // rust-analyzer runs several overlapping progresses (roots scanned, crate
    // graph, cache priming) and a flag would report ready at the first `end`.
    let mut indexing: HashSet<String> = HashSet::new();
    let mut ever_indexed = false;

    let cause = loop {
        match proto::read_message(&mut reader).await {
            Ok(Frame::Message(message)) => {
                handle(
                    message,
                    &pending,
                    &ready,
                    &outgoing,
                    &mut indexing,
                    &mut ever_indexed,
                    &health,
                    &speaks_status,
                    &published,
                    &publish_tick,
                );
            }
            Ok(Frame::Eof) => break "the language server exited".to_string(),
            Err(e) => break format!("the language server sent something unreadable: {e}"),
        }
    };

    // One message, assembled once, given to everybody: the death record the next
    // call reads, and the answer to every request already in flight. Without the
    // second half each pending request would sit out its own sixty-second
    // timeout for a process that is already gone.
    let tail = stderr_tail.lock().expect("stderr tail").clone();
    let message = if tail.trim().is_empty() {
        format!("{cause}, with nothing on stderr to say why.")
    } else {
        format!("{cause}. The last of its stderr:\n{}", tail.trim_end())
    };
    *death.lock().expect("death") = Some(message.clone());
    let waiting: Vec<_> = pending.lock().expect("pending").drain().collect();
    for (_, tx) in waiting {
        let _ = tx.send(Err(ToolError::Failed(message.clone())));
    }
}

#[allow(clippy::too_many_arguments)]
fn handle(
    message: Value,
    pending: &Pending,
    ready: &watch::Sender<Phase>,
    outgoing: &mpsc::UnboundedSender<Value>,
    indexing: &mut HashSet<String>,
    ever_indexed: &mut bool,
    health: &Health,
    speaks_status: &AtomicBool,
    published: &Published,
    publish_tick: &watch::Sender<u64>,
) {
    // A reply: `id` present, `method` absent.
    if message.get("method").is_none() {
        let Some(id) = message.get("id").and_then(Value::as_i64) else {
            return;
        };
        let Some(tx) = pending.lock().expect("pending").remove(&id) else {
            return;
        };
        let answer = match message.get("error") {
            // A JSON-RPC error is the server refusing a request it understood —
            // machinery, not world. `Failed`, and the server's own message,
            // which is usually specific ("content modified", "invalid offset").
            Some(error) => Err(ToolError::Failed(format!(
                "the language server refused the request: {}",
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or(&error.to_string())
            ))),
            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(answer);
        return;
    }

    let method = message["method"].as_str().unwrap_or_default();
    let id = message.get("id").cloned();

    match method {
        // The authoritative signal. Once it has been seen even once, the
        // progress heuristic is switched off — see `speaks_status`.
        "experimental/serverStatus" | "rust-analyzer/serverStatus" => {
            speaks_status.store(true, Ordering::Relaxed);
            let params = &message["params"];
            let quiescent = params["quiescent"].as_bool().unwrap_or(false);
            // Health is recorded whatever the readiness, because a server that
            // is *finished* and *broken* is the one case readiness alone gets
            // exactly backwards: it looks completely ready and answers nothing.
            let ok = params["health"].as_str().unwrap_or("ok") == "ok";
            *health.lock().expect("health") = (!ok).then(|| {
                let detail = params["message"]
                    .as_str()
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or("no detail given");
                format!(
                    "The language server reports its own health as \"{}\": {detail}. Answers \
                     may be missing or wrong — this usually means the project model could not \
                     be loaded.",
                    params["health"].as_str().unwrap_or("unknown")
                )
            });
            let _ = ready.send(if quiescent {
                Phase::Quiescent
            } else {
                Phase::Working
            });
        }
        // The push half of the protocol. Recorded rather than acted on: the
        // waiting is `Client::diagnostics`'s job, and doing it here would mean
        // the reader task blocking on a consumer.
        "textDocument/publishDiagnostics" => {
            let params = &message["params"];
            let Some(uri) = params["uri"].as_str() else {
                return;
            };
            let items = params["diagnostics"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            published
                .lock()
                .expect("published")
                .insert(published_key(uri), items);
            publish_tick.send_modify(|n| *n += 1);
        }
        "$/progress" => {
            let token = progress_token(&message["params"]["token"]);
            if !is_indexing_token(&token) {
                return;
            }
            match message["params"]["value"]["kind"].as_str() {
                Some("begin") => {
                    indexing.insert(token);
                    *ever_indexed = true;
                    // Sent even when already `Working`, so a `Quiet` that was
                    // only a gap between two phases is contradicted promptly and
                    // the settle wait in `wait_ready` sees it.
                    let _ = ready.send(Phase::Working);
                }
                Some("end") => {
                    indexing.remove(&token);
                    if indexing.is_empty() && *ever_indexed {
                        // `Quiet`, never `Quiescent`. This is the correction:
                        // "all tokens closed" is evidence, and the trace in the
                        // module doc shows it being true three times before the
                        // index exists.
                        let _ = ready.send(Phase::Quiet);
                    }
                }
                // "report" is progress within a token: it moves no state, but it
                // is still evidence that *something* was reported, which is what
                // `wait_ready`'s first phase is listening for.
                _ => {
                    if indexing.is_empty() && !*ever_indexed {
                        *ever_indexed = true;
                        indexing.insert(token);
                        let _ = ready.send(Phase::Working);
                    }
                }
            }
        }
        // Server-to-client requests. Each one must be answered or rust-analyzer
        // waits on it — `workspace/configuration` in particular is asked during
        // startup, and an unanswered one stalls the whole index behind it.
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "window/showMessageRequest" => {
            reply(outgoing, id, Value::Null);
        }
        "workspace/configuration" => {
            // One entry per requested item. The settings already went in
            // `initializationOptions`; rust-analyzer reads both, and answering
            // `null` per item means "use what you were given".
            let count = message["params"]["items"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(1);
            reply(outgoing, id, Value::Array(vec![Value::Null; count]));
        }
        _ => {
            // Notifications (no id) are ignored — `window/logMessage`,
            // `textDocument/publishDiagnostics` and the rest. An unknown
            // *request* gets a proper error rather than silence, so the server
            // stops waiting on it.
            if id.is_some() {
                let _ = outgoing.send(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("Emma does not implement {method}") },
                }));
            }
        }
    }
}

fn reply(outgoing: &mpsc::UnboundedSender<Value>, id: Option<Value>, result: Value) {
    let Some(id) = id else { return };
    let _ = outgoing.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
}

/// Progress tokens are `string | integer` in the protocol.
fn progress_token(token: &Value) -> String {
    match token {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Which progress tokens count as "the index is not ready".
///
/// rust-analyzer namespaces all of its own work under `rustAnalyzer/`. Matching
/// the prefix rather than an enumerated list is the deliberate choice: a new
/// phase in a future version is then counted by default, and the failure mode of
/// being wrong is a longer wait rather than a confident wrong answer.
fn is_indexing_token(token: &str) -> bool {
    token.starts_with("rustAnalyzer/")
}

fn hash_of(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

// endregion: The pumps

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The readiness rules that do not need a server at all. Everything that needs
// two ends of a pipe is in `tests/fake_server.rs`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The eight pieces of reader state one dispatch needs. Clippy would like a
    /// type alias; it would name a tuple that exists only so these tests can
    /// call one function, and would move the shape one indirection away from
    /// the tests that are about it.
    #[allow(clippy::type_complexity)]
    fn state() -> (
        Pending,
        watch::Sender<Phase>,
        watch::Receiver<Phase>,
        mpsc::UnboundedSender<Value>,
        mpsc::UnboundedReceiver<Value>,
        HashSet<String>,
        Health,
        Arc<AtomicBool>,
    ) {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = watch::channel(Phase::Handshaking);
        let (otx, orx) = mpsc::unbounded_channel();
        (
            pending,
            tx,
            rx,
            otx,
            orx,
            HashSet::new(),
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(false)),
        )
    }

    /// Runs one message through the reader's dispatcher. A macro rather than a
    /// function because the eight pieces of reader state are borrowed
    /// differently at each call site, and threading them by hand made every test
    /// below twice as long as the thing it was checking.
    macro_rules! feed {
        ($msg:expr, $p:expr, $tx:expr, $otx:expr, $idx:expr, $ever:expr, $h:expr, $st:expr) => {
            // The two publication channels are constructed here rather than
            // threaded through every call site: no test below cares what a
            // `$/progress` message did to the diagnostics map, and a ninth and
            // tenth parameter on twenty call sites would bury what each one is
            // actually checking. The publication path has its own tests, in
            // `tests/diagnostics.rs`, driven through the fake server.
            handle(
                $msg,
                &$p,
                &$tx,
                &$otx,
                &mut $idx,
                &mut $ever,
                &$h,
                &$st,
                &Arc::new(Mutex::new(HashMap::new())),
                &watch::channel(0u64).0,
            )
        };
    }

    fn progress(token: &str, kind: &str) -> Value {
        json!({ "method": "$/progress", "params": { "token": token, "value": { "kind": kind } } })
    }

    /// **The bug, as a unit test.** Traced against rust-analyzer 0.3.3008: the
    /// count of open indexing tokens returns to zero in the millisecond gaps
    /// between one phase ending and the next beginning — three times, before the
    /// index exists. The first version of this file sent `Ready` at each of
    /// them, asked, and got an empty list. So nothing may report readiness from
    /// progress alone: the most a closed token may produce is `Quiet`, which
    /// `wait_ready` then has to earn with the settle window.
    #[test]
    fn all_tokens_closed_is_evidence_and_never_an_answer() {
        let (p, tx, rx, otx, _orx, mut idx, h, st) = state();
        let mut ever = false;

        feed!(
            progress("rustAnalyzer/Fetching", "begin"),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert_eq!(*rx.borrow(), Phase::Working);
        feed!(
            progress("rustAnalyzer/Fetching", "end"),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert_eq!(
            *rx.borrow(),
            Phase::Quiet,
            "a gap between phases must never become Quiescent"
        );

        // …and the next phase starts, which is what makes the gap a gap.
        feed!(
            progress("rustAnalyzer/cachePriming", "begin"),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert_eq!(*rx.borrow(), Phase::Working);
        feed!(
            progress("rustAnalyzer/cachePriming", "end"),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert_eq!(*rx.borrow(), Phase::Quiet);

        // Only the server saying so produces the authoritative state.
        assert_ne!(*rx.borrow(), Phase::Quiescent);
    }

    /// Overlapping tokens still have to all close. rust-analyzer runs several at
    /// once, and a flag rather than a count would go quiet at the first `end`.
    #[test]
    fn readiness_waits_for_every_indexing_token_not_the_first() {
        let (p, tx, rx, otx, _orx, mut idx, h, st) = state();
        let mut ever = false;
        feed!(
            progress("rustAnalyzer/Indexing", "begin"),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        feed!(
            progress("rustAnalyzer/cachePriming", "begin"),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        feed!(
            progress("rustAnalyzer/Indexing", "end"),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert_eq!(*rx.borrow(), Phase::Working, "one of two tokens ended");
        feed!(
            progress("rustAnalyzer/cachePriming", "end"),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert_eq!(*rx.borrow(), Phase::Quiet);
    }

    /// The signal that is actually trustworthy, spelled as rust-analyzer sends
    /// it. Both spellings are accepted because the method was renamed once, and
    /// a client that knows only the new one silently falls back to the heuristic
    /// against an older server.
    #[test]
    fn server_status_is_authoritative_and_switches_the_heuristic_off() {
        for method in ["experimental/serverStatus", "rust-analyzer/serverStatus"] {
            let (p, tx, rx, otx, _orx, mut idx, h, st) = state();
            let mut ever = false;
            feed!(
                json!({ "method": method, "params": { "health": "ok", "quiescent": false } }),
                p,
                tx,
                otx,
                idx,
                ever,
                h,
                st
            );
            assert_eq!(*rx.borrow(), Phase::Working, "{method}");
            assert!(
                st.load(Ordering::Relaxed),
                "{method}: the fallback must be disabled"
            );
            assert!(
                h.lock().unwrap().is_none(),
                "{method}: healthy is not a note"
            );

            feed!(
                json!({ "method": method, "params": { "health": "ok", "quiescent": true } }),
                p,
                tx,
                otx,
                idx,
                ever,
                h,
                st
            );
            assert_eq!(*rx.borrow(), Phase::Quiescent, "{method}");
        }
    }

    /// The case readiness alone gets exactly backwards: a server that has
    /// finished everything and cannot load the project is perfectly quiescent
    /// and answers nothing. Without this note the empty answer reads as fact.
    #[test]
    fn a_quiescent_but_broken_server_says_so() {
        let (p, tx, rx, otx, _orx, mut idx, h, st) = state();
        let mut ever = false;
        feed!(
            json!({ "method": "experimental/serverStatus",
                    "params": { "health": "error", "quiescent": true,
                                "message": "cargo metadata failed" } }),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert_eq!(*rx.borrow(), Phase::Quiescent, "it really has finished");
        let note = h
            .lock()
            .unwrap()
            .clone()
            .expect("a broken server owes a note");
        assert!(note.contains("cargo metadata failed"), "{note}");
        assert!(note.contains("missing or wrong"), "{note}");
    }

    /// Progress that is not the server's own work must not be mistaken for it.
    #[test]
    fn only_the_servers_own_progress_counts() {
        let (p, tx, rx, otx, _orx, mut idx, h, st) = state();
        let mut ever = false;
        feed!(
            progress("someExtension/thing", "end"),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert_eq!(*rx.borrow(), Phase::Handshaking);
        assert!(!ever);
    }

    /// An `end` with no matching `begin` must not manufacture quiet, because
    /// nothing was ever observed being built.
    #[test]
    fn an_unmatched_end_does_not_manufacture_readiness() {
        let (p, tx, rx, otx, _orx, mut idx, h, st) = state();
        let mut ever = false;
        feed!(
            progress("rustAnalyzer/Indexing", "end"),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert_eq!(*rx.borrow(), Phase::Handshaking);
    }

    /// Every state that is not `Ready` owes the model a sentence, and the
    /// sentence has to say the thing that stops it misreading an empty list.
    #[test]
    fn every_unready_state_warns_that_empty_is_not_an_answer() {
        assert_eq!(Readiness::Ready.caveat(), None);
        for state in [
            Readiness::Handshaking,
            Readiness::Indexing,
            Readiness::Unknown,
        ] {
            let caveat = state.caveat().unwrap_or_else(|| panic!("{state:?}"));
            assert!(caveat.contains("empty"), "{state:?}: {caveat}");
            assert!(
                caveat.contains("not evidence"),
                "{state:?} must say an empty result proves nothing: {caveat}"
            );
        }
    }

    /// Requests are answered by id, and a reply for one must not resolve
    /// another. Obvious, and the kind of obvious that is wrong in half of the
    /// hand-rolled JSON-RPC clients ever written.
    #[test]
    fn replies_are_matched_by_id() {
        let (p, tx, _rx, otx, _orx, mut idx, h, st) = state();
        let mut ever = false;
        let (a_tx, mut a_rx) = oneshot::channel();
        let (b_tx, mut b_rx) = oneshot::channel();
        p.lock().unwrap().insert(1, a_tx);
        p.lock().unwrap().insert(2, b_tx);

        feed!(
            json!({ "id": 2, "result": "two" }),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert!(a_rx.try_recv().is_err(), "id 1 must still be waiting");
        assert_eq!(b_rx.try_recv().unwrap().unwrap(), json!("two"));

        // And a JSON-RPC error is machinery failing, not the world answering.
        feed!(
            json!({ "id": 1, "error": { "code": -32603, "message": "content modified" } }),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        let err = a_rx.try_recv().unwrap().unwrap_err();
        assert_eq!(err.kind(), "tool_failed");
        assert!(err.detail().contains("content modified"), "{err}");
    }

    /// The startup stall that costs a day to find in every hand-rolled client:
    /// rust-analyzer asks for configuration during initialisation and blocks
    /// until it is answered, so a client that ignores server-to-client requests
    /// never finishes indexing and every answer is empty.
    #[test]
    fn server_requests_are_answered_so_startup_does_not_stall() {
        let (p, tx, _rx, otx, mut orx, mut idx, h, st) = state();
        let mut ever = false;
        feed!(
            json!({ "id": 10, "method": "workspace/configuration", "params": { "items": [{}, {}] } }),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        let reply = orx.try_recv().expect("configuration must be answered");
        assert_eq!(reply["id"], 10);
        assert_eq!(reply["result"], json!([null, null]));

        feed!(
            json!({ "id": 11, "method": "window/workDoneProgress/create", "params": { "token": "t" } }),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert_eq!(orx.try_recv().expect("must be answered")["id"], 11);

        // A request nobody implements gets an error rather than silence, so the
        // server stops waiting on it.
        feed!(
            json!({ "id": 12, "method": "workspace/applyEdit", "params": {} }),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert_eq!(
            orx.try_recv().expect("unknown requests need an answer too")["error"]["code"],
            -32601
        );

        // A *notification* must not be answered — replying to something with no
        // id is a protocol violation and some servers close the connection.
        feed!(
            json!({ "method": "window/logMessage", "params": { "message": "hi" } }),
            p,
            tx,
            otx,
            idx,
            ever,
            h,
            st
        );
        assert!(orx.try_recv().is_err(), "a notification was replied to");
    }

    /// The switches that make `read_only: true` true rather than convenient. If
    /// any of these comes back on, the tool starts running the analysed
    /// project's build scripts or writing to its `target/`, and the approval
    /// gate is letting it through without asking anybody.
    #[test]
    fn the_dangerous_switches_are_off() {
        let options: Value = serde_json::from_str(INIT_OPTIONS).expect("valid JSON");
        assert_eq!(options["check"]["enable"], json!(false));
        assert_eq!(options["checkOnSave"], json!(false));
        assert_eq!(options["cargo"]["buildScripts"]["enable"], json!(false));
        assert_eq!(options["procMacro"]["enable"], json!(false));
        assert_eq!(options["cargo"]["extraArgs"], json!(["--offline"]));
    }
}

// endregion: Tests
