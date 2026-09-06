//! Rewriting stale tool traffic in the history before it is re-sent.
//!
//! **What this is for.** Every tool result Emma has ever received is re-sent on
//! every later model call, so a result's cost is its size multiplied by the
//! number of calls still to come. An earlier `Read` of a file that a later
//! `Read` has already replaced is paid for again on every remaining turn while
//! saying nothing the newer copy does not.
//!
//! **Off unless asked for, and the fork's number did not survive being
//! measured here.** The fork this is ported from claims 81.8% of byte-turns
//! across 25 recorded runs were carried by superseded `Read` results. Those
//! recordings were not in the archive, so the claim could only be re-derived
//! against a different corpus — which the `measurement` harness at the foot of
//! this file does, over the operator's own 156 session files, on 2026-08-26.
//! On that corpus superseded `Read` results carry **2.4%** of byte-turns:
//! 245,728 `tool_result` bytes of 10,104,760, over the 36 sessions that used
//! a tool at all. All three
//! transforms together reach 2.5%. The best single session was 5.1%; most were
//! 0.0%.
//!
//! Two corpora, two answers, and this one is not evidence that the fork's is
//! wrong — it is evidence that **the payoff depends entirely on what the
//! session did**. A run that reads one file eleven times to make four edits is
//! the shape the fork describes and the shape that pays; a run that greps, runs
//! a build and reads each file once is this corpus, and pruning it saves
//! almost nothing. So the flag ships off, and what would move it is a
//! measurement over sessions of the *first* kind, not a bigger sample of the
//! second. Re-run the harness against any corpus and it prints its own number.
//!
//! **The same run found a bug in the port, which is the argument for measuring
//! at all.** On the first pass, all three transforms together removed *fewer*
//! bytes than [`supersede_reads`] alone — 245,356 against 245,694.
//! [`collapse_failures`] replaces a repeated rejection with a sentence, and a
//! short rejection like `"denied"` is smaller than the sentence explaining it
//! was dropped, so the rewrite grew the request and lost the original text at
//! the same time. Neither the fork nor this port's first draft had a guard
//! against that, and no test could have found it: every fixture in the file
//! used a short result. [`claim_stub`] is the fix, and the same run now reports
//! 2.5% against 2.4%.
//!
//! **Why this rewrites and never deletes.** A `tool_result` whose
//! `tool_use_id` has no matching `tool_use` is rejected by Anthropic. So
//! dropping one side of a pair is not an option even when only one side holds
//! the bytes. Every function here mutates the *contents* of a block that is
//! already there. Nothing inserts a block, removes one, or reorders them, and
//! the transform takes `&mut [Message]` rather than a `Vec` so the type makes
//! that length invariant for it. That is what keeps a rewritten pair a pair.
//!
//! **This is the shape the provider itself uses.** Anthropic's server-side
//! context editing (`clear_tool_uses_20250919`) replaces a cleared tool result
//! with placeholder text and leaves the pairing intact, and its
//! `clear_tool_inputs` option clears the `tool_use` parameters the same way.
//! That is why [`fold_edits`] is willing to rewrite a `tool_use` input that may
//! sit beside a signed thinking block: the provider ships the same edit as a
//! feature. It is the one claim here taken from documentation rather than from
//! this tree, and it is the thing a live run should confirm first.
//!
//! **Nothing here touches the session log.** `Agent::conversation` stays
//! faithful to what was recorded, because `session::fold` of the log has to
//! equal it; only the outbound request is rewritten. Despite the module's name
//! nothing on disk is read, written or removed — there is no `std::fs` in this
//! file.
//!
//! # Deviations from the fork
//!
//! One, and it is a correctness fix rather than a port artefact. The fork
//! modelled a `Read` with no `limit` as reaching infinity, so an unbounded
//! re-read from line 1 was treated as covering *any* earlier read of the same
//! file. Live `Read` shows at most [`MAX_LINES`] lines whatever the arguments
//! say, so an unbounded read of a 5,000-line file stops at 2,000 — and stubbing
//! an earlier `offset: 3000` read against it would delete the only copy of
//! lines 3000-3020 from the conversation while claiming a newer one exists.
//! [`Window`] is therefore bounded on both ends, with the cap read from the tool
//! itself so the two cannot drift.

use std::collections::HashMap;

use emma_llm::{Content, ContentBlock, Message};
use emma_tools_fs::read::MAX_LINES;
use serde_json::{json, Value};

use crate::settings::Settings;

/// The two tool names this module knows how to reason about.
///
/// Spelled here rather than imported because `read::NAME` and `edit::NAME` are
/// private to their modules. A rename in `tools/fs` therefore does not break
/// this file — it silently stops it matching, which costs bytes and never
/// correctness, and is the direction a mismatch should fail in.
const READ: &str = "Read";
const EDIT: &str = "Edit";

/// What one call to [`prune`] did, so a run is never quietly reshaped.
///
/// A transform that changes what the model is shown and reports nothing is the
/// shape this project calls a plausible success: the run gets worse and no test
/// goes red. The counts are what a log line or a status note quotes; the byte
/// figures are this repository's own measurement of the saving, taken from the
/// rendered wire content rather than from the length of the strings inside it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// `Read` results whose body was replaced by a pointer to the newer read.
    pub stale_reads: usize,
    /// Completed `Edit` calls whose `old_string`/`new_string` became a count.
    pub folded_edits: usize,
    /// Repeated identical failures replaced by a one-line note.
    pub repeated_failures: usize,
    /// Wire bytes of the whole message list before the rewrite.
    pub bytes_before: usize,
    /// Wire bytes after it. A stub can no longer grow a result — `claim_stub`
    /// refuses that — but a folded `Edit` whose `old_string` was two characters
    /// renders a longer note than it replaced, so this is not asserted to be
    /// the smaller number and [`Report::saved`] saturates.
    pub bytes_after: usize,
}

impl Report {
    /// Did this change the request at all? A `false` here is the only honest
    /// thing to say when the gate is off.
    pub fn changed(&self) -> bool {
        self.stale_reads + self.folded_edits + self.repeated_failures > 0
    }

    /// Bytes removed, saturating so a rewrite that grew the request reports
    /// nothing saved rather than an enormous number.
    pub fn saved(&self) -> usize {
        self.bytes_before.saturating_sub(self.bytes_after)
    }

    /// One line naming what was shaped and by how much, or `None` when nothing
    /// was.
    ///
    /// A sentence rather than a struct dump because the reader is a person
    /// scanning a log for "why does the model not remember that file", and the
    /// answer to that question is this line.
    pub fn note(&self) -> Option<String> {
        if !self.changed() {
            return None;
        }
        Some(format!(
            "history shaped before sending: {} stale Read result(s), {} folded Edit call(s), \
             {} repeated failure(s); {} of {} wire bytes removed",
            self.stale_reads,
            self.folded_edits,
            self.repeated_failures,
            self.saved(),
            self.bytes_before,
        ))
    }
}

/// Rewrite stale tool traffic in place, if the user has asked for it.
///
/// **This is the only way in, and the gate is inside it on purpose.** An
/// ungated mechanism exported beside a gate is a mechanism that eventually gets
/// called without one, and the failure mode of that is not a crash — it is a
/// run that answers differently and reports success. `prune_history` is
/// `Option<bool>` and **absent means off**: a settings file written by a build
/// that did not know the word must not read as consent.
///
/// The length of `messages`, the length of every message's block list, and the
/// order and `type` of every block are all unchanged. Only the `content` of a
/// `tool_result` and the `input` of a `tool_use` are ever written.
pub fn prune(settings: &Settings, messages: &mut [Message]) -> Report {
    if settings.prune_history != Some(true) {
        return Report::default();
    }

    let calls = collect(messages);
    let mut actions: HashMap<Site, Action> = HashMap::new();
    // Sequential rather than a struct literal because the order is load-bearing:
    // the three share one `actions` map and the first to claim a site keeps it.
    let stale_reads = supersede_reads(&calls, &mut actions);
    let folded_edits = fold_edits(&calls, &mut actions);
    let repeated_failures = collapse_failures(&calls, &mut actions);
    let mut report = Report {
        stale_reads,
        folded_edits,
        repeated_failures,
        ..Report::default()
    };

    if actions.is_empty() {
        return report;
    }
    // Measured only when something is actually going to change: rendering the
    // whole conversation twice is the cost of the receipt, and a run that
    // pruned nothing should not pay it.
    report.bytes_before = wire_len(messages);
    apply(messages, &actions);
    report.bytes_after = wire_len(messages);
    report
}

/// Wire bytes of the whole list, which is what the provider is charged for —
/// quotes and escapes included, because those are tokenised too.
fn wire_len(messages: &[Message]) -> usize {
    messages.iter().map(|m| m.content.wire_len()).sum()
}

/// Where a block lives: which message, which block inside it.
type Site = (usize, usize);

#[derive(Debug, Clone, PartialEq)]
enum Action {
    /// Overwrite a `tool_result`'s `content`.
    Stub(String),
    /// Overwrite a `tool_use`'s `input`.
    Fold(Value),
}

// region: reading the history
// ---------------------------------------------------------------------------
// Reading the history
//
// One pass to index results by the call they answer, one to walk the calls.
// Everything after this point works on `Call`s and never looks at a `Message`
// again until `apply`.
// ---------------------------------------------------------------------------

/// One tool call, with its result located and its arguments already reduced to
/// the little the transforms below need.
///
/// The arguments are parsed here rather than carried as a `Value` so a large
/// `old_string` is never cloned: this runs once per model call over the whole
/// conversation, which is exactly the list whose size is the problem.
#[derive(Debug)]
struct Call {
    name: String,
    /// The assistant message and block holding the `tool_use`.
    use_site: Site,
    /// The user message and block holding the matching `tool_result`, if the
    /// result is present. A call whose result has not arrived yet is the live
    /// one and is never touched.
    result_site: Option<Site>,
    result_is_error: bool,
    result_content: String,
    /// 1-based count of assistant messages holding tool calls, up to and
    /// including this one.
    ///
    /// Called a "turn" to the model because that is the unit someone reading
    /// the transcript counts in. It is derived from the message list rather
    /// than read from the session log so this stays a pure function of its
    /// input, which is what makes it testable without a session.
    turn: usize,
    args: Args,
}

#[derive(Debug, PartialEq)]
enum Args {
    Read {
        path: String,
        window: Window,
    },
    Edit {
        path: String,
        plus: usize,
        minus: usize,
    },
    Other,
}

/// The lines a `Read` actually returned, as a half-open interval of 1-based
/// line numbers.
///
/// **Both ends are bounded, and that is the port's one deviation from the
/// fork.** `read.rs` shows at most [`MAX_LINES`] lines whatever `limit` says —
/// an absent `limit` becomes the cap and a larger one is clamped down to it —
/// so a read with no `limit` is not "as far as it goes", it is "up to 2,000
/// lines from `offset`". Modelling it as unbounded made every unbounded read
/// cover every earlier read of the same file, including one that started past
/// the cap, which would stub the only copy of lines nothing newer contains.
///
/// The cap is imported from the tool rather than written here so the two cannot
/// drift; if `read.rs` raises it, this widens with it on the next build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    start: u64,
    /// Exclusive.
    end: u64,
}

impl Window {
    /// Does this window contain every line the other one returned?
    ///
    /// Plain interval containment, which is only this simple because the
    /// constructor already resolved the cap. Equality counts: two whole-file
    /// reads produce the same window and the later one supersedes the earlier.
    fn covers(&self, other: &Window) -> bool {
        self.start <= other.start && self.end >= other.end
    }
}

fn collect(messages: &[Message]) -> Vec<Call> {
    // Results are indexed first because a call is only interesting once its
    // answer is in the list, and the answer follows the call.
    let mut results: HashMap<&str, (Site, bool, &str)> = HashMap::new();
    for (m, message) in messages.iter().enumerate() {
        for (b, block) in message.content.blocks().iter().enumerate() {
            if let ContentBlock::ToolResult(r) = block {
                results.insert(r.tool_use_id.as_str(), ((m, b), r.is_error, &r.content));
            }
        }
    }

    let mut calls = Vec::new();
    let mut turn = 0;
    for (m, message) in messages.iter().enumerate() {
        let mut counted = false;
        for (b, block) in message.content.blocks().iter().enumerate() {
            let ContentBlock::ToolUse(c) = block else {
                continue;
            };
            if !counted {
                turn += 1;
                counted = true;
            }
            let found = results.get(c.id.as_str());
            calls.push(Call {
                name: c.name.clone(),
                use_site: (m, b),
                result_site: found.map(|(site, _, _)| *site),
                result_is_error: found.is_some_and(|(_, e, _)| *e),
                result_content: found.map(|(_, _, s)| (*s).to_string()).unwrap_or_default(),
                turn,
                args: args_of(&c.name, &c.input),
            });
        }
    }
    calls
}

fn args_of(name: &str, input: &Value) -> Args {
    let path = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match name {
        READ => {
            let Some(path) = path("file_path") else {
                return Args::Other;
            };
            // `offset` is 1-based with a schema minimum of 1, and absent means
            // the top of the file.
            let start = input
                .get("offset")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .max(1);
            // `read.rs`: `limit` lowers the cap and cannot raise it, and an
            // absent `limit` *is* the cap. Both spellings resolve to the same
            // arithmetic here for the reason in [`Window`]'s doc.
            let span = input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(MAX_LINES as u64)
                .min(MAX_LINES as u64);
            Args::Read {
                path,
                window: Window {
                    start,
                    end: start.saturating_add(span),
                },
            }
        }
        EDIT => {
            let Some(path) = path("file_path") else {
                return Args::Other;
            };
            let plus = count_lines(input.get("new_string").and_then(Value::as_str));
            // `old_string` and `lines` are alternatives, never both: the schema
            // says "Give this or lines, not both" and `edit.rs` rejects the
            // pair. Whichever is present is how many lines went away.
            let minus = match input.get("lines").and_then(Value::as_str) {
                Some(spec) => addressed_lines(spec),
                None => count_lines(input.get("old_string").and_then(Value::as_str)),
            };
            Args::Edit { path, plus, minus }
        }
        _ => Args::Other,
    }
}

fn count_lines(s: Option<&str>) -> usize {
    match s {
        None | Some("") => 0,
        Some(s) => s.lines().count(),
    }
}

/// How many lines a `lines` address covers.
///
/// The spelling is Read's: `"12#a3f9"` for one line, `"12#a3f9-15#b7c1"` for an
/// inclusive range. Only the leading integers matter here, and a spelling this
/// cannot parse reports 0 rather than guessing, because the number ends up in a
/// sentence the model reads and a wrong count there is worse than no count.
fn addressed_lines(spec: &str) -> usize {
    let number = |part: &str| {
        part.split('#')
            .next()
            .and_then(|n| n.trim().parse::<u64>().ok())
    };
    let mut parts = spec.split('-');
    let Some(first) = parts.next().and_then(number) else {
        return 0;
    };
    match parts.next().and_then(number) {
        Some(last) if last >= first => (last - first + 1) as usize,
        Some(_) => 0,
        None => 1,
    }
}

// endregion: reading the history

// region: the three transforms
// ---------------------------------------------------------------------------
// The three transforms
//
// Each decides sites and returns how many it claimed; none writes to a message.
// They share one `actions` map and each claims a site only if it is vacant, so
// the first transform to claim a site keeps it and no site is rewritten twice.
// ---------------------------------------------------------------------------

/// Claim a `tool_result` site for a stub, unless the stub costs more than the
/// text it would replace.
///
/// **The guard is here because a measurement found it.** Over the operator's
/// session corpus the three transforms together removed *fewer* bytes than
/// [`supersede_reads`] alone, and the difference was [`collapse_failures`]
/// replacing rejections like `"denied"` with the sentence that explains they
/// were dropped. A rewrite that grows the request and loses the original text
/// is worse than no rewrite on both counts, so it does not happen — and the
/// site stays vacant for a later transform that might do better with it.
///
/// **The occupied arm is unreachable with well-formed input, and that is a
/// finding rather than a claim.** Mutating it to overwrite instead of refuse
/// survived a mutation run against the whole file: [`supersede_reads`] only
/// ever claims successful results and [`collapse_failures`] only failed ones,
/// so the two never contend for a site, and within either transform each call
/// has one result of its own. It stays because the alternative is
/// `unreachable!()` in a function whose only job is to be careful, and because
/// two `tool_use` blocks sharing one id would make it reachable at once.
fn claim_stub(
    actions: &mut HashMap<Site, Action>,
    site: Site,
    replacing: &str,
    stub: String,
) -> bool {
    if stub.len() >= replacing.len() {
        return false;
    }
    match actions.entry(site) {
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(Action::Stub(stub));
            true
        }
        std::collections::hash_map::Entry::Occupied(_) => false,
    }
}

/// Replace the body of a `Read` result that a later `Read` has already
/// replaced.
///
/// **What "covers" means, and why it is this conservative.** A later read
/// supersedes an earlier one only when it is on the same `file_path` *and* the
/// lines it returned contain every line the earlier one returned. A re-read of
/// lines 40-60 does not supersede a whole-file read, because the earlier result
/// is still the only copy of lines 1-39.
///
/// That rule is stricter than "same path wins", and the reason is Edit's
/// `lines` argument rather than politeness. Read prints every line with a hash
/// label and Edit accepts `"12#a3f9-15#b7c1"` in place of `old_string`, which
/// is what lets a model edit without re-reading. Stubbing a wide read because a
/// narrow one followed it would take away the labels for every line the narrow
/// read did not return, and the next edit would fail on an anchor no longer in
/// context. A covering read does not have that problem: the result that stays
/// carries a current label for every line the stubbed one did.
///
/// Paths are compared as written, after trimming. Nothing here canonicalises,
/// because canonicalising means touching the filesystem and this is a pure
/// function of the message list. Two spellings of one file therefore fail to
/// match and simply do not supersede, which costs bytes and never correctness.
///
/// Failed reads are left alone. [`collapse_failures`] owns those, and a read
/// that errored has no body worth taking anyway.
fn supersede_reads(calls: &[Call], actions: &mut HashMap<Site, Action>) -> usize {
    let reads: Vec<(&Call, &String, &Window)> = calls
        .iter()
        .filter(|c| !c.result_is_error && c.result_site.is_some())
        .filter_map(|c| match &c.args {
            Args::Read { path, window } => Some((c, path, window)),
            _ => None,
        })
        .collect();

    let mut claimed = 0;
    for (i, (call, path, window)) in reads.iter().enumerate() {
        let superseder = reads[i + 1..]
            .iter()
            .find(|(_, later_path, later)| later_path == path && later.covers(window));
        let Some((later, _, _)) = superseder else {
            continue;
        };
        let Some(site) = call.result_site else {
            continue;
        };
        let turn = later.turn;
        let stub = format!(
            "[stale Read of {path}: re-read at turn {turn}, and that newer result is the current \
             one. This copy was dropped to save context. Read the file again if you need it.]"
        );
        if claim_stub(actions, site, &call.result_content, stub) {
            claimed += 1;
        }
    }
    claimed
}

/// Replace the `old_string`/`new_string` of a finished `Edit` with a count.
///
/// A successful edit's arguments describe a change that has already happened.
/// The file on disk is the authority on what it says now, and a model that
/// needs the text can read it, so carrying both halves of every earlier edit
/// for the rest of the session buys nothing.
///
/// **Only the call is folded, never the result.** Edit echoes its replacement
/// lines back with fresh hash labels, up to `edit::ECHO_LIMIT`, precisely so
/// the *next* edit does not need a re-read. Folding the result would undo the
/// thing that echo exists to do and would cost a `Read` per edit, which is the
/// traffic this module is trying to remove.
///
/// **The most recent successful edit per file is kept whole.** It is the one
/// the model is most likely to still be reasoning about, and keeping it matches
/// what the other two transforms do: each keeps the newest instance verbatim
/// and takes the ones behind it. Dropping that exception is a one-line change
/// if a live run says the bytes are worth more than the recency.
fn fold_edits(calls: &[Call], actions: &mut HashMap<Site, Action>) -> usize {
    let edits: Vec<(&Call, &String, usize, usize)> = calls
        .iter()
        .filter(|c| !c.result_is_error && c.result_site.is_some())
        .filter_map(|c| match &c.args {
            Args::Edit { path, plus, minus } => Some((c, path, *plus, *minus)),
            _ => None,
        })
        .collect();

    let mut claimed = 0;
    for (i, (call, path, plus, minus)) in edits.iter().enumerate() {
        if !edits[i + 1..].iter().any(|(_, later, _, _)| later == path) {
            continue;
        }
        if let std::collections::hash_map::Entry::Vacant(slot) = actions.entry(call.use_site) {
            slot.insert(Action::Fold(json!({
                "file_path": path,
                "note": format!(
                    "edit applied at turn {}: +{plus}/-{minus} lines. The old and new text was \
                     dropped to save context; the file on disk is authoritative.",
                    call.turn
                ),
            })));
            claimed += 1;
        }
    }
    claimed
}

/// Keep the last of a repeated rejection and stub the ones before it.
///
/// A model that is stuck repeats a call and collects the same refusal several
/// times. The last one is what it is currently looking at and stays verbatim;
/// the earlier copies say nothing the last one does not, and their only
/// remaining job is to show this has happened before, which one sentence does
/// as well as the whole message.
///
/// "Identical" is exact equality of the tool name and the result text, so a
/// rejection carrying a timestamp or a changing line number is left alone. That
/// is the conservative direction: a failure that differs may differ for a
/// reason the model needs.
fn collapse_failures(calls: &[Call], actions: &mut HashMap<Site, Action>) -> usize {
    let failures: Vec<&Call> = calls
        .iter()
        .filter(|c| c.result_is_error && c.result_site.is_some())
        .collect();

    let mut claimed = 0;
    for (i, call) in failures.iter().enumerate() {
        let repeated = failures[i + 1..]
            .iter()
            .any(|later| later.name == call.name && later.result_content == call.result_content);
        if !repeated {
            continue;
        }
        let Some(site) = call.result_site else {
            continue;
        };
        let name = &call.name;
        let stub = format!(
            "[repeated {name} failure: this call failed the same way again later, and the last \
             of those is quoted in full below. This copy was dropped to save context.]"
        );
        if claim_stub(actions, site, &call.result_content, stub) {
            claimed += 1;
        }
    }
    claimed
}

// endregion: the three transforms

/// Write the decided edits back.
///
/// A `Stub` only ever lands on a `tool_result` and a `Fold` only ever on a
/// `tool_use`, because that is how the sites were collected. The mismatched
/// arms do nothing rather than panic: a transform that corrupted a block would
/// be worse than one that skipped it, and no input reaches them.
fn apply(messages: &mut [Message], actions: &HashMap<Site, Action>) {
    for (m, message) in messages.iter_mut().enumerate() {
        let Content::Blocks(blocks) = &mut message.content else {
            continue;
        };
        for (b, block) in blocks.iter_mut().enumerate() {
            match (actions.get(&(m, b)), block) {
                (Some(Action::Stub(text)), ContentBlock::ToolResult(r)) => {
                    r.content = text.clone();
                    // The pictures go with the prose. A stub exists to take a
                    // superseded result's bytes out of the request, and a
                    // screenshot result is almost entirely its image: leaving
                    // `images` populated would replace a few hundred bytes of
                    // text and still ship a few hundred kilobytes of base64,
                    // which is the pruning reported and not the pruning done.
                    r.images.clear();
                }
                (Some(Action::Fold(input)), ContentBlock::ToolUse(c)) => {
                    c.input = input.clone();
                }
                _ => {}
            }
        }
    }
}

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The whole module is pure, so all of it is testable here. Two invariants are
// asserted after *every* transform rather than once in a test of their own,
// because a transform that broke either would still produce plausible text and
// a plausible-looking conversation: the block list keeps its shape, and every
// `tool_result` still has the `tool_use` it answers — checked on the rendered
// JSON, which is the thing the API actually pairs.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use emma_llm::{Role, ThinkingBlock, ToolCall, ToolResult};

    /// Settings saying exactly one thing about pruning.
    ///
    /// Assignment rather than a functional-update literal because `Settings`
    /// has a private field (`legacy_model`, the pre-provider model spelling)
    /// and `..Default::default()` cannot name it from another module.
    #[allow(clippy::field_reassign_with_default)]
    fn settings(prune_history: Option<bool>) -> Settings {
        let mut s = Settings::default();
        s.prune_history = prune_history;
        s
    }

    /// The gate open. Every test that expects a rewrite uses this; a test that
    /// expects nothing uses `Settings::default()`, which is the shipped state.
    fn on() -> Settings {
        settings(Some(true))
    }

    fn call(id: &str, name: &str, input: Value) -> ContentBlock {
        ContentBlock::ToolUse(ToolCall {
            id: id.into(),
            name: name.into(),
            input,
            ..ToolCall::default()
        })
    }

    /// A tool result the size a real one is.
    ///
    /// The stub that replaces a result is a sentence of about 150 bytes, and
    /// `claim_stub` refuses to grow the request — so a two-word fixture would
    /// exercise the guard rather than the transform it was written for, and
    /// every assertion about superseding would silently become an assertion
    /// about byte counts.
    fn long(label: &str) -> String {
        format!(
            "{label}
{}",
            "x".repeat(400)
        )
    }

    fn ok(id: &str, content: &str) -> ContentBlock {
        ContentBlock::ToolResult(ToolResult::ok(id, content))
    }

    fn failed(id: &str, content: &str) -> ContentBlock {
        ContentBlock::ToolResult(ToolResult::failed(id, content))
    }

    fn read(id: &str, path: &str, offset: Option<u64>, limit: Option<u64>) -> ContentBlock {
        let mut input = json!({ "file_path": path });
        if let Some(o) = offset {
            input["offset"] = json!(o);
        }
        if let Some(l) = limit {
            input["limit"] = json!(l);
        }
        call(id, "Read", input)
    }

    fn edit(id: &str, path: &str, old: &str, new: &str) -> ContentBlock {
        call(
            id,
            "Edit",
            json!({ "file_path": path, "old_string": old, "new_string": new }),
        )
    }

    fn results(blocks: Vec<ContentBlock>) -> Message {
        Message {
            role: Role::User,
            content: Content::Blocks(blocks),
        }
    }

    /// One tool call and its answer, as the two messages the loop appends.
    fn exchange(use_block: ContentBlock, result: ContentBlock) -> Vec<Message> {
        vec![Message::assistant(vec![use_block]), results(vec![result])]
    }

    fn result_texts(messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .flat_map(|m| m.content.blocks())
            .filter_map(|b| match b {
                ContentBlock::ToolResult(r) => Some(r.content.clone()),
                _ => None,
            })
            .collect()
    }

    fn inputs(messages: &[Message]) -> Vec<Value> {
        messages
            .iter()
            .flat_map(|m| m.content.blocks())
            .filter_map(|b| match b {
                ContentBlock::ToolUse(c) => Some(c.input.clone()),
                _ => None,
            })
            .collect()
    }

    /// Every `tool_result` has a `tool_use` with its id, before it — **on the
    /// wire**, not in the enum.
    ///
    /// This is the constraint the whole module exists to respect: Anthropic
    /// rejects an unpaired `tool_result`. It walks the rendered JSON rather
    /// than the typed blocks on the same reasoning that made
    /// `content.rs`'s `caller` bug expensive — what the API sees is the bytes,
    /// and a check that only ever looks at the Rust side cannot notice a block
    /// that renders as something else.
    fn assert_paired(messages: &[Message]) {
        let rendered = serde_json::to_value(messages).expect("messages must render");
        let mut seen: Vec<String> = Vec::new();
        let mut results = 0;
        for message in rendered.as_array().expect("a message list") {
            let Some(blocks) = message["content"].as_array() else {
                continue; // a plain-string message carries no tool traffic
            };
            for block in blocks {
                match block["type"].as_str() {
                    Some("tool_use") => {
                        let id = block["id"].as_str().expect("a tool_use renders an id");
                        assert!(
                            block["name"].as_str().is_some_and(|n| !n.is_empty()),
                            "a tool_use lost its name: {block}"
                        );
                        seen.push(id.to_string());
                    }
                    Some("tool_result") => {
                        let id = block["tool_use_id"]
                            .as_str()
                            .expect("a tool_result renders a tool_use_id");
                        assert!(
                            seen.iter().any(|s| s == id),
                            "tool_result {id} has no preceding tool_use: {rendered}"
                        );
                        results += 1;
                    }
                    _ => {}
                }
            }
        }
        // Without this the whole function passes vacuously over a conversation
        // whose results all vanished, which is the failure it exists to catch.
        assert_eq!(
            results,
            messages
                .iter()
                .flat_map(|m| m.content.blocks())
                .filter(|b| matches!(b, ContentBlock::ToolResult(_)))
                .count(),
            "a tool_result did not survive rendering"
        );
    }

    /// The shape of the list is untouched: same messages, same blocks, same
    /// order, same block types.
    fn assert_shape_held(before: &[Message], after: &[Message]) {
        assert_eq!(before.len(), after.len(), "a message was added or removed");
        for (b, a) in before.iter().zip(after) {
            assert_eq!(b.role, a.role, "a role changed");
            let (bb, ab) = (b.content.blocks(), a.content.blocks());
            assert_eq!(bb.len(), ab.len(), "a block was added or removed");
            for (x, y) in bb.iter().zip(ab) {
                assert_eq!(
                    std::mem::discriminant(x),
                    std::mem::discriminant(y),
                    "a block changed type"
                );
            }
        }
    }

    // -- the gate ------------------------------------------------------------

    #[test]
    fn the_default_settings_prune_nothing() {
        // The shipped state. Two whole-file reads of one file is the case every
        // other test in this file expects to be rewritten, so a build that
        // started pruning silently would fail here and nowhere else.
        let mut m = exchange(read("a", "f.rs", None, None), ok("a", &long("FIRST BODY")));
        m.extend(exchange(
            read("b", "f.rs", None, None),
            ok("b", &long("SECOND")),
        ));
        let before = m.clone();

        let report = prune(&Settings::default(), &mut m);

        assert_eq!(
            m, before,
            "an unasked-for build must send what it was given"
        );
        assert_eq!(report, Report::default());
        assert!(!report.changed());
        assert_eq!(
            report.note(),
            None,
            "and says nothing, because it did nothing"
        );
    }

    #[test]
    fn an_explicit_false_prunes_nothing_either() {
        let mut m = exchange(read("a", "f.rs", None, None), ok("a", &long("FIRST BODY")));
        m.extend(exchange(
            read("b", "f.rs", None, None),
            ok("b", &long("SECOND")),
        ));
        let before = m.clone();

        let report = prune(&settings(Some(false)), &mut m);

        assert_eq!(m, before);
        assert!(!report.changed());
    }

    #[test]
    fn the_same_history_with_the_gate_open_is_rewritten() {
        // The pair to the two tests above: without this one they pass over a
        // `prune` that does nothing at all, and prove only that nothing works.
        let mut m = exchange(read("a", "f.rs", None, None), ok("a", &long("FIRST BODY")));
        m.extend(exchange(
            read("b", "f.rs", None, None),
            ok("b", &long("SECOND")),
        ));

        let report = prune(&on(), &mut m);

        assert!(result_texts(&m)[0].starts_with("[stale Read"));
        assert_eq!(report.stale_reads, 1);
        assert!(report.changed());
    }

    // -- the receipt ---------------------------------------------------------

    #[test]
    fn the_report_counts_each_transform_and_measures_the_saving() {
        let body = "X".repeat(4000);
        let mut m = exchange(read("a", "f.rs", None, None), ok("a", &body));
        m.extend(exchange(
            read("b", "f.rs", None, None),
            ok("b", &long("SECOND")),
        ));
        m.extend(exchange(
            edit("e1", "f.rs", "one\ntwo", "1"),
            ok("e1", "edited"),
        ));
        m.extend(exchange(edit("e2", "f.rs", "1", "2"), ok("e2", "edited")));
        m.extend(exchange(
            call("x1", "Bash", json!({})),
            failed("x1", &long("denied")),
        ));
        m.extend(exchange(
            call("x2", "Bash", json!({})),
            failed("x2", &long("denied")),
        ));

        let report = prune(&on(), &mut m);

        assert_eq!(report.stale_reads, 1);
        assert_eq!(report.folded_edits, 1);
        assert_eq!(report.repeated_failures, 1);
        assert!(
            report.saved() > 3000,
            "a 4,000-byte body was replaced by a sentence: {report:?}"
        );
        assert_eq!(
            report.bytes_after,
            wire_len(&m),
            "the reported figure is the list that is about to be sent"
        );
        let note = report
            .note()
            .expect("something happened, so there is a line");
        assert!(note.contains("1 stale Read result(s)"), "{note}");
        assert!(note.contains("wire bytes removed"), "{note}");
        assert_paired(&m);
    }

    #[test]
    fn a_history_with_nothing_to_shape_reports_nothing_and_measures_nothing() {
        let mut m = exchange(read("a", "f.rs", None, None), ok("a", "BODY"));

        let report = prune(&on(), &mut m);

        assert!(!report.changed());
        assert_eq!(
            report.bytes_before, 0,
            "the receipt costs two renders of the conversation; a no-op does not pay it"
        );
    }

    // -- transform 1: superseding stale reads --------------------------------

    #[test]
    fn a_whole_file_re_read_supersedes_the_earlier_whole_file_read() {
        let mut m = exchange(
            read("a", "src/lib.rs", None, None),
            ok("a", &long("FIRST BODY")),
        );
        m.extend(exchange(
            read("b", "src/lib.rs", None, None),
            ok("b", &long("SECOND BODY")),
        ));
        let before = m.clone();

        prune(&on(), &mut m);

        let texts = result_texts(&m);
        assert!(
            texts[0].starts_with("[stale Read of src/lib.rs"),
            "{texts:?}"
        );
        assert!(
            texts[0].contains("turn 2"),
            "the stub names the turn that superseded it: {texts:?}"
        );
        assert_eq!(
            texts[1],
            long("SECOND BODY"),
            "the newest read is kept whole"
        );
        assert_shape_held(&before, &m);
        assert_paired(&m);
    }

    #[test]
    fn a_narrow_re_read_does_not_supersede_a_wide_one() {
        let mut m = exchange(
            read("a", "src/lib.rs", None, None),
            ok("a", &long("WHOLE FILE")),
        );
        m.extend(exchange(
            read("b", "src/lib.rs", Some(40), Some(20)),
            ok("b", &long("LINES 40 TO 59")),
        ));

        prune(&on(), &mut m);

        assert_eq!(
            result_texts(&m),
            vec![long("WHOLE FILE"), long("LINES 40 TO 59")],
            "a re-read of part of a file leaves the full read the only copy of the rest, and \
             the only source of hash labels for it"
        );
        assert_paired(&m);
    }

    #[test]
    fn a_re_read_further_down_the_file_does_not_supersede_one_from_the_top() {
        // **The case only the `start` half of `covers` catches, and it survived
        // a mutation run before this test existed.** Both reads are uncapped,
        // so the later one ends MAX_LINES lines further down and its *end*
        // alone says it covers the earlier — while it never returned lines 1-39
        // at all. Take `self.start <= other.start` out of `covers` and this is
        // the only test that notices.
        let mut m = exchange(read("a", "f.rs", None, None), ok("a", &long("FROM LINE 1")));
        m.extend(exchange(
            read("b", "f.rs", Some(40), None),
            ok("b", &long("FROM LINE 40")),
        ));

        prune(&on(), &mut m);

        assert_eq!(
            result_texts(&m)[0],
            long("FROM LINE 1"),
            "the later read starts at 40, so lines 1-39 exist only in the earlier result"
        );
        assert_paired(&m);
    }

    #[test]
    fn a_capped_re_read_from_the_top_does_not_supersede_an_uncapped_one() {
        // The case the `start` comparison alone does not catch: both reads
        // begin at line 1, so only the end rule can tell that the second one
        // stopped short of where the first one got to.
        let mut m = exchange(
            read("a", "f.rs", None, None),
            ok("a", &long("ALL 900 LINES")),
        );
        m.extend(exchange(
            read("b", "f.rs", Some(1), Some(50)),
            ok("b", &long("FIRST 50 LINES")),
        ));

        prune(&on(), &mut m);

        assert_eq!(
            result_texts(&m),
            vec![long("ALL 900 LINES"), long("FIRST 50 LINES")],
            "a capped re-read is not a replacement for an uncapped one"
        );
        assert_paired(&m);
    }

    #[test]
    fn an_uncapped_re_read_does_not_supersede_a_read_that_started_past_the_cap() {
        // **The fork's bug, and the reason `Window` deviates from it.** There
        // it modelled an absent `limit` as infinity, so this second read
        // covered everything and the first result was stubbed — deleting the
        // conversation's only copy of lines 3000-3019 while telling the model a
        // newer one existed. `read.rs` stops at MAX_LINES, so it does not.
        let past_cap = MAX_LINES as u64 + 1000;
        let mut m = exchange(
            read("a", "f.rs", Some(past_cap), Some(20)),
            ok("a", &long("THE ONLY COPY OF THE TAIL")),
        );
        m.extend(exchange(
            read("b", "f.rs", None, None),
            ok("b", &long("THE FIRST 2000 LINES")),
        ));

        prune(&on(), &mut m);

        assert_eq!(
            result_texts(&m)[0],
            long("THE ONLY COPY OF THE TAIL"),
            "an unbounded Read shows at most MAX_LINES lines, so it cannot replace a read that \
             began beyond them"
        );
        assert_paired(&m);
    }

    #[test]
    fn a_wide_re_read_supersedes_an_earlier_narrow_one() {
        let mut m = exchange(
            read("a", "src/lib.rs", Some(40), Some(20)),
            ok("a", &long("LINES 40 TO 59")),
        );
        m.extend(exchange(
            read("b", "src/lib.rs", Some(1), Some(2000)),
            ok("b", &long("WHOLE FILE")),
        ));

        prune(&on(), &mut m);

        let texts = result_texts(&m);
        assert!(texts[0].starts_with("[stale Read"), "{texts:?}");
        assert_eq!(texts[1], long("WHOLE FILE"));
        assert_paired(&m);
    }

    #[test]
    fn a_read_of_another_file_supersedes_nothing() {
        let mut m = exchange(read("a", "src/lib.rs", None, None), ok("a", &long("LIB")));
        m.extend(exchange(
            read("b", "src/main.rs", None, None),
            ok("b", &long("MAIN")),
        ));

        prune(&on(), &mut m);

        assert_eq!(result_texts(&m), vec![long("LIB"), long("MAIN")]);
        assert_paired(&m);
    }

    #[test]
    fn a_superseded_pair_is_still_a_pair_with_its_id_and_name_intact() {
        let mut m = exchange(read("a", "f.rs", None, None), ok("a", &long("FIRST")));
        m.extend(exchange(
            read("b", "f.rs", None, None),
            ok("b", &long("SECOND")),
        ));
        let before = m.clone();

        prune(&on(), &mut m);

        assert_shape_held(&before, &m);
        let blocks: Vec<&ContentBlock> = m.iter().flat_map(|x| x.content.blocks()).collect();
        match (blocks[0], blocks[1]) {
            (ContentBlock::ToolUse(c), ContentBlock::ToolResult(r)) => {
                assert_eq!(c.id, "a");
                assert_eq!(c.name, "Read", "the call keeps its name");
                assert_eq!(r.tool_use_id, "a", "the result still points at its call");
                assert_eq!(
                    c.input["file_path"], "f.rs",
                    "a stubbed read keeps its arguments: the call is not where the bytes are"
                );
            }
            _ => panic!("the block order changed: {blocks:?}"),
        }
        assert_paired(&m);
    }

    #[test]
    fn the_last_read_of_a_file_is_never_stubbed() {
        let mut m = Vec::new();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            m.extend(exchange(
                read(id, "f.rs", None, None),
                ok(id, &long(&format!("BODY {i}"))),
            ));
        }

        prune(&on(), &mut m);

        let texts = result_texts(&m);
        assert!(texts[0].starts_with("[stale Read"));
        assert!(texts[1].starts_with("[stale Read"));
        assert_eq!(
            texts[2],
            long("BODY 2"),
            "whatever superseded means, the survivor is the current one"
        );
        assert_paired(&m);
    }

    #[test]
    fn a_call_still_waiting_for_its_result_is_left_alone() {
        let mut m = exchange(read("a", "f.rs", None, None), ok("a", &long("FIRST")));
        // The second read has been made and not yet answered, which is the
        // state the loop is in when it builds the request that carries it.
        m.push(Message::assistant(vec![read("b", "f.rs", None, None)]));

        prune(&on(), &mut m);

        assert_eq!(
            result_texts(&m),
            vec![long("FIRST")],
            "an unanswered read cannot supersede anything: its body does not exist yet"
        );
        assert_paired(&m);
    }

    #[test]
    fn a_failed_read_is_not_treated_as_superseding() {
        let mut m = exchange(read("a", "f.rs", None, None), ok("a", &long("BODY")));
        m.extend(exchange(
            read("b", "f.rs", None, None),
            failed("b", &long("no such file")),
        ));

        prune(&on(), &mut m);

        assert_eq!(
            result_texts(&m),
            vec![long("BODY"), long("no such file")],
            "a read that failed replaced nothing"
        );
        assert_paired(&m);
    }

    #[test]
    fn window_coverage_rules() {
        let cap = MAX_LINES as u64;
        let whole = Window {
            start: 1,
            end: 1 + cap,
        };
        let tail = Window {
            start: 40,
            end: 40 + cap,
        };
        let narrow = Window { start: 40, end: 60 };
        let wide = Window { start: 1, end: 501 };

        assert!(whole.covers(&narrow));
        assert!(!narrow.covers(&whole));
        assert!(
            !wide.covers(&whole),
            "a 500-line read does not replace one that ran to the cap"
        );
        assert!(!wide.covers(&tail));
        assert!(
            !whole.covers(&tail),
            "a read from line 40 runs 40 lines further than one from line 1, because both stop \
             MAX_LINES after where they started"
        );
        assert!(whole.covers(&whole), "two identical reads: the later wins");
        assert!(narrow.covers(&narrow));
        assert!(!narrow.covers(&Window { start: 40, end: 61 }));
    }

    #[test]
    fn a_limit_above_the_cap_is_clamped_to_it() {
        // `read.rs` does `limit.min(MAX_LINES)`, so a model asking for 10,000
        // lines gets 2,000 and this must not believe otherwise.
        let huge = args_of("Read", &json!({ "file_path": "f.rs", "limit": 10_000 }));
        let uncapped = args_of("Read", &json!({ "file_path": "f.rs" }));
        assert_eq!(huge, uncapped);
    }

    // -- transform 2: folding completed edits --------------------------------

    #[test]
    fn a_completed_edit_folds_to_a_count_and_keeps_its_pairing() {
        let mut m = exchange(
            edit("a", "f.rs", "one\ntwo\nthree", "ONE"),
            ok("a", "edited"),
        );
        m.extend(exchange(
            edit("b", "f.rs", "x", "y"),
            ok("b", "edited again"),
        ));
        let before = m.clone();

        prune(&on(), &mut m);

        let ins = inputs(&m);
        assert_eq!(ins[0]["file_path"], "f.rs");
        assert!(
            ins[0].get("old_string").is_none(),
            "the old text is gone: {ins:?}"
        );
        assert!(
            ins[0].get("new_string").is_none(),
            "the new text is gone: {ins:?}"
        );
        assert!(
            ins[0]["note"].as_str().unwrap().contains("+1/-3"),
            "the counts survive: {ins:?}"
        );
        assert_eq!(ins[1]["old_string"], "x", "the newest edit is kept whole");
        assert_eq!(
            result_texts(&m),
            vec!["edited", "edited again"],
            "Edit results carry the hash labels the next edit anchors on, so they are never \
             folded: doing so would cost a Read per edit"
        );
        assert_shape_held(&before, &m);
        assert_paired(&m);
    }

    #[test]
    fn a_lines_addressed_edit_counts_the_range_it_replaced() {
        let mut m = exchange(
            call(
                "a",
                "Edit",
                json!({ "file_path": "f.rs", "lines": "12#a3f9-15#b7c1", "new_string": "a\nb" }),
            ),
            ok("a", "edited"),
        );
        m.extend(exchange(edit("b", "f.rs", "x", "y"), ok("b", "edited")));

        prune(&on(), &mut m);

        let note = inputs(&m)[0]["note"].as_str().unwrap().to_string();
        assert!(
            note.contains("+2/-4"),
            "12 through 15 inclusive is 4 lines: {note}"
        );
        assert_paired(&m);
    }

    #[test]
    fn a_single_line_address_counts_one_line() {
        assert_eq!(addressed_lines("12#a3f9"), 1);
        assert_eq!(addressed_lines("12#a3f9-15#b7c1"), 4);
        assert_eq!(
            addressed_lines("nonsense"),
            0,
            "an address this cannot parse reports nothing rather than guessing"
        );
    }

    #[test]
    fn the_only_edit_of_a_file_is_kept_whole() {
        let mut m = exchange(edit("a", "f.rs", "a", "b"), ok("a", "edited"));
        m.extend(exchange(edit("b", "other.rs", "c", "d"), ok("b", "edited")));

        prune(&on(), &mut m);

        let ins = inputs(&m);
        assert_eq!(
            ins[0]["old_string"], "a",
            "edits to different files do not fold each other"
        );
        assert_eq!(ins[1]["old_string"], "c");
        assert_paired(&m);
    }

    #[test]
    fn a_failed_edit_is_not_folded() {
        let mut m = exchange(
            edit("a", "f.rs", "a", "b"),
            failed("a", "old_string was not found"),
        );
        m.extend(exchange(edit("b", "f.rs", "a", "b"), ok("b", "edited")));

        prune(&on(), &mut m);

        assert_eq!(
            inputs(&m)[0]["old_string"],
            "a",
            "an edit that never landed still describes a change the model has to make"
        );
        assert_paired(&m);
    }

    // -- transform 3: collapsing repeated failures ---------------------------

    #[test]
    fn repeated_identical_failures_keep_only_the_last_verbatim() {
        let mut m = Vec::new();
        for id in ["a", "b", "c"] {
            m.extend(exchange(
                call(id, "Bash", json!({ "command": "make" })),
                failed(id, &long("permission denied")),
            ));
        }
        let before = m.clone();

        prune(&on(), &mut m);

        let texts = result_texts(&m);
        assert!(texts[0].starts_with("[repeated Bash failure"), "{texts:?}");
        assert!(texts[1].starts_with("[repeated Bash failure"), "{texts:?}");
        assert_eq!(
            texts[2],
            long("permission denied"),
            "the last rejection stays verbatim"
        );
        assert_shape_held(&before, &m);
        assert_paired(&m);
    }

    #[test]
    fn a_collapsed_failure_keeps_its_is_error_flag() {
        // The flag is what makes a failure an observation rather than prose the
        // model has to interpret, and the stub is still describing a failure.
        let mut m = exchange(call("a", "Bash", json!({})), failed("a", &long("denied")));
        m.extend(exchange(
            call("b", "Bash", json!({})),
            failed("b", &long("denied")),
        ));

        prune(&on(), &mut m);

        let flags: Vec<bool> = m
            .iter()
            .flat_map(|x| x.content.blocks())
            .filter_map(|b| match b {
                ContentBlock::ToolResult(r) => Some(r.is_error),
                _ => None,
            })
            .collect();
        assert_eq!(flags, vec![true, true]);
    }

    #[test]
    fn a_rejection_smaller_than_the_note_that_would_replace_it_is_left_alone() {
        // `"denied"` is six bytes; the note explaining it was dropped is about
        // a hundred and fifty. Rewriting it would grow the request *and* lose
        // the original text. This is not a hypothetical: over the operator's
        // session corpus this case was common enough that all three transforms
        // together removed fewer bytes than superseding reads alone, which is
        // what `claim_stub` exists to stop.
        let mut m = exchange(call("a", "Bash", json!({})), failed("a", "denied"));
        m.extend(exchange(
            call("b", "Bash", json!({})),
            failed("b", "denied"),
        ));

        let report = prune(&on(), &mut m);

        assert_eq!(result_texts(&m), vec!["denied", "denied"]);
        assert_eq!(report.repeated_failures, 0, "and it is not counted either");
        assert!(!report.changed());
    }

    #[test]
    fn failures_that_differ_are_all_kept() {
        let mut m = exchange(
            call("a", "Bash", json!({ "command": "make" })),
            failed("a", &long("permission denied")),
        );
        m.extend(exchange(
            call("b", "Bash", json!({ "command": "make" })),
            failed("b", &long("no such target")),
        ));

        prune(&on(), &mut m);

        assert_eq!(
            result_texts(&m),
            vec![long("permission denied"), long("no such target")],
            "a failure that differs may differ for a reason the model needs"
        );
        assert_paired(&m);
    }

    #[test]
    fn the_same_text_from_a_different_tool_is_not_a_repeat() {
        let mut m = exchange(call("a", "Bash", json!({})), failed("a", &long("denied")));
        m.extend(exchange(
            call("b", "Write", json!({})),
            failed("b", &long("denied")),
        ));

        prune(&on(), &mut m);

        assert_eq!(result_texts(&m), vec![long("denied"), long("denied")]);
        assert_paired(&m);
    }

    // -- the whole transform -------------------------------------------------

    #[test]
    fn plain_text_messages_are_untouched() {
        let mut m = vec![
            Message::user("read the file and fix it"),
            Message::assistant(vec![ContentBlock::text("on it")]),
        ];
        let before = m.clone();

        prune(&on(), &mut m);

        assert_eq!(m, before);
    }

    #[test]
    fn an_empty_conversation_is_fine() {
        let mut m: Vec<Message> = Vec::new();
        let report = prune(&on(), &mut m);
        assert!(m.is_empty());
        assert!(!report.changed());
    }

    #[test]
    fn parallel_tool_calls_in_one_message_are_handled_by_site() {
        // Two reads of the same file in one assistant turn, answered in one
        // user turn, which is the shape parallel tool use produces.
        let mut m = vec![
            Message::assistant(vec![
                read("a", "f.rs", Some(1), Some(10)),
                read("b", "f.rs", None, None),
            ]),
            results(vec![
                ok("a", &long("LINES 1 TO 10")),
                ok("b", &long("WHOLE FILE")),
            ]),
        ];
        let before = m.clone();

        prune(&on(), &mut m);

        let texts = result_texts(&m);
        assert!(
            texts[0].starts_with("[stale Read"),
            "the wide read covers the narrow one even inside one turn: {texts:?}"
        );
        assert_eq!(texts[1], long("WHOLE FILE"));
        assert_shape_held(&before, &m);
        assert_paired(&m);
    }

    #[test]
    fn thinking_blocks_and_their_signatures_are_never_written() {
        let thinking = ContentBlock::Thinking(ThinkingBlock {
            thinking: "the file is long".into(),
            signature: Some("sig-abc".into()),
            ..Default::default()
        });
        let mut m = vec![
            Message::assistant(vec![thinking.clone(), edit("a", "f.rs", "a", "b")]),
            results(vec![ok("a", "edited")]),
        ];
        m.extend(exchange(edit("b", "f.rs", "b", "c"), ok("b", "edited")));

        prune(&on(), &mut m);

        assert_eq!(
            m[0].content.blocks()[0],
            thinking,
            "folding a tool call beside a signed thinking block leaves that block byte-identical"
        );
        assert!(
            inputs(&m)[0].get("old_string").is_none(),
            "and still folds the call itself"
        );
        assert_paired(&m);
    }

    #[test]
    fn a_block_this_client_does_not_model_is_carried_through_untouched() {
        // `Passthrough` is how `content.rs` keeps a server-side tool block or
        // anything else new; a transform that rewrote one by index would be
        // corrupting bytes it cannot read.
        let opaque = ContentBlock::Passthrough(json!({
            "type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}
        }));
        let mut m = vec![
            Message::assistant(vec![opaque.clone(), read("a", "f.rs", None, None)]),
            results(vec![ok("a", &long("FIRST"))]),
        ];
        m.extend(exchange(
            read("b", "f.rs", None, None),
            ok("b", &long("SECOND")),
        ));

        prune(&on(), &mut m);

        assert_eq!(m[0].content.blocks()[0], opaque);
        assert!(
            result_texts(&m)[0].starts_with("[stale Read"),
            "and still pruned"
        );
        assert_paired(&m);
    }

    #[test]
    fn all_three_transforms_compose_over_one_conversation() {
        let mut m = Vec::new();
        m.extend(exchange(
            read("r1", "f.rs", None, None),
            ok("r1", &long("V1")),
        ));
        m.extend(exchange(
            call("x1", "Bash", json!({ "command": "test" })),
            failed("x1", &long("denied")),
        ));
        m.extend(exchange(edit("e1", "f.rs", "a", "b"), ok("e1", "edited")));
        m.extend(exchange(
            read("r2", "f.rs", None, None),
            ok("r2", &long("V2")),
        ));
        m.extend(exchange(
            call("x2", "Bash", json!({ "command": "test" })),
            failed("x2", &long("denied")),
        ));
        m.extend(exchange(edit("e2", "f.rs", "b", "c"), ok("e2", "edited")));
        let before = m.clone();

        let report = prune(&on(), &mut m);

        let texts = result_texts(&m);
        assert!(texts[0].starts_with("[stale Read"), "{texts:?}");
        assert!(texts[1].starts_with("[repeated Bash failure"), "{texts:?}");
        assert_eq!(texts[2], "edited");
        assert_eq!(texts[3], long("V2"), "the surviving read is whole");
        assert_eq!(texts[4], long("denied"), "the surviving failure is whole");
        assert_eq!(texts[5], "edited");

        let ins = inputs(&m);
        assert!(ins[2].get("old_string").is_none(), "the first edit folded");
        assert_eq!(ins[5]["old_string"], "b", "the last edit did not");

        assert_eq!(
            (
                report.stale_reads,
                report.folded_edits,
                report.repeated_failures
            ),
            (1, 1, 1)
        );
        assert_shape_held(&before, &m);
        assert_paired(&m);
    }
}

// endregion: Tests

// region: The measurement
// ---------------------------------------------------------------------------
// The measurement
//
// Not a test. It asserts nothing and is `#[ignore]`d, so `cargo test` never
// runs it — it exists so the number in the module doc is one somebody can
// re-derive rather than one they have to believe. The corpus is the operator's
// own `~/.emma/sessions`, named through an environment variable because a test
// file that hard-codes a home directory is a test that only ever ran once.
//
//     EMMA_SESSIONS=~/.emma/sessions cargo test -p emma --lib \
//         session_corpus -- --ignored --nocapture
// ---------------------------------------------------------------------------

#[cfg(test)]
mod measurement {
    use super::*;
    use crate::session;
    use emma_llm::Role;

    /// Wire bytes of every `tool_result` in a message list.
    ///
    /// Rendered rather than `content.len()` for the reason `Content::wire_len`
    /// gives: the quotes and the escapes are bytes the provider tokenises too.
    fn tool_result_bytes(messages: &[Message]) -> usize {
        messages
            .iter()
            .flat_map(|m| m.content.blocks())
            .filter(|b| matches!(b, ContentBlock::ToolResult(_)))
            .map(|b| serde_json::to_string(b).map(|s| s.len()).unwrap_or(0))
            .sum()
    }

    #[allow(clippy::field_reassign_with_default)]
    fn gate_open() -> Settings {
        let mut s = Settings::default();
        s.prune_history = Some(true);
        s
    }

    /// **What a "byte-turn" is here, spelled out, because the fork's figure is
    /// only comparable if the definition is.** A model call carries the whole
    /// conversation so far, so a result costs its wire size once per remaining
    /// call. Fold a session, take every prefix that ends in a user message —
    /// each of those is one request the loop actually built — and sum the
    /// `tool_result` bytes in it. The total over every prefix of every session
    /// is the denominator; what [`supersede_reads`] removes from the same sum
    /// is the numerator.
    #[test]
    #[ignore = "reads a session corpus named by EMMA_SESSIONS; measures, asserts nothing"]
    fn session_corpus_byte_turns() {
        let Ok(dir) = std::env::var("EMMA_SESSIONS") else {
            println!("EMMA_SESSIONS is unset; nothing to measure");
            return;
        };
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .expect("EMMA_SESSIONS must name a readable directory")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        files.sort();

        let (mut total, mut after_reads, mut after_all, mut with_traffic) = (0, 0, 0, 0);
        for file in &files {
            let Ok(messages) = session::fold(file) else {
                continue;
            };
            let mut session_total = 0usize;
            for k in 1..=messages.len() {
                if messages[k - 1].role != Role::User {
                    continue;
                }
                let prefix = &messages[..k];
                let base = tool_result_bytes(prefix);
                if base == 0 {
                    continue;
                }
                session_total += base;

                let mut reads_only = prefix.to_vec();
                let calls = collect(&reads_only);
                let mut actions = HashMap::new();
                supersede_reads(&calls, &mut actions);
                apply(&mut reads_only, &actions);
                after_reads += tool_result_bytes(&reads_only);

                let mut everything = prefix.to_vec();
                prune(&gate_open(), &mut everything);
                after_all += tool_result_bytes(&everything);
            }
            if session_total > 0 {
                with_traffic += 1;
            }
            total += session_total;
        }

        let share = |kept: usize| 100.0 * (total - kept) as f64 / total.max(1) as f64;
        println!(
            "sessions: {} read, {with_traffic} with tool traffic",
            files.len()
        );
        println!("tool_result byte-turns: {total}");
        println!(
            "superseded Reads remove {} ({:.1}%)",
            total - after_reads,
            share(after_reads)
        );
        println!(
            "all three transforms remove {} ({:.1}%)",
            total.saturating_sub(after_all),
            share(after_all)
        );
    }
}

// endregion: The measurement
