//! Where the server lives, and what happens when it stops living.
//!
//! **Why a pool at all.** Starting rust-analyzer per request costs a spawn plus
//! a full re-index — measured in tens of seconds on this workspace — for one
//! question. So one server per root, started on the first call that needs it and
//! kept.
//!
//! **Where it hangs.** Off the tools themselves, behind an `Arc`, exactly as
//! `tools/fs` hangs its `ReadTracker`: `ToolCtx` carries no session state, so
//! shared state lives in the tool structs and `lsp_tools()` is the only
//! supported way to build the set. The same known defect applies and is worth
//! restating rather than discovering — a tool constructed with its own pool gets
//! its own rust-analyzer, and it compiles.
//!
//! Not a global `static`. A process-wide singleton would outlive any particular
//! session, would be untestable without leaking a server between test cases, and
//! would put the teardown back in `main` — the exact shape of the `tools/web`
//! leak this crate is trying not to repeat.
//!
//! **Keyed by root and language**, because a language server is a project's
//! index and Emma's root is the project. Two roots is two servers; the same root
//! asked twice is one. Root alone was the key while there was one language and
//! it cannot stay that way: a workspace with `.rs` and `.tf` in it needs two
//! servers, and the crash ceiling has to be per server or a bicep server that
//! dies three times stops rust from ever starting.
//!
//! **When it dies.** The client notices and records why (see
//! `client::pump_reader`). The pool checks before handing one out, drops the
//! corpse, and starts a fresh one — up to [`MAX_CRASHES`]. The ceiling is the
//! part worth arguing for: without it, a server that panics on this particular
//! workspace becomes an invisible respawn on every call, each costing a spawn
//! and an index, and the model sees a slow tool rather than a broken one. At the
//! ceiling the pool refuses with the last death message, which contains the
//! server's own stderr.
//!
//! **The request in flight when it dies** is not retried here. It comes back
//! `Failed` with the exit and the stderr tail, and the model decides. An
//! automatic retry would hide a crash loop behind a tool that merely feels slow,
//! and it would re-run a request whose crash it may well have caused.
//!
//! **Nothing reaps an idle server.** A rust-analyzer holding a workspace index
//! is hundreds of megabytes, and the honest thing is to say that it stays until
//! the pool is dropped rather than to imply otherwise. An idle timer was
//! considered and refused for this pass: it is a background task with its own
//! lifecycle, and a background task that outlives its owner is the bug this
//! module is organised around not having.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use emma_tool_api::ToolError;

use crate::client::Client;
use crate::lang::Language;
use crate::server::{self, Server};

// region: The pool
// ---------------------------------------------------------------------------
// The pool
//
// A map from root to a running client, and the crash accounting that keeps a
// broken server from looking like a slow one.
// ---------------------------------------------------------------------------

/// How many times a server may die before the pool stops starting new ones.
///
/// Three, because the failures that are worth retrying are the transient ones —
/// a race with a `cargo` process holding a lock, a machine briefly out of file
/// handles — and those clear in one. Anything that survives three starts is a
/// property of this workspace or this binary, and re-running it costs seconds
/// per call while producing the same result.
pub const MAX_CRASHES: u32 = 3;

#[derive(Default)]
struct Slot {
    client: Option<Arc<Client>>,
    crashes: u32,
    last_death: Option<String>,
}

/// The shared, lazily-started language servers.
///
/// `tokio::sync::Mutex` rather than `std`'s: starting a server is an `await`,
/// and it is held across that await on purpose. Two tools asking at once during
/// the first call of a session is the normal case, and the alternative to
/// serialising them is two rust-analyzers indexing the same workspace.
pub struct Pool {
    slots: tokio::sync::Mutex<HashMap<Key, Slot>>,
    /// Which languages may be started. Everything else is refused by
    /// [`server::disabled_language`] before a process exists.
    enabled: Vec<String>,
}

/// One root, one language. See the module doc for why root alone will not do.
type Key = (PathBuf, &'static str);

/// `Default` is [`Pool::new`] and therefore the default language set. Derived
/// would be an empty `enabled`, which is a pool that refuses everything, and a
/// wrong default here is silent.
impl Default for Pool {
    fn default() -> Self {
        Self::new()
    }
}

impl Pool {
    /// A pool serving the default language set.
    pub fn new() -> Self {
        Self::with_enabled(crate::lang::DEFAULT_ENABLED.iter().map(|s| s.to_string()))
    }

    /// A pool serving exactly these language keys.
    ///
    /// Unknown keys are kept rather than rejected. A settings file written by a
    /// newer build naming a language this one has never heard of should not stop
    /// the ones it does know from working, and [`Pool::unknown_keys`] is how a
    /// caller reports the difference instead.
    pub fn with_enabled(keys: impl IntoIterator<Item = String>) -> Self {
        Self {
            slots: tokio::sync::Mutex::new(HashMap::new()),
            enabled: keys
                .into_iter()
                .map(|k| k.trim().to_ascii_lowercase())
                .collect(),
        }
    }

    /// Whether this pool may start a server for `language`.
    pub fn is_enabled(&self, language: &Language) -> bool {
        self.enabled.iter().any(|k| k == language.key)
    }

    /// Enabled keys that name no language here, for a caller that wants to say
    /// so out loud.
    pub fn unknown_keys(&self) -> Vec<String> {
        self.enabled
            .iter()
            .filter(|k| crate::lang::by_key(k).is_none())
            .cloned()
            .collect()
    }

    /// The running server for `root`, starting one if there is not one.
    ///
    /// `root` must already be canonical — it comes from `path::root`, which is
    /// the same canonicalisation every filesystem tool uses. Keying on anything
    /// else would let two spellings of one directory become two servers.
    pub async fn client(
        &self,
        root: &Path,
        language: &'static Language,
    ) -> Result<Arc<Client>, ToolError> {
        // The switch is checked before the lock and before any process, so a
        // disabled language costs nothing and cannot contend with an enabled one.
        if !self.is_enabled(language) {
            return Err(server::disabled_language(language));
        }
        let mut slots = self.slots.lock().await;
        let slot = slots.entry((root.to_path_buf(), language.key)).or_default();

        if let Some(existing) = &slot.client {
            match existing.death() {
                None => return Ok(existing.clone()),
                Some(why) => {
                    // Dropping it here is what kills the process: `Client`'s
                    // `Drop` aborts the pumps and signals the child. Leaving the
                    // corpse in the map would leak one server per crash.
                    slot.client = None;
                    slot.crashes += 1;
                    slot.last_death = Some(why);
                }
            }
        }

        if slot.crashes >= MAX_CRASHES {
            return Err(ToolError::Failed(format!(
                "the {} language server has died {} times for this workspace, so Emma has \
                 stopped restarting it. The last time: {}",
                language.label,
                slot.crashes,
                slot.last_death.as_deref().unwrap_or("no reason recorded")
            )));
        }

        let server = server::resolve(language)?;
        match Client::start(&server, root).await {
            Ok(client) => {
                slot.client = Some(client.clone());
                Ok(client)
            }
            Err(e) => {
                // A start that fails counts against the ceiling too. Otherwise a
                // server that dies during the handshake is retried forever,
                // which is the same invisible-slow-tool failure by a different
                // route.
                slot.crashes += 1;
                slot.last_death = Some(e.detail().to_string());
                Err(e)
            }
        }
    }

    /// Put an already-connected client in the pool for `root`.
    ///
    /// The seam that lets the four tools be tested end to end — containment,
    /// the language gate, the rendering, the empty-result rule — against
    /// `Client::connect` and a fake, on a machine with no language server. Every
    /// one of those is a property of this crate rather than of rust-analyzer,
    /// and gating them behind an install is how they end up untested.
    ///
    /// Not only for tests: a harness that wants to warm a server during startup,
    /// rather than making the first tool call of a session pay for the index,
    /// starts one and adopts it here.
    pub async fn adopt(&self, root: &Path, client: Arc<Client>) {
        let language = client.server().language.key;
        let mut slots = self.slots.lock().await;
        slots.insert(
            (root.to_path_buf(), language),
            Slot {
                client: Some(client),
                crashes: 0,
                last_death: None,
            },
        );
    }

    /// Which server is answering for `root`, without starting one.
    ///
    /// For a `/lsp`-style status command: "is one running, and which binary" is
    /// a question worth being able to ask without paying for an index.
    pub async fn running(&self, root: &Path, language: &'static Language) -> Option<Server> {
        let slots = self.slots.lock().await;
        slots
            .get(&(root.to_path_buf(), language.key))
            .and_then(|s| s.client.as_ref())
            .map(|c| c.server().clone())
    }

    /// Every server currently running, whatever the root or language.
    ///
    /// The `/lsp`-status question once there are seven languages: "which of
    /// these is actually up" is no longer answerable one root at a time.
    pub async fn all_running(&self) -> Vec<Server> {
        let slots = self.slots.lock().await;
        let mut servers: Vec<Server> = slots
            .values()
            .filter_map(|s| s.client.as_ref())
            .map(|c| c.server().clone())
            .collect();
        servers.sort_by(|a, b| a.language.key.cmp(b.language.key));
        servers
    }

    /// Shut every server down politely, then drop it.
    ///
    /// Optional, and nothing depends on it — dropping the `Pool` is sufficient
    /// and is what actually guarantees the processes end. This exists for a
    /// harness that is exiting cleanly and can spare rust-analyzer the
    /// half-second to flush its cache.
    pub async fn shutdown(&self) {
        let clients: Vec<Arc<Client>> = {
            let mut slots = self.slots.lock().await;
            slots.drain().filter_map(|(_, s)| s.client).collect()
        };
        for client in clients {
            client.shutdown().await;
        }
    }
}

// endregion: The pool
