//! The bridge between the Code page's editor and the language servers the
//! model's tools already run.
//!
//! # The one law this module exists to obey
//!
//! **The input thread never waits on a language server.** It runs under the
//! frame lock, and a lock held across a request to rust-analyzer is a frozen
//! terminal — the hazard the Code page's own git calls were moved off the
//! input thread for. So nothing here is called from a key handler. A key posts
//! a [`Request`] with [`Handle::post`], which is a `try_send` on a bounded
//! channel and cannot block or fail slowly: a full queue drops the request and
//! says so in its return value. The task below owns every `.await` in the
//! feature, and answers come back through a [`Sink`] that takes the frame lock
//! briefly, writes view state and repaints.
//!
//! The consequence is the honest one: a slow or dead server costs the editor
//! its decorations and never a keystroke. The page draws whatever it was last
//! told, and if it was told nothing it draws nothing — which is why every
//! refusal here is a [`LspStatus`] sentence rather than silence.
//!
//! # One pool, two consumers
//!
//! The `Pool` this task holds is the same `Arc<Pool>` `main` gives the five LSP
//! tools, and that is a decision with evidence rather than a convenience.
//! `emma_tools_lsp`'s `Client` correlates requests by an `AtomicI64` id against
//! a shared pending map, publishes diagnostics into a `Mutex<HashMap>` behind a
//! `watch` counter every reader clones its own receiver from, and takes `&self`
//! everywhere. There is no single-consumer assumption in it, so a second
//! consumer needed no surgery there — only the four additive methods
//! `did_save`, `did_close`, `published_for` and `publish_tick`, which the
//! multi-language port landed.
//!
//! What sharing buys is not only the ~1 GB of RSS a second rust-analyzer would
//! cost. The editor's `didChange` keeps the server's view of the file current,
//! so the model's next `GoToDefinition` is answered against what the person
//! actually has on screen rather than against what was last written to disk.
//!
//! # The lifecycle
//!
//! Open sends `didOpen` with the **buffer**, not the file: what the page shows
//! is what the server is told about, which is the whole difference between
//! decorations as you type and decorations after you save. Every edit is
//! debounced by [`DEBOUNCE`] of quiet and then sent as `didChange`; the clock
//! lives here, in the task, never in the input thread. Save flushes the pending
//! buffer and then sends `didSave`. Closing the file or the page sends
//! `didClose` and drops what the server published for it. Diagnostics are not
//! polled: one watcher task per open file waits on the client's publish tick
//! and pushes each new set at the view.
//!
//! # Two coordinate systems, converted exactly once
//!
//! LSP counts columns in **UTF-16 code units** and [`super::code::OpenFile`]
//! indexes **chars**. One accented character on a line is the whole difference,
//! and a conversion done twice or in the paint would put every decoration after
//! it under the wrong character. It happens here, in [`position_of`] on the way
//! out and [`char_column`] on the way back, against the buffer the server was
//! told about.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use emma_tool_api::ToolError;
use emma_tools_lsp::client::Client;
use emma_tools_lsp::{doc, lang, Pool};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::code::{Candidate, DefTarget, Diag, LspStatus, LspUpdate, Place, Severity};

// region: The seam
// ---------------------------------------------------------------------------
// The seam: what the input thread is allowed to touch
// ---------------------------------------------------------------------------

/// How long a burst of typing must stop before the buffer is sent.
///
/// 300ms is the usual editor figure, and the argument for it here is cost:
/// every `didChange` makes the server re-parse the file, so a notification per
/// keystroke would leave it permanently one edit behind while burning a core.
/// The clock is in the task, so a debounce that is too long costs late
/// diagnostics and never a slow key.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// How long a hover or definition request may take before the page is told it
/// did not arrive.
///
/// Shorter than the crate's own `REQUEST_TIMEOUT`, deliberately: a model can
/// afford to wait a minute for an answer and a person pressing `F5` cannot. The
/// wait covers indexing too, which is the usual reason for it — measured at
/// 4 to 8 seconds on this repository from a cold pool, which is why it is ten
/// and not five.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(10);

/// How many requests may be in flight before new ones are dropped.
///
/// Small on purpose. The queue holds keystrokes' worth of work, and a backlog
/// of stale buffers is worth less than the newest one; dropping is the correct
/// behaviour and [`Handle::post`] reports it rather than hiding it.
pub const QUEUE: usize = 64;

/// What the page asks the servers for.
///
/// Every variant that needs one carries the **buffer**, because the buffer is
/// the thing the server has not got. Carrying a path and letting the task read
/// the file would answer about what is on disk, which is precisely the file the
/// person is in the middle of changing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Open {
        rel: String,
        text: String,
    },
    Change {
        rel: String,
        text: String,
    },
    Save {
        rel: String,
        text: String,
    },
    Close {
        rel: String,
    },
    Hover {
        rel: String,
        text: String,
        line: usize,
        col: usize,
    },
    Definition {
        rel: String,
        text: String,
        line: usize,
        col: usize,
    },
    /// Ask what could be typed here.
    ///
    /// Carries the same buffer every other question does, for the same reason:
    /// a completion computed against the text the server saw three hundred
    /// milliseconds ago offers members of a type the person has just finished
    /// changing.
    Completion {
        rel: String,
        text: String,
        line: usize,
        col: usize,
    },
    /// Ask where the symbol under the cursor is used.
    References {
        rel: String,
        text: String,
        line: usize,
        col: usize,
    },
    /// Ask what this file contains.
    Symbols {
        rel: String,
        text: String,
    },
    /// Ask what every run of characters in the file is, for colour.
    Highlight {
        rel: String,
        text: String,
    },
    /// Ask which argument the cursor is in.
    Signature {
        rel: String,
        text: String,
        line: usize,
        col: usize,
    },
}

/// The input thread's half. Cloneable, and the only thing it may hold.
#[derive(Debug, Clone)]
pub struct Handle {
    tx: mpsc::Sender<Request>,
}

impl Handle {
    /// Post a request without waiting for anything.
    ///
    /// **`try_send` and nothing else.** No `.await`, no blocking send, no
    /// timeout: this is called with the frame lock held, and every other way of
    /// putting a message on a channel can park the thread that paints. `false`
    /// means the queue was full or the task is gone, which is a fact about
    /// decorations and not about the keystroke.
    pub fn post(&self, request: Request) -> bool {
        self.tx.try_send(request).is_ok()
    }
}

/// Where answers go. The frame's implementation takes the paint lock, writes,
/// and repaints; a test's collects into a `Vec`.
pub type Sink = Arc<dyn Fn(LspUpdate) + Send + Sync>;

/// A handle and the receiver its task will drain.
pub fn channel() -> (Handle, mpsc::Receiver<Request>) {
    let (tx, rx) = mpsc::channel(QUEUE);
    (Handle { tx }, rx)
}

// endregion: The seam

// region: The task
// ---------------------------------------------------------------------------
// The task: every await in the feature is inside this function
// ---------------------------------------------------------------------------

/// What the task knows about the file that is open.
struct Open {
    rel: String,
    path: PathBuf,
    client: Arc<Client>,
    /// The buffer as the server last saw it, shared with the diagnostics
    /// watcher so it converts UTF-16 columns against the right text.
    text: Arc<Mutex<String>>,
    watcher: tokio::task::JoinHandle<()>,
}

impl Drop for Open {
    fn drop(&mut self) {
        self.watcher.abort();
    }
}

/// Own every language-server conversation the Code page has, until the channel
/// closes.
///
/// Spawned once, from `main`, with the same pool the tools hold.
pub async fn run(root: PathBuf, pool: Arc<Pool>, mut rx: mpsc::Receiver<Request>, sink: Sink) {
    let mut open: Option<Open> = None;
    // The debounce clock. `Some` means a buffer is waiting for the typing to
    // stop; the deadline is pushed forward by every further edit.
    let mut pending: Option<(String, String)> = None;
    let mut deadline: Option<tokio::time::Instant> = None;

    loop {
        let tick = deadline;
        let request = tokio::select! {
            request = rx.recv() => match request {
                Some(r) => Some(r),
                // The page is gone. Tell the server, so a stale document does
                // not sit in an index the model will later ask about.
                None => {
                    if let Some(o) = &open {
                        o.client.did_close(&o.path);
                    }
                    return;
                }
            },
            _ = async {
                match tick {
                    Some(at) => tokio::time::sleep_until(at).await,
                    // Nothing pending: park forever, and let the recv arm win.
                    None => std::future::pending::<()>().await,
                }
            } => None,
        };

        let Some(request) = request else {
            // The debounce expired: send the newest buffer, once, and ask what
            // it is made of. **The colour question belongs here rather than on
            // the keystroke**: it needs the server to have the current text, so
            // asking it earlier meant flushing the debounce, which is how a
            // three-hundred-millisecond debounce came to send one `didChange`
            // and one full token request per character typed.
            deadline = None;
            if let (Some((rel, text)), Some(o)) = (pending.take(), open.as_ref()) {
                if rel == o.rel {
                    *o.text.lock().expect("buffer") = text.clone();
                    o.client.sync_document(&o.path, &text);
                    ask_tokens(o, &sink, &text);
                }
            }
            continue;
        };

        match request {
            Request::Open { rel, text } => {
                if let Some(previous) = open.take() {
                    previous.client.did_close(&previous.path);
                }
                pending = None;
                deadline = None;
                open = start(&root, &pool, &rel, &text, &sink).await;
            }
            Request::Change { rel, text } => {
                // The whole debounce: remember the newest buffer, push the
                // deadline out, and send nothing yet.
                pending = Some((rel, text));
                deadline = Some(tokio::time::Instant::now() + DEBOUNCE);
            }
            Request::Save { rel, text } => {
                // A save flushes the debounce rather than racing it: the server
                // must not be told the file was written and only then told what
                // was in it. `ready` is that flush.
                if let Some(o) = ready(&open, &rel, &text, &mut pending, &mut deadline) {
                    o.client.did_save(&o.path);
                }
            }
            Request::Close { rel } => {
                if open.as_ref().is_some_and(|o| o.rel == rel) {
                    if let Some(o) = open.take() {
                        o.client.did_close(&o.path);
                    }
                    pending = None;
                    deadline = None;
                    sink(LspUpdate::Status(LspStatus::Idle));
                }
            }
            Request::Hover {
                rel,
                text,
                line,
                col,
            } => {
                if let Some(o) = ready(&open, &rel, &text, &mut pending, &mut deadline) {
                    ask(
                        o,
                        &sink,
                        "textDocument/hover",
                        line,
                        col,
                        &text,
                        Answer::Hover,
                    );
                }
            }
            Request::Completion {
                rel,
                text,
                line,
                col,
            } => {
                if let Some(o) = ready(&open, &rel, &text, &mut pending, &mut deadline) {
                    let answer = Answer::Completion(text.clone());
                    ask(
                        o,
                        &sink,
                        "textDocument/completion",
                        line,
                        col,
                        &text,
                        answer,
                    );
                }
            }
            Request::References {
                rel,
                text,
                line,
                col,
            } => {
                if let Some(o) = ready(&open, &rel, &text, &mut pending, &mut deadline) {
                    // Not `ask`, which reads its answer back as one of the
                    // `Answer` shapes and sends a bare position:
                    // `ReferenceParams` has a required `context`, and a server
                    // within its rights to reject the request without one would
                    // fail here. It is still `spawn_request`, so a rejection is
                    // a sentence rather than an empty list.
                    let mut params = position_params(o, &text, line, col);
                    // The declaration is included because a reader asking
                    // "where is this used" is usually navigating, and the
                    // definition is one of the places to go.
                    params["context"] = json!({ "includeDeclaration": true });
                    let about = word_at(&text, line, col);
                    let root = root.clone();
                    let path = rel.clone();
                    spawn_request(o, &sink, "textDocument/references", params, move |value| {
                        LspUpdate::Places {
                            about: format!("references to {about}"),
                            rows: reference_places(value, &root),
                            path,
                        }
                    });
                }
            }
            Request::Symbols { rel, text } => {
                if let Some(o) = ready(&open, &rel, &text, &mut pending, &mut deadline) {
                    let params = json!({ "textDocument": { "uri": doc::to_uri(&o.path) } });
                    let path = rel.clone();
                    spawn_request(
                        o,
                        &sink,
                        "textDocument/documentSymbol",
                        params,
                        move |value| {
                            let rows = symbol_places(value, &path);
                            LspUpdate::Places {
                                about: format!("symbols in {path}"),
                                rows,
                                path,
                            }
                        },
                    );
                }
            }
            Request::Highlight { rel, text } => {
                // **Only when nothing is pending.** A buffer waiting out the
                // debounce will be sent, and answered about, the moment it
                // expires; asking now would mean flushing it, and flushing it
                // on every keystroke is the debounce not existing. The page
                // posts this on open and on every change, so the open case
                // colours immediately and the typing case colours when the
                // typing stops.
                if pending.is_some() {
                    continue;
                }
                if let Some(o) = open.as_ref().filter(|o| o.rel == rel) {
                    ask_tokens(o, &sink, &text);
                }
            }
            Request::Signature {
                rel,
                text,
                line,
                col,
            } => {
                if let Some(o) = ready(&open, &rel, &text, &mut pending, &mut deadline) {
                    ask(
                        o,
                        &sink,
                        "textDocument/signatureHelp",
                        line,
                        col,
                        &text,
                        Answer::Signature,
                    );
                }
            }
            Request::Definition {
                rel,
                text,
                line,
                col,
            } => {
                if let Some(o) = ready(&open, &rel, &text, &mut pending, &mut deadline) {
                    let answer = Answer::Definition(root.clone(), text.clone());
                    ask(
                        o,
                        &sink,
                        "textDocument/definition",
                        line,
                        col,
                        &text,
                        answer,
                    );
                }
            }
        }
    }
}

/// One protocol completion, narrowed to what the page draws and inserts.
///
/// The conversion that matters is the range: the wire counts UTF-16 code units
/// on a line and the page counts characters, and they differ on any line with a
/// character outside the basic plane. Doing it here keeps the rule this crate
/// already has, which is that a column crosses the boundary exactly once.
fn candidate(c: &emma_tools_lsp::render::Completion, buffer: &str) -> Candidate {
    let replace = c.replace.and_then(|(start, end)| {
        // Only a range on one line can be a word under the cursor. A
        // multi-line edit is a refactor, not a completion, and the page
        // declines it rather than applying half.
        if start.line != end.line {
            return None;
        }
        let line = buffer.lines().nth(start.line as usize)?;
        Some((
            char_column_in(line, start.character),
            char_column_in(line, end.character),
        ))
    });
    Candidate {
        label: c.label.clone(),
        filter: c.filter.clone(),
        // A snippet the page cannot expand is offered as its label rather than
        // its placeholders: `push(${1:value})` typed literally into a file is
        // worse than a plain `push`.
        insert: if c.snippet {
            c.filter.clone()
        } else {
            c.insert.clone()
        },
        replace,
        kind: c.kind,
        detail: c.detail.clone(),
    }
}

/// The active signature as one line, with the current argument marked.
///
/// One line because it shares the note row: two floating boxes over three lines
/// of code is a page nobody can read. The marker is square brackets rather than
/// a colour, so it survives the ASCII skin and a terminal with no colour at
/// all.
///
/// **The parameters are matched left to right, not by a bare `find`, and that
/// is a defect this had.** `find` returns the *first* textual occurrence, so
/// `fn f(fun: u8, un: u8)` with the second argument active bracketed the `un`
/// inside `fun`: a marker on the wrong argument, which the comment below
/// already called worse than none while the code produced one. Consuming the
/// label in parameter order fixes every case where an earlier parameter's text
/// contains a later one, which is the whole family.
///
/// **It is still a search, and the wire did not need one.** A server may send
/// each parameter as an exact `[start, end]` pair of UTF-16 offsets into the
/// label, and `parse_signatures` in `tools/lsp/src/render.rs` resolves those to
/// strings and throws the offsets away. If that ever returns the pairs, use
/// them and delete this scan: two parameters with identical text — `f(u8, u8)`
/// — cannot be told apart by any amount of searching.
fn signature_line(value: &Value) -> Option<String> {
    let (sigs, active) = emma_tools_lsp::render::parse_signatures(value)?;
    let sig = sigs.get(active)?;
    let Some(at) = sig.active_parameter else {
        return Some(sig.label.clone());
    };
    let Some(param) = sig.parameters.get(at) else {
        return Some(sig.label.clone());
    };
    // Walk the earlier parameters first, so the search for this one starts past
    // where they sit. A parameter the label does not contain is skipped rather
    // than fatal: it costs this scan its head start and nothing else.
    let mut from = 0;
    for earlier in sig.parameters.iter().take(at) {
        if let Some(i) = sig.label[from..].find(earlier.as_str()) {
            from += i + earlier.len();
        }
    }
    // Falling back to the bare label rather than guessing: a marker on the
    // wrong argument is worse than none.
    match sig.label[from..].find(param.as_str()) {
        Some(i) => {
            let i = from + i;
            Some(format!(
                "{}[{}]{}",
                &sig.label[..i],
                param,
                &sig.label[i + param.len()..]
            ))
        }
        None => Some(sig.label.clone()),
    }
}

/// The two things every question about the open file must do before it is
/// asked, in one place because they are one rule and it was written out seven
/// times.
///
/// **The guard**: the request names a file, and by the time the task reads it
/// the person may have opened another. Answering about a buffer that is no
/// longer on screen is the staleness `apply_lsp` then has to throw away.
///
/// **The flush**: a question needs the server to have the buffer, so asking one
/// cancels the debounce and sends it. Otherwise `F5` answers about the text as
/// it was three hundred milliseconds ago — which is the version the person has
/// just changed.
///
/// Seven arms each ended in this pair, which is seven chances to add an eighth
/// request that forgets the flush and quietly answers about the wrong text.
/// `None` means the request was about a file that is not open, and the caller
/// has nothing to do.
fn ready<'a>(
    open: &'a Option<Open>,
    rel: &str,
    text: &str,
    pending: &mut Option<(String, String)>,
    deadline: &mut Option<tokio::time::Instant>,
) -> Option<&'a Open> {
    let o = open.as_ref().filter(|o| o.rel == rel)?;
    *pending = None;
    *deadline = None;
    *o.text.lock().expect("buffer") = text.to_string();
    o.client.sync_document(&o.path, text);
    Some(o)
}

/// The sentence for a request that did not come back with an answer.
///
/// **This used to be written out per call site, and one call site did not have
/// it**, so a references or a symbols question that timed out or was refused
/// drew nothing at all -- the silence this module's header names as the failure
/// it exists to prevent. It is now reached only through [`spawn_request`],
/// which is the shape that stops a future request being added without it.
fn timed_out() -> String {
    format!(
        "the language server did not answer within {}s; it is probably still indexing",
        ASK_TIMEOUT.as_secs()
    )
}

/// Ask what every run of characters in the file is, for colour.
///
/// **Only ever called with a buffer the server already has**, which is the
/// whole of the debounce's correctness: the answer's positions index the
/// version the server holds, so asking about a buffer it has not been sent
/// produces colour half a line out. `parse_tokens` drops a token past the end
/// of the buffer it is given, which contains the damage but does not prevent
/// it.
///
/// **Not [`spawn_request`], and the difference is the failure.** Every other
/// question here is a key somebody pressed and is owed a sentence for; colour
/// is asked for by the page itself, so a server that will not answer costs the
/// decorations and must not put a note over a row the person is reading. The
/// legend check has the same shape: no legend means the numbers are unnamed,
/// and there is nothing to say about that either.
fn ask_tokens(o: &Open, sink: &Sink, text: &str) {
    let client = o.client.clone();
    let sink = sink.clone();
    let uri = doc::to_uri(&o.path);
    let path = o.rel.clone();
    let buffer = text.to_string();
    tokio::spawn(async move {
        let legend = client.capabilities().semantic_tokens;
        if legend.is_empty() {
            // No legend means the numbers in the answer are unnamed, and
            // colouring by an order this client guessed is worse than drawing
            // plain text.
            return;
        }
        let params = json!({ "textDocument": { "uri": uri } });
        let result = tokio::time::timeout(
            ASK_TIMEOUT,
            client.request("textDocument/semanticTokens/full", params),
        )
        .await;
        if let Ok(Ok(a)) = result {
            sink(LspUpdate::Tokens {
                path,
                items: emma_tools_lsp::render::parse_tokens(&a.value, &legend, &buffer),
            });
        }
    });
}

/// Which shape the answer is read back in.
enum Answer {
    Hover,
    /// Carries the buffer, because what the popup filters against is the word
    /// already typed, and that word is in the buffer rather than on the wire.
    Completion(String),
    Signature,
    /// Carries the root, because containment is decided against it, and the
    /// buffer, because a definition landing in the file that is already open
    /// can have its column converted here rather than by the shell.
    Definition(PathBuf, String),
}

/// Fire one request off into its own task, so the loop keeps draining keys —
/// and give it the one answer every question owes its asker.
///
/// **The two failure arms are the reason this exists as a function.** They had
/// been written out once per request, and the two requests that built their own
/// task matched only `Ok(Ok(_))`: a refusal or a timeout drew nothing, which
/// reads as "there are none" rather than as "it would not say". A caller now
/// supplies only `read`, the shape it wants the *successful* answer in, and
/// cannot express the version that swallows the other two.
fn spawn_request(
    o: &Open,
    sink: &Sink,
    method: &'static str,
    params: Value,
    read: impl FnOnce(&Value) -> LspUpdate + Send + 'static,
) {
    let client = o.client.clone();
    let sink = sink.clone();
    let path = o.rel.clone();
    tokio::spawn(async move {
        let result = tokio::time::timeout(ASK_TIMEOUT, client.request(method, params)).await;
        let update = match result {
            Ok(Ok(a)) => read(&a.value),
            Ok(Err(e)) => LspUpdate::Note {
                path,
                text: e.detail().to_string(),
            },
            // Named, not swallowed: the person pressed a key and is owed an
            // answer, and "it did not come back" is one.
            Err(_) => LspUpdate::Note {
                path,
                text: timed_out(),
            },
        };
        sink(update);
    });
}

/// The `TextDocumentPositionParams` every question at the cursor sends.
///
/// One function because the char-to-UTF-16 conversion is in it, and this crate's
/// rule is that a column crosses that boundary exactly once.
fn position_params(o: &Open, text: &str, line: usize, col: usize) -> Value {
    let position = position_of(text, line, col);
    json!({
        "textDocument": { "uri": doc::to_uri(&o.path) },
        "position": { "line": position.line, "character": position.character },
    })
}

/// One question at the cursor, read back in the shape its [`Answer`] names.
fn ask(
    o: &Open,
    sink: &Sink,
    method: &'static str,
    line: usize,
    col: usize,
    text: &str,
    answer: Answer,
) {
    let rel = o.rel.clone();
    let params = position_params(o, text, line, col);
    spawn_request(o, sink, method, params, move |value| match answer {
        Answer::Hover => LspUpdate::Hover {
            path: rel,
            lines: hover_lines(value),
        },
        Answer::Completion(buffer) => {
            let (items, incomplete) = emma_tools_lsp::render::parse_completions(value);
            LspUpdate::Completions {
                path: rel,
                // The wire's ranges are UTF-16 columns on a line; the page
                // counts characters. Converted here, where the buffer that
                // decides the answer is in hand, which is the same rule the
                // definition arm follows.
                items: items.iter().map(|c| candidate(c, &buffer)).collect(),
                incomplete,
                origin: (line, col),
            }
        }
        Answer::Signature => LspUpdate::Signature {
            path: rel,
            line: signature_line(value),
        },
        Answer::Definition(root, buffer) => {
            let mut target = definition_target(value, &root);
            // A jump inside the file that is already open can be converted
            // here, because the buffer for it is in hand. A jump into another
            // file cannot: nothing has read it.
            if let DefTarget::Inside {
                rel: to, line, col, ..
            } = &target
            {
                if *to == rel {
                    target = DefTarget::Inside {
                        rel: to.clone(),
                        line: *line,
                        col: char_column(&buffer, *line, *col as u32),
                    };
                }
            }
            LspUpdate::Definition { path: rel, target }
        }
    });
}

/// Resolve the language, start or find the server, tell it about the file, and
/// start watching for what it publishes.
///
/// Every exit reports an [`LspStatus`], which is the honesty surface: the page
/// never has to guess why it has no decorations.
async fn start(root: &Path, pool: &Arc<Pool>, rel: &str, text: &str, sink: &Sink) -> Option<Open> {
    let path = super::code_git::abs(root, rel);
    // **The one table.** `emma_tools_lsp::lang` decides what language a file is
    // for the model's tools; a second answer here would be the "one input
    // shape, two answers" defect this codebase has already paid for, and the
    // two would disagree the first time a language was added to either.
    let Some(language) = lang::for_path(root, &path) else {
        sink(LspUpdate::Status(LspStatus::Unsupported(shown(rel))));
        return None;
    };
    if !pool.is_enabled(language) {
        sink(LspUpdate::Status(LspStatus::Disabled(
            language.label.to_string(),
        )));
        return None;
    }
    sink(LspUpdate::Status(LspStatus::Starting(
        language.label.to_string(),
    )));
    let client = match pool.client(root, language).await {
        Ok(client) => client,
        Err(ToolError::Unavailable(detail)) => {
            sink(LspUpdate::Status(LspStatus::Absent {
                label: language.label.to_string(),
                detail,
            }));
            return None;
        }
        Err(e) => {
            sink(LspUpdate::Status(LspStatus::Failed(e.detail().to_string())));
            return None;
        }
    };
    // `running` rather than `found`: a handshake completed, which is the
    // stronger of the two claims the Settings card distinguishes.
    sink(LspUpdate::Status(LspStatus::Running(entry_name(&client))));
    // What this server says opens a list, sent once. Read from the handshake
    // rather than guessed: rust-analyzer names `.`, `:`, `'` and `(`, and a
    // client with its own hard-coded set is wrong for every language whose
    // server disagrees.
    let caps = client.capabilities();
    sink(LspUpdate::Triggers {
        completion: caps.completion_triggers,
        signature: caps.signature_triggers,
    });

    let buffer = Arc::new(Mutex::new(text.to_string()));
    client.sync_document(&path, text);
    let watcher = tokio::spawn(watch_diagnostics(
        client.clone(),
        doc::to_uri(&path),
        rel.to_string(),
        buffer.clone(),
        sink.clone(),
    ));
    Some(Open {
        rel: rel.to_string(),
        path,
        client,
        text: buffer,
        watcher,
    })
}

/// Push every set of diagnostics the server publishes for one file at the view,
/// until the file closes.
///
/// A subscription, not a poll: the client ticks a `watch` on every publish and
/// this waits on it. The first look happens *before* the first wait, because a
/// server that published on `didOpen` may well have done it already, and this
/// task is only spawned after `sync_document` has asked.
///
/// **That ordering is belt-and-braces today and the braces are in another
/// crate**, which is worth saying rather than leaving somebody to discover.
/// `Client::publish_tick` clones a receiver the client itself never advances,
/// so every clone starts at version zero and `changed()` returns immediately
/// for anything already published - deleting the look below leaves the suite
/// green, and the report says so. It stays because the property it leans on is
/// somebody else's to change: one `borrow_and_update` on that stored receiver
/// and a watcher that waited first would sit out the whole session having
/// missed the only tick that mattered.
async fn watch_diagnostics(
    client: Arc<Client>,
    uri: String,
    rel: String,
    text: Arc<Mutex<String>>,
    sink: Sink,
) {
    let mut tick = client.publish_tick();
    let mut last: Option<Vec<Value>> = None;
    loop {
        if let Some(items) = client.published_for(&uri) {
            if last.as_ref() != Some(&items) {
                let buffer = text.lock().expect("buffer").clone();
                sink(LspUpdate::Diagnostics {
                    path: rel.clone(),
                    items: items.iter().map(|d| convert(d, &buffer)).collect(),
                });
                last = Some(items);
            }
        }
        if tick.changed().await.is_err() {
            return;
        }
    }
}

// endregion: The task

// region: Reading what the server said
// ---------------------------------------------------------------------------
// Reading what the server said
//
// Pure, and tested as such. Every function here turns one LSP shape into one of
// the page's.
// ---------------------------------------------------------------------------

/// A (line, char) cursor as an LSP position.
pub fn position_of(text: &str, line: usize, col: usize) -> doc::Position {
    let source = text.lines().nth(line).unwrap_or_default();
    let byte: usize = source.chars().take(col).map(char::len_utf8).sum();
    doc::Position {
        line: line as u32,
        character: doc::utf16_column(source, byte),
    }
}

/// The inverse of [`position_of`], on a line that is already in hand.
///
/// **The one implementation, and it had three.** The module header claims a
/// column crosses the UTF-16 boundary exactly once; it was in fact written out
/// here, in `candidate`'s range conversion, and again in the shell's cross-file
/// jump — three chances for one of them to stop agreeing about a line with an
/// astral character on it. `pub` for the shell, which converts against a buffer
/// only it has read.
pub fn char_column_in(line: &str, utf16: u32) -> usize {
    let byte = doc::byte_offset(line, utf16);
    line[..byte.min(line.len())].chars().count()
}

/// The same, indexing `text` by line first. A line that is not there is empty,
/// which yields column zero rather than a panic.
fn char_column(text: &str, line: usize, utf16: u32) -> usize {
    char_column_in(text.lines().nth(line).unwrap_or_default(), utf16)
}

/// One LSP diagnostic in the page's coordinates.
pub fn convert(raw: &Value, text: &str) -> Diag {
    let range = &raw["range"];
    let line = range["start"]["line"].as_u64().unwrap_or(0) as usize;
    let end_line = range["end"]["line"].as_u64().unwrap_or(line as u64) as usize;
    let start_col = char_column(
        text,
        line,
        range["start"]["character"].as_u64().unwrap_or(0) as u32,
    );
    let end_col = char_column(
        text,
        end_line,
        range["end"]["character"].as_u64().unwrap_or(0) as u32,
    );
    Diag {
        line,
        end_line,
        start_col,
        end_col,
        // Absent severity is an error by LSP's own rule, and it is the safe way
        // round: a real error shown as a hint is the failure that matters.
        severity: Severity::from_lsp(raw["severity"].as_i64().unwrap_or(1)),
        message: raw["message"].as_str().unwrap_or_default().to_string(),
    }
}

/// Hover contents, in all three shapes the protocol allows.
///
/// `None` means the server answered with nothing, which the page says out loud
/// rather than showing as an empty box.
pub fn hover_lines(result: &Value) -> Option<Vec<String>> {
    let contents = result.get("contents")?;
    let text = match contents {
        Value::String(s) => s.clone(),
        Value::Object(o) => o
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(s) => s.clone(),
                other => other["value"].as_str().unwrap_or_default().to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    // The fences go, the text stays. A code fence in a bounded popup is three
    // characters of decoration on a row that has no room for them, and this
    // repository's markdown renderer is built for the streaming transcript
    // rather than for an overlay.
    let lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .map(|l| l.trim_end().to_string())
        .collect();
    let trimmed: Vec<String> = lines.iter().skip_while(|l| l.is_empty()).cloned().collect();
    if trimmed.iter().all(String::is_empty) {
        return None;
    }
    Some(trimmed)
}

/// The identifier under the cursor, for the places panel's header.
///
/// Best effort and labelled as such: it is a caption, never a lookup. The
/// server is asked about a position, not about this word, so a header that is
/// slightly wrong is a cosmetic defect while a *lookup* keyed on a guessed word
/// would be an answer about the wrong symbol.
fn word_at(text: &str, line: usize, col: usize) -> String {
    let Some(source) = text.lines().nth(line) else {
        return String::new();
    };
    let chars: Vec<char> = source.chars().collect();
    let ident = |c: &char| c.is_alphanumeric() || *c == '_';
    let mut start = col.min(chars.len());
    while start > 0 && ident(&chars[start - 1]) {
        start -= 1;
    }
    let mut end = col.min(chars.len());
    while end < chars.len() && ident(&chars[end]) {
        end += 1;
    }
    chars[start..end].iter().collect()
}

/// Where a wire `Range` begins, as (line, UTF-16 column).
///
/// Both producers of a [`Place`] read this out of a different shape of answer
/// and got the same six lines twice; a missing range is position zero for both,
/// which is the only defensible guess for a row whose whole job is to be opened.
fn range_start(range: Option<&Value>) -> (usize, usize) {
    let at = |k: &str| {
        range
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get(k))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize
    };
    (at("line"), at("character"))
}

/// Every reference the server named, as rows the panel can open.
///
/// **A hit outside the root is dropped rather than listed**, which is the same
/// containment law the definition jump follows: this page does not open files
/// outside the repository, so offering a row that refuses when pressed would be
/// a drawn control that does nothing. The count of what was dropped is not
/// shown, because a reference in a registry checkout is not a fact about this
/// repository.
fn reference_places(result: &Value, root: &Path) -> Vec<Place> {
    let items = match result {
        Value::Array(items) => items.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    };
    let mut out = Vec::new();
    for item in items {
        let Some(uri) = item.get("uri").and_then(Value::as_str) else {
            continue;
        };
        let Some(path) = doc::from_uri(uri) else {
            continue;
        };
        // `relative_to`, never `strip_prefix`: on Windows the root is
        // canonicalised to `\\?\E:\...` and the URI comes back as `E:\...`,
        // so the raw comparison drops every hit in the repository. The
        // definition jump paid for this one already.
        let Some(rel) = relative_to(root, &path) else {
            continue;
        };
        let (line, col) = range_start(item.get("range"));
        out.push(Place {
            // The path and line, because the panel is a list of *places* and
            // the line's own text is not available here: this answer names
            // files the page has not read.
            label: format!("{rel}:{}", line + 1),
            rel,
            line,
            col,
        });
    }
    out
}

/// This file's symbols, flattened depth-first with their nesting shown.
///
/// Both wire shapes, for the reason the tool's renderer handles both: a
/// `DocumentSymbol` is a tree and a `SymbolInformation` is a flat list with a
/// location, and a server that ignores the hierarchical declaration would
/// otherwise render as an empty file.
fn symbol_places(result: &Value, rel: &str) -> Vec<Place> {
    fn walk(items: &Value, depth: usize, rel: &str, out: &mut Vec<Place>) {
        let Some(items) = items.as_array() else {
            return;
        };
        for item in items {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>");
            // `selectionRange` is the identifier and `range` the whole item;
            // the first is where a reader wants the cursor to land.
            let range = item
                .get("selectionRange")
                .or_else(|| item.get("range"))
                .or_else(|| item.pointer("/location/range"));
            let (line, col) = range_start(range);
            out.push(Place {
                label: format!("{}{name}", "  ".repeat(depth)),
                rel: rel.to_string(),
                line,
                col,
            });
            if let Some(children) = item.get("children") {
                walk(children, depth + 1, rel, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(result, 0, rel, &mut out);
    out
}

/// Where a `textDocument/definition` answer points, with containment applied.
///
/// Three shapes again: a `Location`, an array of them, or an array of
/// `LocationLink`. The first is taken; a server that returns several is
/// answering about an item with several definitions, and jumping to the first is
/// what every editor does.
pub fn definition_target(result: &Value, root: &Path) -> DefTarget {
    let first = match result {
        Value::Array(items) => match items.first() {
            Some(item) => item.clone(),
            None => return DefTarget::NotFound,
        },
        Value::Null => return DefTarget::NotFound,
        other => other.clone(),
    };
    let (uri, range) = if let Some(uri) = first.get("targetUri").and_then(Value::as_str) {
        (uri.to_string(), first["targetSelectionRange"].clone())
    } else if let Some(uri) = first.get("uri").and_then(Value::as_str) {
        (uri.to_string(), first["range"].clone())
    } else {
        return DefTarget::NotFound;
    };
    let Some(path) = doc::from_uri(&uri) else {
        return DefTarget::Outside(uri);
    };
    let line = range["start"]["line"].as_u64().unwrap_or(0) as usize;
    let col = range["start"]["character"].as_u64().unwrap_or(0) as usize;
    // The containment law: a path that is not under the root is named, never
    // opened. The standard library and every registry checkout land here.
    match relative_to(root, &path) {
        Some(rel) => DefTarget::Inside {
            rel,
            line,
            // Still UTF-16, and the buffer this is a column into is a file the
            // page has not read. Converted after the open, where the lines
            // exist — or in `ask`, when the jump is inside the open file.
            col,
        },
        None => DefTarget::Outside(path.display().to_string()),
    }
}

/// One path spelling, so two spellings of one directory compare equal.
///
/// **Both halves of this are Windows defects the fork could not have seen**,
/// and both make every in-repository definition read as "outside this
/// repository" here:
///
/// 1. `path::root` canonicalises, and on Windows that produces `\\?\E:\...`,
///    while `doc::from_uri` returns `E:\...` — `strip_prefix` between the two
///    never matches. `doc::to_uri` already strips the verbatim prefix on the
///    way out for the same reason.
/// 2. rust-analyzer lower-cases the drive letter in everything it sends back,
///    which `tools/lsp`'s own `published_key` was fixed for. A byte comparison
///    against a root spelled `E:` fails on a URI spelled `e:`.
///
/// Separators are folded too, because a URI comes home with `/` and a Windows
/// root has `\`.
fn norm(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = if let Some(rest) = text.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = text.strip_prefix("//?/") {
        rest.to_string()
    } else {
        text
    };
    text.trim_end_matches('/').to_string()
}

/// `path` as a repo-relative, forward-slashed name, or `None` when it is not
/// under `root`.
///
/// The boundary check is a separate clause rather than a bare `starts_with`:
/// `/src/emma-old/x.rs` starts with `/src/emma` and is a different repository.
fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let (r, p) = (norm(root), norm(path));
    let (rk, pk) = if cfg!(windows) {
        (r.to_lowercase(), p.to_lowercase())
    } else {
        (r.clone(), p.clone())
    };
    if !pk.starts_with(&rk) {
        return None;
    }
    let rest = p.get(rk.len()..)?;
    let rest = rest.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    Some(rest.to_string())
}

/// What a file's extension is called in a refusal.
fn shown(rel: &str) -> String {
    match rel.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => format!(".{ext} files"),
        _ => format!("{rel:?}"),
    }
}

/// The server's own entry point, by name. `rust-analyzer`, not a path: the path
/// is machine-specific and the status row is one line wide.
fn entry_name(client: &Client) -> String {
    client
        .server()
        .entry
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| client.server().language.label.to_string())
}

// endregion: Reading what the server said

#[cfg(test)]
mod tests {
    //! Everything here but the two live cases runs against a fake language
    //! server over a pair of in-memory pipes — the instrument `tools/lsp`'s own
    //! scaffolding established, rebuilt here at the size this module needs. No
    //! network, no installed server, no real process: the properties being
    //! checked are properties of *this bridge*, and gating them behind an
    //! install is how they end up untested.
    //!
    //! The two `#[ignore]`d cases are the opposite claim and are marked as
    //! such: a fake agrees with its author, and the URI-spelling defect
    //! `tools/lsp` shipped with proves what that is worth.

    use super::*;
    use emma_tools_lsp::proto::{self, Frame};
    use emma_tools_lsp::server::{Server, Source};
    use tokio::io::BufReader;

    /// A scripted peer: answers `initialize`, declares itself quiescent, then
    /// answers from a table and publishes what the test asked it to.
    struct Fake {
        publishes: Option<Vec<Value>>,
        answers: Vec<(String, Value)>,
        /// Methods this server answers with a JSON-RPC `error` rather than a
        /// result. A refusal and an empty result are different answers, and a
        /// client that draws them the same says "there are none" when the truth
        /// is "it would not say".
        refusals: Vec<(String, String)>,
        /// Spell a published URI the way a real rust-analyzer does — the drive
        /// letter lower-cased. Without this the fake echoes the exact string it
        /// was handed, and a lookup keyed on the wrong spelling is green.
        respell: bool,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                publishes: None,
                answers: Vec::new(),
                refusals: Vec::new(),
                respell: false,
            }
        }

        fn publishing(mut self, items: Vec<Value>) -> Self {
            self.publishes = Some(items);
            self
        }

        fn answering(mut self, method: &str, result: Value) -> Self {
            self.answers.push((method.to_string(), result));
            self
        }

        /// Refuse a method the way a real server does when it cannot answer:
        /// a JSON-RPC `error`, not a null result. The two are different
        /// answers and a client that renders them the same is telling somebody
        /// "there are none" when the truth is "it would not say".
        fn refusing(mut self, method: &str, message: &str) -> Self {
            self.refusals
                .push((method.to_string(), message.to_string()));
            self
        }

        fn spelling_uris_as_a_real_server_does(mut self) -> Self {
            self.respell = true;
            self
        }

        /// Start it, and hand back a handshaken client plus the log of
        /// everything the client sent.
        async fn start(
            self,
            root: &Path,
            language: &'static lang::Language,
        ) -> (Arc<Client>, Arc<Mutex<Vec<Value>>>) {
            let (client_side, server_side) = tokio::io::duplex(64 * 1024);
            let (client_read, client_write) = tokio::io::split(client_side);
            let (server_read, mut server_write) = tokio::io::split(server_side);
            let sent: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
            let log = sent.clone();
            let publishes = self.publishes.clone();
            let answers = self.answers.clone();
            let refusals = self.refusals.clone();
            let respell = self.respell;
            tokio::spawn(async move {
                let mut reader = BufReader::new(server_read);
                loop {
                    let message = match proto::read_message(&mut reader).await {
                        Ok(Frame::Message(m)) => m,
                        _ => break,
                    };
                    log.lock().expect("sent").push(message.clone());
                    let method = message["method"].as_str().unwrap_or_default().to_string();
                    let id = message.get("id").cloned();
                    if method == "initialize" {
                        let reply = json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": { "capabilities": {} }
                        });
                        if proto::write_message(&mut server_write, &reply)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    if method == "initialized" {
                        // Authoritative readiness, so nothing in these tests
                        // waits out an indexing heuristic.
                        let status = json!({
                            "jsonrpc": "2.0",
                            "method": "experimental/serverStatus",
                            "params": { "quiescent": true, "health": "ok" }
                        });
                        let _ = proto::write_message(&mut server_write, &status).await;
                        continue;
                    }
                    if method == "textDocument/didOpen" || method == "textDocument/didChange" {
                        if let Some(items) = &publishes {
                            let uri = message["params"]["textDocument"]["uri"].clone();
                            let uri = match (respell, uri.as_str()) {
                                (true, Some(s)) => Value::String(respelled(s)),
                                _ => uri,
                            };
                            let note = json!({
                                "jsonrpc": "2.0",
                                "method": "textDocument/publishDiagnostics",
                                "params": { "uri": uri, "diagnostics": items }
                            });
                            let _ = proto::write_message(&mut server_write, &note).await;
                        }
                        continue;
                    }
                    if let Some(id) = id {
                        let reply =
                            if let Some((_, why)) = refusals.iter().find(|(m, _)| *m == method) {
                                json!({
                                    "jsonrpc": "2.0", "id": id,
                                    "error": { "code": -32603, "message": why }
                                })
                            } else {
                                let result = answers
                                    .iter()
                                    .find(|(m, _)| *m == method)
                                    .map(|(_, r)| r.clone())
                                    .unwrap_or(Value::Null);
                                json!({ "jsonrpc": "2.0", "id": id, "result": result })
                            };
                        if proto::write_message(&mut server_write, &reply)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            });
            let server = Server {
                program: PathBuf::from("fake"),
                args: Vec::new(),
                entry: PathBuf::from("fake-analyzer"),
                version: "0".into(),
                source: Source::Path,
                language,
            };
            let client = Client::connect(server, root, client_read, client_write)
                .await
                .expect("the fake handshakes");
            (client, sent)
        }
    }

    /// A `file:///E:/x` URI as a real rust-analyzer sends it back — and on a
    /// platform with no drive letters, unchanged, so the test asserts it
    /// respelled something before relying on it.
    fn respelled(uri: &str) -> String {
        let rest = uri.strip_prefix("file:///").unwrap_or(uri);
        let mut chars = rest.chars();
        match (chars.next(), chars.next()) {
            (Some(c), Some(':')) if c.is_ascii_alphabetic() => {
                format!("file:///{}{}", c.to_ascii_lowercase(), &rest[1..])
            }
            _ => uri.to_string(),
        }
    }

    fn rust() -> &'static lang::Language {
        lang::by_key("rust").expect("rust is in the table")
    }

    /// A collecting sink, and the updates it has seen so far.
    fn collector() -> (Sink, Arc<Mutex<Vec<LspUpdate>>>) {
        let seen: Arc<Mutex<Vec<LspUpdate>>> = Arc::new(Mutex::new(Vec::new()));
        let out = seen.clone();
        let sink: Sink = Arc::new(move |u| out.lock().expect("seen").push(u));
        (sink, seen)
    }

    /// Wait for the sink to hold something the predicate likes, or give up.
    /// Bounded, so a broken bridge fails the test rather than hanging it.
    async fn until(
        seen: &Arc<Mutex<Vec<LspUpdate>>>,
        what: impl Fn(&LspUpdate) -> bool,
    ) -> LspUpdate {
        until_within(seen, what, 400).await
    }

    /// The same, with the budget named. Four seconds is plenty for a fake over
    /// a pipe and nowhere near enough for a real server indexing a nine-crate
    /// workspace, which is the only reason this parameter exists.
    async fn until_within(
        seen: &Arc<Mutex<Vec<LspUpdate>>>,
        what: impl Fn(&LspUpdate) -> bool,
        attempts: usize,
    ) -> LspUpdate {
        for _ in 0..attempts {
            if let Some(found) = seen.lock().expect("seen").iter().find(|u| what(u)) {
                return found.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("nothing matching arrived: {:?}", seen.lock().expect("seen"));
    }

    /// A temp root with one rust file in it, a pool holding the fake for that
    /// root, and the running bridge.
    async fn rig(
        fake: Fake,
        body: &str,
    ) -> (
        tempfile::TempDir,
        Handle,
        Arc<Mutex<Vec<LspUpdate>>>,
        Arc<Mutex<Vec<Value>>>,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        std::fs::write(root.join("lib.rs"), body).expect("write");
        let (client, sent) = fake.start(&root, rust()).await;
        let pool = Arc::new(Pool::new());
        pool.adopt(&root, client).await;
        let (handle, rx) = channel();
        let (sink, seen) = collector();
        tokio::spawn(run(root, pool, rx, sink));
        (dir, handle, seen, sent)
    }

    fn open(rel: &str, text: &str) -> Request {
        Request::Open {
            rel: rel.into(),
            text: text.into(),
        }
    }

    fn a_diag() -> Value {
        json!({
            "range": {
                "start": { "line": 1, "character": 4 },
                "end": { "line": 1, "character": 9 }
            },
            "severity": 1,
            "message": "cannot find value `bod`"
        })
    }

    /// The whole point of the feature, end to end: a file opened in the page
    /// becomes a `didOpen` carrying the **buffer**, and what the server
    /// publishes about it becomes decorations in the view's own coordinates.
    #[tokio::test]
    async fn opening_a_file_syncs_the_buffer_and_its_diagnostics_come_back() {
        let (_dir, handle, seen, sent) =
            rig(Fake::new().publishing(vec![a_diag()]), "fn main() {}").await;
        assert!(handle.post(open("lib.rs", "fn main() {\n    bod();\n}\n")));

        let update = until(&seen, |u| matches!(u, LspUpdate::Diagnostics { .. })).await;
        let LspUpdate::Diagnostics { path, items } = update else {
            unreachable!()
        };
        assert_eq!(path, "lib.rs");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].line, 1);
        assert_eq!(items[0].start_col, 4);
        assert_eq!(items[0].end_col, 9);
        assert_eq!(items[0].severity, Severity::Error);
        assert!(items[0].message.contains("bod"), "{}", items[0].message);

        // And the buffer, not the file on disk: the server was told what is on
        // screen, which is the difference between decorations as you type and
        // decorations after you save.
        let log = sent.lock().expect("sent");
        let opened = log
            .iter()
            .find(|m| m["method"] == "textDocument/didOpen")
            .expect("a didOpen was sent");
        assert!(
            opened["params"]["textDocument"]["text"]
                .as_str()
                .unwrap_or_default()
                .contains("bod()"),
            "the unsaved buffer must be what was sent"
        );
    }

    /// The defect `tools/lsp` shipped with, at this layer: a real server spells
    /// the URI its own way, and a watcher that looked the publication up by the
    /// string it sent would wait out the session finding nothing.
    #[tokio::test]
    async fn a_publication_spelled_the_servers_way_still_reaches_the_page() {
        let fake = Fake::new()
            .publishing(vec![a_diag()])
            .spelling_uris_as_a_real_server_does();
        let (_dir, handle, seen, _sent) = rig(fake, "fn main() {}").await;
        assert!(handle.post(open("lib.rs", "fn main() {\n    bod();\n}\n")));
        let update = until(&seen, |u| matches!(u, LspUpdate::Diagnostics { .. })).await;
        assert!(matches!(update, LspUpdate::Diagnostics { items, .. } if items.len() == 1));
    }

    /// A publication that landed before the watcher existed still reaches the
    /// page: the publication is in the map before this spawns, and nothing
    /// further is ever published.
    ///
    /// **This test does not prove the ordering inside the watcher**, and the
    /// mutation sweep is how that was found out. Making the loop wait before
    /// its first look leaves it green, because `Client::publish_tick` clones a
    /// receiver the client never advances: the clone is behind from the moment
    /// it exists, so `changed()` returns at once for a publication that already
    /// happened. What is proved here is the *outcome* - a set already in the
    /// map becomes decorations - which is the thing the page depends on and
    /// stays true whichever of the two mechanisms delivers it.
    #[tokio::test]
    async fn a_publication_that_arrived_before_the_watcher_did_is_not_missed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        let path = root.join("lib.rs");
        std::fs::write(&path, "fn main() {}").expect("write");
        let (client, _sent) = Fake::new()
            .publishing(vec![a_diag()])
            .start(&root, rust())
            .await;

        // Provoke the publication and wait for it to land, so the watcher is
        // guaranteed to start after the only tick there will ever be.
        let uri = doc::to_uri(&path);
        let body = "fn main() {\n    bod();\n}\n";
        client.sync_document(&path, body);
        let mut waited = 0;
        while client.published_for(&uri).is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
            waited += 1;
            assert!(waited < 400, "the fake never published");
        }

        let (sink, seen) = collector();
        let text = Arc::new(Mutex::new(body.to_string()));
        tokio::spawn(watch_diagnostics(
            client.clone(),
            uri,
            "lib.rs".to_string(),
            text,
            sink,
        ));
        let update = until(&seen, |u| matches!(u, LspUpdate::Diagnostics { .. })).await;
        assert!(matches!(update, LspUpdate::Diagnostics { items, .. } if items.len() == 1));
    }

    /// **A question about a file that is not open is dropped, buffer and all.**
    ///
    /// `ready` guards on the name before it flushes, and the flush writes the
    /// request's text into the open file's document. Without the guard a stale
    /// `Hover` — posted before an open, answered after it — would send the
    /// *other* file's contents as this file's `didChange`, and the server would
    /// answer every later question about a document that never existed.
    #[tokio::test]
    async fn a_question_about_another_file_never_reaches_the_open_document() {
        let (_dir, handle, seen, sent) = rig(Fake::new(), "fn main() {}").await;
        handle.post(open("lib.rs", "fn main() {}"));
        until(&seen, |u| {
            matches!(u, LspUpdate::Status(LspStatus::Running(_)))
        })
        .await;

        assert!(handle.post(Request::Hover {
            rel: "somewhere/else.rs".into(),
            text: "THE WRONG BUFFER".into(),
            line: 0,
            col: 0,
        }));
        // Give the task time to do the wrong thing if it is going to.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let log = sent.lock().expect("sent");
        assert!(
            !log.iter()
                .any(|m| m.to_string().contains("THE WRONG BUFFER")),
            "another file's buffer was written into this document: {log:#?}"
        );
        assert!(
            !log.iter().any(|m| m["method"] == "textDocument/hover"),
            "a question about a file that is not open was asked anyway"
        );
    }

    /// The debounce, which is the whole reason the clock lives in the task:
    /// five keystrokes' worth of buffers collapse into one notification, and it
    /// is the newest one.
    ///
    /// **Every keystroke posts a `Change` and then a `Highlight`, because that
    /// is what the shell posts** -- and the first version of this test posted
    /// `Change` alone, a shape `app.rs` never produces. It stayed green for a
    /// week while the debounce was dead: `Request::Highlight` flushed the
    /// pending buffer like every other question, so the real binary sent one full
    /// `didChange` and one full token request per character typed. A test whose
    /// input shape is not the caller's input shape is a test of a program
    /// nobody runs.
    #[tokio::test]
    async fn a_burst_of_edits_collapses_into_one_did_change() {
        let (_dir, handle, seen, sent) = rig(Fake::new(), "fn main() {}").await;
        handle.post(open("lib.rs", "fn main() {}"));
        until(&seen, |u| {
            matches!(u, LspUpdate::Status(LspStatus::Running(_)))
        })
        .await;

        for n in 1..=5 {
            let text = format!("fn main() {{}}{}", "!".repeat(n));
            assert!(handle.post(Request::Change {
                rel: "lib.rs".into(),
                text: text.clone(),
            }));
            assert!(handle.post(Request::Highlight {
                rel: "lib.rs".into(),
                text,
            }));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(DEBOUNCE + Duration::from_millis(300)).await;

        let log = sent.lock().expect("sent");
        let changes: Vec<&Value> = log
            .iter()
            .filter(|m| m["method"] == "textDocument/didChange")
            .collect();
        assert_eq!(changes.len(), 1, "five edits, one didChange: {changes:?}");
        assert_eq!(
            changes[0]["params"]["contentChanges"][0]["text"],
            json!("fn main() {}!!!!!")
        );
        // And the same for the colour question, which is the request that was
        // forcing the flush. At most one per burst, asked after the buffer
        // landed rather than before it.
        let asks = log
            .iter()
            .filter(|m| m["method"] == "textDocument/semanticTokens/full")
            .count();
        assert!(
            asks <= 1,
            "a burst of typing asked for colour {asks} times: {log:?}"
        );
    }

    /// A save flushes rather than races, and says so on the wire: the server
    /// must not be told the file was written and only then told what was in it.
    #[tokio::test]
    async fn a_save_sends_the_buffer_and_then_did_save() {
        let (_dir, handle, seen, sent) = rig(Fake::new(), "fn main() {}").await;
        handle.post(open("lib.rs", "fn main() {}"));
        until(&seen, |u| {
            matches!(u, LspUpdate::Status(LspStatus::Running(_)))
        })
        .await;
        handle.post(Request::Change {
            rel: "lib.rs".into(),
            text: "fn main() { saved(); }".into(),
        });
        handle.post(Request::Save {
            rel: "lib.rs".into(),
            text: "fn main() { saved(); }".into(),
        });
        tokio::time::sleep(DEBOUNCE * 4).await;

        let log = sent.lock().expect("sent");
        let methods: Vec<&str> = log
            .iter()
            .filter_map(|m| m["method"].as_str())
            .filter(|m| m.starts_with("textDocument/"))
            .collect();
        assert_eq!(
            methods,
            [
                "textDocument/didOpen",
                "textDocument/didChange",
                "textDocument/didSave"
            ],
            "the save must flush the pending buffer before announcing itself"
        );
    }

    /// Closing tells the server, so a document the person is no longer looking
    /// at does not stay in an index the model will later ask about.
    #[tokio::test]
    async fn closing_the_file_tells_the_server() {
        let (_dir, handle, seen, sent) = rig(Fake::new(), "fn main() {}").await;
        handle.post(open("lib.rs", "fn main() {}"));
        until(&seen, |u| {
            matches!(u, LspUpdate::Status(LspStatus::Running(_)))
        })
        .await;
        handle.post(Request::Close {
            rel: "lib.rs".into(),
        });
        until(&seen, |u| matches!(u, LspUpdate::Status(LspStatus::Idle))).await;
        for _ in 0..100 {
            if sent
                .lock()
                .expect("sent")
                .iter()
                .any(|m| m["method"] == "textDocument/didClose")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no didClose reached the server");
    }

    #[tokio::test]
    async fn hover_comes_back_as_lines_without_its_fences() {
        let fake = Fake::new().answering(
            "textDocument/hover",
            json!({ "contents": {
                "kind": "markdown",
                "value": "```rust\nfn main()\n```\nThe entry point."
            } }),
        );
        let (_dir, handle, seen, _sent) = rig(fake, "fn main() {}").await;
        handle.post(open("lib.rs", "fn main() {}"));
        handle.post(Request::Hover {
            rel: "lib.rs".into(),
            text: "fn main() {}".into(),
            line: 0,
            col: 3,
        });
        let update = until(&seen, |u| matches!(u, LspUpdate::Hover { .. })).await;
        let LspUpdate::Hover { lines, .. } = update else {
            unreachable!()
        };
        let lines = lines.expect("the fake answered with contents");
        assert_eq!(lines, ["fn main()", "The entry point."]);
    }

    /// A server with nothing to say says nothing, and the page is told that
    /// rather than shown an empty box.
    ///
    /// **Two shapes, because the mutation sweep found the second untested.**
    /// A server that omits `contents` and a server that sends an empty
    /// `contents` are both "nothing here", and the first version of this test
    /// only ever exercised the first: `Value::Null` fails at the `get` and
    /// never reaches the emptiness check below it, which deleting left green.
    #[tokio::test]
    async fn hover_with_no_content_is_an_answer_not_a_popup() {
        for empty in [
            json!({}),
            json!({ "contents": { "kind": "markdown", "value": "" } }),
            json!({ "contents": { "kind": "markdown", "value": "```rust\n```\n\n" } }),
        ] {
            assert_eq!(hover_lines(&empty), None, "{empty} is not a popup");
        }
        let (_dir, handle, seen, _sent) = rig(Fake::new(), "fn main() {}").await;
        handle.post(open("lib.rs", "fn main() {}"));
        handle.post(Request::Hover {
            rel: "lib.rs".into(),
            text: "fn main() {}".into(),
            line: 0,
            col: 0,
        });
        let update = until(&seen, |u| matches!(u, LspUpdate::Hover { .. })).await;
        assert!(matches!(update, LspUpdate::Hover { lines: None, .. }));
    }

    /// Where a symbol is used, all the way through - and the half that
    /// matters is the location this test drops rather than the one it keeps.
    #[tokio::test]
    async fn a_reference_outside_the_root_is_not_offered_as_a_place() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        std::fs::write(root.join("lib.rs"), "fn main() {}").expect("write");
        let outside = dir.path().parent().expect("parent").join("elsewhere.rs");
        let fake = Fake::new().answering(
            "textDocument/references",
            json!([
                {
                    "uri": doc::to_uri(&root.join("lib.rs")),
                    "range": {
                        "start": { "line": 4, "character": 8 },
                        "end": { "line": 4, "character": 12 }
                    }
                },
                {
                    "uri": doc::to_uri(&outside),
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 4 }
                    }
                }
            ]),
        );
        let (client, sent) = fake.start(&root, rust()).await;
        let pool = Arc::new(Pool::new());
        pool.adopt(&root, client).await;
        let (handle, rx) = channel();
        let (sink, seen) = collector();
        tokio::spawn(run(root, pool, rx, sink));

        handle.post(open("lib.rs", "fn main() {}"));
        handle.post(Request::References {
            rel: "lib.rs".into(),
            text: "fn main() {}".into(),
            line: 0,
            col: 3,
        });
        let update = until(&seen, |u| matches!(u, LspUpdate::Places { .. })).await;
        let LspUpdate::Places { about, rows, .. } = update else {
            unreachable!("matched above");
        };
        assert_eq!(about, "references to main", "the header names the word");
        assert_eq!(rows.len(), 1, "the hit outside the root is not a row");
        assert_eq!(rows[0].rel, "lib.rs");
        assert_eq!(rows[0].line, 4);
        assert_eq!(rows[0].col, 8);

        // And the request carried the context the spec makes mandatory: a
        // server within its rights to reject a `ReferenceParams` without one
        // would fail silently here, because a request that times out draws
        // nothing at all.
        let log = sent.lock().expect("sent");
        let asked = log
            .iter()
            .find(|m| m.get("method").and_then(Value::as_str) == Some("textDocument/references"))
            .expect("a references request was sent");
        assert_eq!(
            asked.pointer("/params/context/includeDeclaration"),
            Some(&json!(true)),
            "ReferenceParams requires a context"
        );
    }

    /// **A refused question is a sentence, not an empty panel.**
    ///
    /// `ask` has said so since the module was written; the two requests that
    /// spawn their own tasks -- references and symbols -- matched only
    /// `Ok(Ok(_))` and dropped the other two arms on the floor. A server that
    /// refuses, or one still indexing that never answers, therefore drew
    /// nothing at all, which on this page reads as "there are none". Two
    /// functions doing one job, and only one of them had the rule.
    #[tokio::test]
    async fn a_refused_places_question_is_said_rather_than_drawn_as_nothing() {
        for (method, request) in [
            (
                "textDocument/references",
                Request::References {
                    rel: "lib.rs".into(),
                    text: "fn main() {}".into(),
                    line: 0,
                    col: 3,
                },
            ),
            (
                "textDocument/documentSymbol",
                Request::Symbols {
                    rel: "lib.rs".into(),
                    text: "fn main() {}".into(),
                },
            ),
        ] {
            let fake = Fake::new().refusing(method, "content modified");
            let (_dir, handle, seen, _sent) = rig(fake, "fn main() {}").await;
            handle.post(open("lib.rs", "fn main() {}"));
            handle.post(request);
            let update = until(&seen, |u| matches!(u, LspUpdate::Note { .. })).await;
            let LspUpdate::Note { text, .. } = update else {
                unreachable!("matched above");
            };
            assert!(
                text.contains("content modified"),
                "{method} was refused and said {text:?}"
            );
        }
    }

    /// What a file contains, nested: the tree is flattened depth-first and the
    /// nesting survives as indent, because a list of bare names cannot say
    /// which `new` belongs to which type.
    #[tokio::test]
    async fn the_symbols_of_a_file_come_back_flattened_and_still_nested() {
        let fake = Fake::new().answering(
            "textDocument/documentSymbol",
            json!([{
                "name": "Widget",
                "kind": 23,
                "range": {
                    "start": { "line": 2, "character": 0 },
                    "end": { "line": 9, "character": 1 }
                },
                "selectionRange": {
                    "start": { "line": 2, "character": 7 },
                    "end": { "line": 2, "character": 13 }
                },
                "children": [{
                    "name": "new",
                    "kind": 6,
                    "range": {
                        "start": { "line": 4, "character": 4 },
                        "end": { "line": 6, "character": 5 }
                    },
                    "selectionRange": {
                        "start": { "line": 4, "character": 11 },
                        "end": { "line": 4, "character": 14 }
                    }
                }]
            }]),
        );
        let (_dir, handle, seen, _sent) = rig(fake, "fn main() {}").await;
        handle.post(open("lib.rs", "fn main() {}"));
        handle.post(Request::Symbols {
            rel: "lib.rs".into(),
            text: "fn main() {}".into(),
        });
        let update = until(&seen, |u| matches!(u, LspUpdate::Places { .. })).await;
        let LspUpdate::Places { rows, .. } = update else {
            unreachable!("matched above");
        };
        let labels: Vec<&str> = rows.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, vec!["Widget", "  new"], "the child is indented");
        // The identifier, not the item: `selectionRange` beats `range`, so
        // pressing Enter lands on the name rather than on the line above it.
        assert_eq!((rows[0].line, rows[0].col), (2, 7));
        assert_eq!((rows[1].line, rows[1].col), (4, 11));
    }

    /// The other wire shape. A server that ignores the hierarchical
    /// declaration answers with `SymbolInformation`, whose position is under
    /// `location`, and reading only the first shape would draw an empty file.
    #[test]
    fn a_flat_symbol_answer_is_read_as_well_as_a_nested_one() {
        let rows = symbol_places(
            &json!([{
                "name": "main",
                "kind": 12,
                "location": {
                    "uri": "file:///whatever/lib.rs",
                    "range": {
                        "start": { "line": 7, "character": 3 },
                        "end": { "line": 7, "character": 7 }
                    }
                }
            }]),
            "lib.rs",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "main");
        assert_eq!((rows[0].line, rows[0].col), (7, 3));
    }

    /// The panel's header names the word under the cursor, and the cursor is
    /// allowed to be anywhere in it - including at the very end, which is
    /// where it sits after somebody has just typed the name.
    #[test]
    fn the_word_under_the_cursor_is_found_from_either_end_of_it() {
        let text = "let widget = 1;\nfn other() {}";
        assert_eq!(word_at(text, 0, 4), "widget", "at the start");
        assert_eq!(word_at(text, 0, 7), "widget", "in the middle");
        assert_eq!(word_at(text, 0, 10), "widget", "at the end");
        assert_eq!(word_at(text, 1, 3), "other", "on the second line");
        assert_eq!(word_at(text, 0, 12), "", "on nothing");
        assert_eq!(word_at(text, 9, 0), "", "past the end of the file");
    }

    /// Go to definition, all the way through: the server's URI comes back as a
    /// repo-relative path the page can open.
    #[tokio::test]
    async fn a_definition_inside_the_root_comes_back_relative() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        std::fs::write(root.join("lib.rs"), "fn main() {}").expect("write");
        std::fs::write(root.join("other.rs"), "fn other() {}").expect("write");
        let target = doc::to_uri(&root.join("other.rs"));
        let fake = Fake::new().answering(
            "textDocument/definition",
            json!({
                "uri": target,
                "range": {
                    "start": { "line": 0, "character": 3 },
                    "end": { "line": 0, "character": 8 }
                }
            }),
        );
        let (client, _sent) = fake.start(&root, rust()).await;
        let pool = Arc::new(Pool::new());
        pool.adopt(&root, client).await;
        let (handle, rx) = channel();
        let (sink, seen) = collector();
        tokio::spawn(run(root, pool, rx, sink));

        handle.post(open("lib.rs", "fn main() {}"));
        handle.post(Request::Definition {
            rel: "lib.rs".into(),
            text: "fn main() {}".into(),
            line: 0,
            col: 3,
        });
        let update = until(&seen, |u| matches!(u, LspUpdate::Definition { .. })).await;
        let LspUpdate::Definition { target, .. } = update else {
            unreachable!()
        };
        assert_eq!(
            target,
            DefTarget::Inside {
                rel: "other.rs".into(),
                line: 0,
                col: 3
            }
        );
    }

    /// The containment law: a definition in the standard library is named,
    /// never opened.
    #[test]
    fn a_definition_outside_the_root_is_named_and_not_opened() {
        let root = repo_root();
        let outside = doc::to_uri(&PathBuf::from(elsewhere()).join("core/option.rs"));
        let answer = json!([{
            "targetUri": outside,
            "targetSelectionRange": {
                "start": { "line": 12, "character": 4 },
                "end": { "line": 12, "character": 8 }
            }
        }]);
        match definition_target(&answer, &root) {
            DefTarget::Outside(shown) => assert!(shown.contains("option.rs"), "{shown}"),
            other => panic!("a path outside the root must not be opened: {other:?}"),
        }
    }

    /// A neutral absolute root, spelled the way the host spells one.
    fn repo_root() -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            "C:\\src\\emma"
        } else {
            "/src/emma"
        })
    }

    fn elsewhere() -> &'static str {
        if cfg!(windows) {
            "C:\\src\\rust"
        } else {
            "/src/rust"
        }
    }

    /// The boundary the `starts_with` shape gets wrong: a sibling directory
    /// whose name begins with the root's is a different repository, and its
    /// files are outside.
    #[test]
    fn a_sibling_directory_with_the_roots_name_as_a_prefix_is_outside() {
        let root = repo_root();
        let sibling = if cfg!(windows) {
            "C:\\src\\emma-old\\lib.rs"
        } else {
            "/src/emma-old/lib.rs"
        };
        assert_eq!(relative_to(&root, Path::new(sibling)), None);
        let inside = if cfg!(windows) {
            "C:\\src\\emma\\lib.rs"
        } else {
            "/src/emma/lib.rs"
        };
        assert_eq!(
            relative_to(&root, Path::new(inside)),
            Some("lib.rs".to_string())
        );
    }

    /// Windows only, and the reason `norm` exists: the root arrives
    /// canonicalised (`\\?\E:\...`) and the answer arrives from a URI
    /// (`e:\...`), lower-cased by rust-analyzer. Byte equality between the two
    /// is `false`, and the page would report every one of its own files as
    /// defined outside the repository.
    #[test]
    #[cfg(windows)]
    fn a_verbatim_root_and_a_lower_cased_drive_letter_are_one_repository() {
        let root = PathBuf::from("\\\\?\\C:\\src\\emma");
        let answer = PathBuf::from("c:\\src\\emma\\crates\\emma\\src\\main.rs");
        assert_eq!(
            relative_to(&root, &answer),
            Some("crates/emma/src/main.rs".to_string())
        );
    }

    /// The other half of `norm`, and the other spelling Windows has: a share
    /// canonicalises to a `//?/UNC/` prefix and comes back from a URI as a
    /// plain `//host/share`. Untested until the mutation sweep deleted the
    /// branch and nothing went red.
    ///
    /// Written with forward slashes because `norm` folds separators first, and
    /// run on both platforms because the string logic is the same on each.
    #[test]
    fn a_unc_root_and_the_share_it_names_are_one_repository() {
        let root = PathBuf::from("//?/UNC/build/share/emma");
        let answer = PathBuf::from("//build/share/emma/crates/emma/src/main.rs");
        assert_eq!(
            relative_to(&root, &answer),
            Some("crates/emma/src/main.rs".to_string())
        );
    }

    #[test]
    fn an_empty_definition_answer_is_not_found_rather_than_a_guess() {
        let root = repo_root();
        assert_eq!(definition_target(&json!([]), &root), DefTarget::NotFound);
        assert_eq!(definition_target(&Value::Null, &root), DefTarget::NotFound);
    }

    /// An unsupported extension and a disabled language are different
    /// sentences, and neither of them is silence.
    #[tokio::test]
    async fn a_file_with_no_server_says_which_kind_of_no() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        std::fs::write(root.join("notes.md"), "hello").expect("write");
        std::fs::write(root.join("main.tf"), "resource {}").expect("write");
        // Terraform is a real language in the table and is not in the default
        // enabled set: the "off, and you can turn it on" case, which must not
        // read like the "there is no server for this" case.
        let pool = Arc::new(Pool::new());
        let (handle, rx) = channel();
        let (sink, seen) = collector();
        tokio::spawn(run(root, pool, rx, sink));

        handle.post(open("notes.md", "hello"));
        let update = until(&seen, |u| {
            matches!(u, LspUpdate::Status(LspStatus::Unsupported(_)))
        })
        .await;
        assert!(line_of(&update).contains(".md"), "{update:?}");

        handle.post(open("main.tf", "resource {}"));
        let update = until(&seen, |u| {
            matches!(u, LspUpdate::Status(LspStatus::Disabled(_)))
        })
        .await;
        assert!(
            line_of(&update).contains("lsp.enabled"),
            "a disabled language must name the switch: {update:?}"
        );
    }

    fn line_of(update: &LspUpdate) -> String {
        match update {
            LspUpdate::Status(s) => s.line(),
            other => format!("{other:?}"),
        }
    }

    /// **The architecture rule, as a test.** The input thread's only move is
    /// `try_send`, so a bridge that is gone, wedged, or backed up costs a
    /// request and never a keystroke. Seamed twice — a dropped receiver and a
    /// full queue — because those are the two ways a blocking send would park
    /// the thread that paints.
    #[tokio::test]
    async fn a_dead_or_backed_up_bridge_never_blocks_the_poster() {
        let (handle, rx) = channel();
        drop(rx);
        let started = std::time::Instant::now();
        assert!(!handle.post(open("lib.rs", "x")), "a dead task must refuse");
        assert!(started.elapsed() < Duration::from_millis(50));

        // A receiver that exists and is never drained: the queue fills, and the
        // post after that returns rather than waiting for room.
        let (handle, _held) = channel();
        for _ in 0..QUEUE {
            assert!(handle.post(open("lib.rs", "x")));
        }
        let started = std::time::Instant::now();
        assert!(
            !handle.post(open("lib.rs", "x")),
            "a full queue must refuse"
        );
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "post must never wait for room"
        );
    }

    /// LSP counts columns in UTF-16 code units and the page counts chars. One
    /// emoji on the line is the whole difference, and getting it wrong puts
    /// every decoration after it under the wrong character.
    /// **The marker goes on the argument the server said, not on the first
    /// place its text happens to appear.**
    ///
    /// `fn f(fun: u8, un: u8)` with the second argument active: a bare
    /// `label.find("un: u8")` matches inside `fun` and brackets the *first*
    /// parameter, which the code's own comment already called worse than no
    /// marker at all. The parameters are consumed left to right instead.
    #[test]
    fn the_signature_marker_lands_on_the_argument_the_server_named() {
        let value = json!({
            "signatures": [{
                "label": "fn f(fun: u8, un: u8)",
                "parameters": [{ "label": "fun: u8" }, { "label": "un: u8" }],
                "activeParameter": 1,
            }],
            "activeSignature": 0,
        });
        assert_eq!(
            signature_line(&value).as_deref(),
            Some("fn f(fun: u8, [un: u8])"),
        );

        // The first argument still works, and so does a label the parameter is
        // not in at all: the bare label rather than a marker somewhere random.
        let first = json!({
            "signatures": [{
                "label": "fn f(fun: u8, un: u8)",
                "parameters": [{ "label": "fun: u8" }, { "label": "un: u8" }],
                "activeParameter": 0,
            }],
            "activeSignature": 0,
        });
        assert_eq!(
            signature_line(&first).as_deref(),
            Some("fn f([fun: u8], un: u8)"),
        );
        let absent = json!({
            "signatures": [{
                "label": "fn f(a: u8)",
                "parameters": [{ "label": "nowhere" }],
                "activeParameter": 0,
            }],
            "activeSignature": 0,
        });
        assert_eq!(signature_line(&absent).as_deref(), Some("fn f(a: u8)"));
    }

    /// A completion's replace range crosses the UTF-16 boundary through the
    /// same function every other column does.
    ///
    /// This had its own copy of the conversion, so a line with an astral
    /// character on it could have been converted by one rule here and another
    /// in the definition jump. `😀` is two UTF-16 units and one char, so a
    /// range that starts after it is the whole test.
    #[test]
    fn a_completion_range_is_converted_by_the_shared_rule() {
        use emma_tools_lsp::render::Completion;
        let buffer = "let 😀 = x.pus";
        // `x.pus` begins at UTF-16 column 8 (`let ` is 4, the emoji 2, ` = ` 3
        // -- so `x` is at 9) and `pus` runs from 11 to 14.
        let raw = Completion {
            label: "push".into(),
            filter: "push".into(),
            insert: "push".into(),
            replace: Some((
                doc::Position {
                    line: 0,
                    character: 11,
                },
                doc::Position {
                    line: 0,
                    character: 14,
                },
            )),
            kind: "method",
            detail: None,
            documentation: None,
            sort: "push".into(),
            snippet: false,
        };
        let c = candidate(&raw, buffer);
        assert_eq!(
            c.replace,
            Some((char_column_in(buffer, 11), char_column_in(buffer, 14))),
            "the range must be the shared conversion's answer"
        );
        assert_eq!(
            c.replace,
            Some((10, 13)),
            "one char, not two, for the emoji"
        );
    }

    #[test]
    fn utf16_columns_become_char_columns_and_back() {
        let line = "let x = \"\u{1F980}\"; let y = 1;";
        let utf16: u32 = line.encode_utf16().count() as u32;
        assert_eq!(char_column(line, 0, utf16), line.chars().count());
        // The crab is two UTF-16 units and one char, so everything after it is
        // one column further left in the page's coordinates.
        let after = "let x = \"\u{1F980}".encode_utf16().count() as u32;
        assert_eq!(
            char_column(line, 0, after),
            "let x = \"\u{1F980}".chars().count()
        );
        // And back the other way, which is what a hover request is built from.
        let p = position_of(line, 0, "let x = \"\u{1F980}".chars().count());
        assert_eq!(p.character, after);
    }

    /// The same conversion, on a whole diagnostic: a span the server measured
    /// in UTF-16 lands under the characters it is about.
    #[test]
    fn a_diagnostics_span_survives_a_wide_character_on_the_line() {
        let text = "let s = \"\u{1F980}\"; bad();";
        let start = "let s = \"\u{1F980}\"; ".encode_utf16().count();
        let end = start + 3;
        let d = convert(
            &json!({
                "range": {
                    "start": { "line": 0, "character": start },
                    "end": { "line": 0, "character": end }
                },
                "severity": 2,
                "message": "unresolved"
            }),
            text,
        );
        assert_eq!(d.severity, Severity::Warning);
        let chars: Vec<char> = text.chars().collect();
        let named: String = chars[d.start_col..d.end_col].iter().collect();
        assert_eq!(named, "bad");
    }

    #[test]
    fn a_missing_severity_is_an_error_rather_than_a_hint() {
        let d = convert(
            &json!({
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 1 }
                },
                "message": "something"
            }),
            "x",
        );
        assert_eq!(d.severity, Severity::Error);
    }

    // region: Against a real language server
    // -----------------------------------------------------------------------
    // Two live cases. `#[ignore]`d because they need rust-analyzer installed
    // and cost seconds of indexing; run with `-- --ignored --nocapture` and
    // paste what they print, which is what the report does.
    // -----------------------------------------------------------------------

    /// The repository this crate lives in.
    fn this_repository() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/emma is two below the root")
            .canonicalize()
            .expect("canonical")
    }

    /// **Certification.** A real rust-analyzer, this repository, a real file in
    /// it, and a buffer that is not what is on disk — which is the claim the
    /// fake cannot make: the diagnostics come back for text that exists only in
    /// the page.
    #[tokio::test]
    #[ignore = "needs a real rust-analyzer and indexes this repository"]
    async fn a_real_server_decorates_a_buffer_that_is_not_on_disk() {
        let root = this_repository();
        let rel = "crates/emma/src/term/code_lsp.rs";
        let on_disk = std::fs::read_to_string(root.join(rel)).expect("read");
        // One deliberate wound, in a buffer nothing will ever write back.
        let buffer = on_disk.replacen("pub const QUEUE: usize = 64;", "pub const QUEUE = ;", 1);
        assert!(buffer != on_disk, "the wound must actually apply");

        let pool = Arc::new(Pool::new());
        let (handle, rx) = channel();
        let (sink, seen) = collector();
        tokio::spawn(run(root.clone(), pool, rx, sink));
        assert!(handle.post(Request::Open {
            rel: rel.into(),
            text: buffer.clone(),
        }));

        let started = std::time::Instant::now();
        // Two minutes, because this is a nine-crate workspace and a cold
        // rust-analyzer cache is minutes rather than seconds. The bridge itself
        // never waits: it publishes when it has something.
        let update = until_within(
            &seen,
            |u| matches!(u, LspUpdate::Diagnostics { .. }),
            12_000,
        )
        .await;
        let LspUpdate::Diagnostics { path, items } = update else {
            unreachable!()
        };
        println!("--- root: {}", root.display());
        println!("--- diagnostics for {path} after {:.2?}", started.elapsed());
        for d in items.iter().take(10) {
            println!(
                "  {}:{} {:?} {}",
                d.line + 1,
                d.start_col + 1,
                d.severity,
                d.message
            );
        }
        assert!(!items.is_empty(), "a broken buffer must produce something");
        // And the file on disk is untouched: this page sent a buffer, not a
        // write.
        assert_eq!(
            std::fs::read_to_string(root.join(rel)).expect("read"),
            on_disk
        );
    }

    /// **Certification, the other half.** Hover and definition against the same
    /// real server, on a symbol in a real file of this repository.
    #[tokio::test]
    #[ignore = "needs a real rust-analyzer and indexes this repository"]
    async fn a_real_server_answers_hover_and_definition_in_this_repository() {
        let root = this_repository();
        let rel = "crates/emma/src/term/code_lsp.rs";
        let text = std::fs::read_to_string(root.join(rel)).expect("read");
        // The `DEBOUNCE` in `pub const DEBOUNCE: Duration`, by construction
        // rather than by a line number that rots on the next edit.
        let (line, col) = text
            .lines()
            .enumerate()
            .find_map(|(i, l)| {
                l.find("pub const DEBOUNCE")
                    .map(|c| (i, c + "pub const ".len()))
            })
            .expect("the constant this test is about");

        let pool = Arc::new(Pool::new());
        let (handle, rx) = channel();
        let (sink, seen) = collector();
        tokio::spawn(run(root.clone(), pool, rx, sink));
        handle.post(Request::Open {
            rel: rel.into(),
            text: text.clone(),
        });
        until(&seen, |u| {
            matches!(u, LspUpdate::Status(LspStatus::Running(_)))
        })
        .await;

        // **`ASK_TIMEOUT` is a person's patience, not an indexing budget**, so
        // the first `F5` against a cold pool comes back as the "still indexing"
        // note and the answer arrives on a later press. That is the shipped
        // behaviour, and this test drives it rather than hiding it behind a
        // longer timeout: it presses until an answer arrives and prints how
        // many presses it took.
        let mut presses = 0;
        let hover = loop {
            presses += 1;
            assert!(presses <= 12, "twelve presses and still no hover");
            handle.post(Request::Hover {
                rel: rel.into(),
                text: text.clone(),
                line,
                col,
            });
            let answer = until_within(
                &seen,
                |u| matches!(u, LspUpdate::Hover { .. } | LspUpdate::Note { .. }),
                2_000,
            )
            .await;
            if let LspUpdate::Note { text, .. } = &answer {
                println!("--- press {presses}: {text}");
                seen.lock().expect("seen").clear();
                continue;
            }
            break answer;
        };
        println!(
            "--- hover after {presses} press(es) at {}:{}",
            line + 1,
            col + 1
        );
        println!("{hover:#?}");

        handle.post(Request::Definition {
            rel: rel.into(),
            text: text.clone(),
            line,
            col,
        });
        let def = until_within(&seen, |u| matches!(u, LspUpdate::Definition { .. }), 2_000).await;
        println!("--- definition: {def:#?}");
        let LspUpdate::Definition { target, .. } = def else {
            unreachable!()
        };
        // The containment law against a real answer: the definition of a
        // constant declared in this file is inside this repository, and it is
        // named relative to the root rather than as an absolute path.
        assert!(
            matches!(&target, DefTarget::Inside { rel: r, .. } if r == rel),
            "a definition in the open file must come back relative: {target:?}"
        );
    }

    // endregion: Against a real language server
}
