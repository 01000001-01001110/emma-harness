//! What models a provider actually has, asked rather than remembered.
//!
//! [`models`](crate::models) is a table transcribed by hand on a date, and its
//! own doc says what should replace it: *"The seam that removes this table.
//! `GET /v1/models` reports both of these."* This is that seam, built against
//! OpenRouter's `GET {base}/models`, which answers three questions the table
//! guesses at — which ids exist, which cost nothing, and what each will accept.
//!
//! # Free is a price, not a suffix, and price alone is not enough
//!
//! Measured against the live OpenRouter roster on 2026-08-30, 396 models:
//!
//! - Filtering on the id ending `:free` returns 18 and **misses three models
//!   that are genuinely free** — the suffix is a naming convention, not a
//!   guarantee, and nothing makes a provider follow it.
//! - Filtering on `pricing.prompt` and `pricing.completion` both being zero
//!   returns 21, and three of those cannot run a turn of Emma's loop: two music
//!   models and a content-safety classifier, none of which lists `tools` in
//!   `supported_parameters`. Handing any of them a tool-calling request is a
//!   400 on the first call, which is exactly the failure `models.rs` exists to
//!   prevent.
//! - Price-zero **and** tool support **and** text output returns 18 usable
//!   models — and here is the trap: *a different 18*. The two filters agree on
//!   the count and disagree on the set. A test asserting `18` would pass over
//!   either, which is why the tests below assert on membership and never on a
//!   length.
//!
//! **`text_out` excluded nothing in that capture, and is kept anyway.** The
//! brief this was built from said the music models were dropped for emitting
//! `["text","audio"]`; they are not. All 396 entries list `"text"` among their
//! output modalities — the music models included — so the check that actually
//! drops them is `tools`. What [`ModelInfo::text_out`] guards is a shape the
//! roster does not contain today: a tool-capable model that emits no text. It
//! costs one field and one `&&`, it is the difference between a filter that
//! happens to be right and one that says why, and the only test that exercises
//! it uses an invented entry, which is stated where that entry is.
//!
//! # What this deliberately does not claim
//!
//! **Zero prompt and completion price is not the same as "this call is free".**
//! One of the music models above prices whole songs on a dimension that is not
//! `prompt` or `completion`, so a future text model billed per-request would
//! read as free here. The two fields are what the endpoint reports for the
//! shape of call Emma makes; anything else is a claim the roster cannot back.
//!
//! **Free models are rate-limited hard.** Two of four live calls on 2026-08-30
//! came back 429 within a minute, per model, saying the model *"is temporarily
//! rate-limited upstream"*. A roster entry says a model exists and costs
//! nothing, never that it will answer.
//!
//! # Freshness, and failure
//!
//! [`Roster::fetched_at_ms`] is not optional, because a roster shown as current
//! when it is an hour old is the same defect class as a compiled-in sample
//! value: the price it reports is the price at fetch time and nothing else.
//! Whatever displays a roster must be able to say *when* — [`Roster::age_ms`]
//! is there so the answer is arithmetic rather than a guess.
//!
//! A fetch failure is a [`RosterError`] the caller renders as a notice, never a
//! boot failure and never a panic. A provider whose roster cannot be read still
//! works for a user who knows an id — which is Ollama's ordinary position on a
//! box with no server running. Nothing here is called at startup: on demand,
//! and on first key acceptance.
//!
//! # Parsing
//!
//! Every field is read out of a [`Value`] by hand rather than through a derived
//! `Deserialize`, and that is the scar in `content.rs` applied here: a strict
//! decoder in this crate passed every test and then decoded no real tool call,
//! because the API added a key nobody had modelled. An entry with a key this
//! parser has never seen must survive whole; an entry that is unreadable must
//! cost that entry and not the roster.
//!
//! The one thing this module borrows from the rest of the crate is
//! [`ApiKey`] and the process-wide scrub behind [`crate::scrub_secrets`], which
//! is what keeps a key out of an echoed error body. Otherwise it depends on
//! `reqwest` and `serde_json` alone, so it copies to another tree as one file.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::{scrub_secrets, trim_body, ApiKey};

/// OpenRouter's API root. The path `/models` is appended by [`fetch`], so this
/// is the same string a chat-completions client points at.
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// How long a roster fetch may take before it is a transport failure. Short,
/// because every caller is a person waiting: a roster that has not arrived is
/// an inconvenience, and a UI that has stopped responding is a bug report.
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

// region: One model, as the provider describes it
// ---------------------------------------------------------------------------
// One model, as the provider describes it
//
// The six facts a caller acts on, and the three-part test that decides whether
// Emma's loop can use the model at all.
// ---------------------------------------------------------------------------

/// One roster entry, reduced to what a caller decides with.
///
/// The three booleans are separate rather than one `usable` flag because they
/// answer different questions and a UI shows them differently: *free* filters
/// the picker, *tools* explains why a free model is missing from it, and
/// *text_out* is why a music model that costs nothing is not an option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    /// The id to send on the wire, verbatim. Never normalised — OpenRouter
    /// renames models and ships some under nicknames, so the string the
    /// provider gave is the only one guaranteed to route.
    pub id: String,
    /// Total context window, or `None` when the entry does not report one.
    /// `None` means *unknown*, never *unlimited*.
    pub context_length: Option<u32>,
    /// `top_provider.max_completion_tokens` — the per-model output ceiling that
    /// [`models::Limits::max_tokens`](crate::models::Limits::max_tokens)
    /// hardcodes today.
    pub max_output: Option<u32>,
    /// Both `pricing.prompt` and `pricing.completion` parse to exactly zero.
    ///
    /// A missing or unparseable price is **not** free. The asymmetry decides
    /// it: calling a paid model believed free spends the user's money with no
    /// warning, and hiding a free model believed paid costs them a menu entry.
    pub free: bool,
    /// `"tools"` appears in `supported_parameters`. Emma's loop is nothing but
    /// tool calls, so a model without this cannot run a turn.
    pub tools: bool,
    /// `"text"` appears in `architecture.output_modalities`. Excludes the music
    /// and image models that sit on the same roster at the same zero price.
    pub text_out: bool,
}

impl ModelInfo {
    /// Free, tool-capable and text-producing — all three, or Emma cannot use
    /// it. See the module doc for what each one drops.
    pub fn usable(&self) -> bool {
        self.free && self.tools && self.text_out
    }
}

// endregion: One model, as the provider describes it

// region: The roster, and when it was true
// ---------------------------------------------------------------------------
// The roster, and when it was true
//
// A list of models is only half the answer; the other half is the timestamp,
// which is why it is a field and not an `Option`.
// ---------------------------------------------------------------------------

/// Every model one provider reported, and the moment it said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roster {
    pub models: Vec<ModelInfo>,
    /// Unix milliseconds at which the bytes arrived. **Not optional**: a caller
    /// that cannot say when a roster was fetched cannot honestly present the
    /// prices in it.
    pub fetched_at_ms: u64,
    /// The URL the roster came from, so a cache keyed by provider and base URL
    /// can tell two hosts apart, and so a stale entry can name its origin.
    pub source: String,
}

impl Roster {
    /// Parse a `GET {base}/models` body.
    ///
    /// `fetched_at_ms` is a parameter rather than read from the clock here, so
    /// a test can pin an age and a cache can record the time the *bytes*
    /// arrived rather than the time parsing finished.
    ///
    /// An entry that cannot be read — not an object, or no `id` — is dropped
    /// and the rest survive. One malformed row out of hundreds must not cost
    /// the roster; a body with no `data` array at all is not a roster and is an
    /// error.
    pub fn parse(
        body: &str,
        source: impl Into<String>,
        fetched_at_ms: u64,
    ) -> Result<Self, RosterError> {
        let doc: Value = serde_json::from_str(body)
            .map_err(|e| RosterError::Malformed(format!("the body is not JSON: {e}")))?;
        let entries = doc.get("data").and_then(Value::as_array).ok_or_else(|| {
            RosterError::Malformed("the body has no `data` array, so it is not a model list".into())
        })?;
        Ok(Self {
            models: entries.iter().filter_map(model_from_entry).collect(),
            fetched_at_ms,
            source: source.into(),
        })
    }

    /// The models Emma can actually run — see [`ModelInfo::usable`].
    pub fn usable(&self) -> impl Iterator<Item = &ModelInfo> {
        self.models.iter().filter(|m| m.usable())
    }

    /// Everything priced at zero, usable or not. Separate from [`Self::usable`]
    /// so a picker can say *"14 free models hidden: 3 cannot call tools"*
    /// rather than silently shortening.
    pub fn free(&self) -> impl Iterator<Item = &ModelInfo> {
        self.models.iter().filter(|m| m.free)
    }

    /// One model by exact id.
    pub fn get(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    /// How old this roster is at `now_ms`, in milliseconds.
    ///
    /// Saturating, because a clock that moved backwards between fetch and
    /// display must read as *fresh* rather than as an enormous age — the wrong
    /// answer either way, but only one of them puts a decade on the screen.
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.fetched_at_ms)
    }
}

/// Unix milliseconds now, or `0` if the clock is before the epoch. The fallback
/// cannot be a panic: a wrong timestamp degrades a freshness label, and a panic
/// takes down a session that was only listing models.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// endregion: The roster, and when it was true

// region: Reading one entry defensively
// ---------------------------------------------------------------------------
// Reading one entry defensively
//
// Every helper here answers "what does the entry say, if anything?" — never
// "the entry must say this". Unknown keys are ignored by construction because
// nothing enumerates the keys.
// ---------------------------------------------------------------------------

/// One entry, or `None` when there is not enough of it to name a model.
fn model_from_entry(entry: &Value) -> Option<ModelInfo> {
    let id = entry.get("id")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    let pricing = entry.get("pricing");
    Some(ModelInfo {
        id: id.to_string(),
        // The top-level window is what OpenRouter documents; `top_provider`
        // repeats it and is sometimes null, so it is the fallback rather than
        // the source.
        context_length: as_u32(entry.get("context_length")).or_else(|| {
            as_u32(
                entry
                    .get("top_provider")
                    .and_then(|t| t.get("context_length")),
            )
        }),
        max_output: as_u32(
            entry
                .get("top_provider")
                .and_then(|t| t.get("max_completion_tokens")),
        ),
        free: is_zero(pricing.and_then(|p| p.get("prompt")))
            && is_zero(pricing.and_then(|p| p.get("completion"))),
        tools: contains(entry.get("supported_parameters"), "tools"),
        text_out: contains(
            entry
                .get("architecture")
                .and_then(|a| a.get("output_modalities")),
            "text",
        ),
    })
}

/// A price of exactly zero.
///
/// **The values are strings** — `"0"`, `"0.000000834"` — which is the shape
/// that makes a `#[derive(Deserialize)]` with an `f64` field decode nothing at
/// all. A JSON number is accepted too, because a provider that switches to one
/// should not silently turn every free model paid.
///
/// Absent, null, or unparseable is *not* zero: see [`ModelInfo::free`] for why
/// the doubt resolves against "free".
fn is_zero(price: Option<&Value>) -> bool {
    match price {
        Some(Value::String(s)) => s.trim().parse::<f64>().map(|n| n == 0.0).unwrap_or(false),
        Some(Value::Number(n)) => n.as_f64().map(|n| n == 0.0).unwrap_or(false),
        _ => false,
    }
}

/// Whether a JSON array of strings contains `needle`. A missing or
/// wrongly-typed field reads as "does not contain it", which is the
/// conservative direction for both callers: an unstated capability is one Emma
/// does not rely on.
fn contains(list: Option<&Value>, needle: &str) -> bool {
    list.and_then(Value::as_array)
        .is_some_and(|xs| xs.iter().filter_map(Value::as_str).any(|x| x == needle))
}

/// A non-negative integer that fits in `u32`, or `None`. Null, absent, a
/// string, or a value past `u32::MAX` all read as unknown rather than as a
/// truncated number somebody later sends as a ceiling.
fn as_u32(v: Option<&Value>) -> Option<u32> {
    u32::try_from(v?.as_u64()?).ok()
}

// endregion: Reading one entry defensively

// region: Fetching, and failing without stopping anything
// ---------------------------------------------------------------------------
// Fetching, and failing without stopping anything
//
// One request, one timeout, and an error type whose whole job is to be
// rendered as a notice beside a provider that still works.
// ---------------------------------------------------------------------------

/// Why a roster could not be read. Every variant is a notice, never a reason to
/// refuse to start.
pub enum RosterError {
    /// The host could not be reached, or did not answer in time.
    Transport(String),
    /// The host answered with a non-2xx status.
    Http { status: u16, body: String },
    /// The bytes arrived and were not a model list.
    Malformed(String),
}

impl RosterError {
    /// What a user should be shown. `Display` scrubs it; nothing else may call
    /// this.
    fn sentence(&self) -> String {
        match self {
            Self::Transport(e) => {
                format!(
                    "the model roster could not be fetched: {e}. Models can still be named by id"
                )
            }
            Self::Http { status, body } => format!(
                "the provider refused the model roster (HTTP {status}): {body}. Models can still \
                 be named by id"
            ),
            Self::Malformed(e) => format!("the model roster could not be read: {e}"),
        }
    }
}

// Scrubbed at the formatting boundary rather than at each construction site,
// for the reason `LlmError`'s impls carry at length: a request that fails
// against an authenticated endpoint can come back with the key echoed in the
// body, and a construction-site scrub is a habit every new call site has to
// remember. `Debug` is hand-written for the same reason a derived one would be
// wrong — `unwrap()` in a test prints it.
impl std::fmt::Display for RosterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&scrub_secrets(&self.sentence()))
    }
}

impl std::fmt::Debug for RosterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let variant = match self {
            Self::Transport(_) => "Transport",
            Self::Http { .. } => "Http",
            Self::Malformed(_) => "Malformed",
        };
        write!(f, "{variant}({})", scrub_secrets(&self.sentence()))
    }
}

impl std::error::Error for RosterError {}

/// Ask one provider what it has.
///
/// `base_url` is the same API root a chat client points at
/// ([`OPENROUTER_BASE_URL`] for OpenRouter); `/models` is appended here.
///
/// **Never call this at startup.** On demand, and once when a key is first
/// accepted — a network call in the boot path turns a working offline session
/// into a hang, and the roster is not needed to send a request to a model whose
/// id the user already knows.
pub async fn fetch(base_url: &str, key: &ApiKey) -> Result<Roster, RosterError> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| RosterError::Transport(e.to_string()))?;
    let response = client
        .get(&url)
        .bearer_auth(key.expose())
        .send()
        .await
        .map_err(|e| RosterError::Transport(e.to_string()))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| RosterError::Transport(e.to_string()))?;
    if !status.is_success() {
        return Err(RosterError::Http {
            status: status.as_u16(),
            body: trim_body(&body),
        });
    }
    Roster::parse(&body, url, now_ms())
}

// endregion: Fetching, and failing without stopping anything

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Offline, against a fixture cut down from a real capture. What each one pins
// is the part of the filter that the ordinary state of the world does *not*
// already satisfy: that a free music model is excluded, that a free model
// without tools is excluded, that the name filter and this one disagree about
// which models those are, and that an entry with an unfamiliar key survives.
//
// The live certification at the bottom is `#[ignore]`d and gated on a key file
// existing, because the whole point of it is that a fixture agrees with its
// author.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Ten entries cut from a real `GET https://openrouter.ai/api/v1/models`
    /// capture taken on 2026-08-30 (396 models, 655 KB) — the six real ones are
    /// verbatim apart from `description`, `benchmarks` and `default_parameters`
    /// being dropped for size. The four `fictional/` ones are **invented**, and
    /// say so in their ids: the live roster had no entry missing `pricing`,
    /// none without an `id`, and none that emits no text, and no invented key
    /// would be recognisable as invented if it were copied from a real one.
    /// All four are shapes the parser must survive rather than shapes anybody
    /// has observed — the real entries already carry plenty of keys this parser
    /// ignores (`links`, `reasoning`, `canonical_slug`), which is why the
    /// unknown-key test names one that could only have come from here.
    const FIXTURE: &str = include_str!("roster_fixture.json");

    fn fixture() -> Roster {
        Roster::parse(FIXTURE, "fixture", 1_700_000_000_000).expect("the fixture is a model list")
    }

    fn ids<'a>(models: impl Iterator<Item = &'a ModelInfo>) -> Vec<String> {
        models.map(|m| m.id.clone()).collect()
    }

    fn has(ids: &[String], id: &str) -> bool {
        ids.iter().any(|x| x == id)
    }

    #[test]
    fn a_free_music_model_is_not_a_usable_model() {
        // `google/lyria-3-pro-preview` is priced at zero on both fields, so a
        // price-only filter offers it, the first call fails, and the user is
        // left explaining a 400 they did not cause.
        //
        // Note *which* check catches it, because it is not the one the plan
        // predicted: the entry lists `["text","audio"]`, so `text_out` is
        // true and the music model is dropped for having no `tools`.
        let roster = fixture();
        let lyria = roster.get("google/lyria-3-pro-preview").unwrap();
        assert!(
            lyria.free,
            "the fixture entry is not free, so this proves nothing"
        );
        assert!(lyria.text_out, "the capture lists text among its outputs");
        assert!(!lyria.tools);
        assert!(!lyria.usable());
        assert!(
            !roster.usable().any(|m| m.id == lyria.id),
            "a music model reached the usable set"
        );
    }

    /// The one test of [`ModelInfo::text_out`], against an **invented** entry.
    ///
    /// Every model in the 2026-08-30 capture lists `"text"` among its output
    /// modalities, so no real entry exercises this check — which is exactly why
    /// it needs saying out loud rather than being covered by a test that would
    /// pass with the field deleted. `fictional/audio-only-with-tools` is free
    /// and tool-capable and emits audio only; nothing like it is on the roster
    /// today, and if one appears the filter is already right.
    #[test]
    fn a_free_tool_capable_model_that_emits_no_text_is_not_usable() {
        let roster = fixture();
        let audio = roster.get("fictional/audio-only-with-tools").unwrap();
        assert!(
            audio.free && audio.tools,
            "the entry must clear every other check, or this proves nothing about `text_out`"
        );
        assert!(!audio.text_out);
        assert!(!audio.usable());
        assert!(!roster.usable().any(|m| m.id == audio.id));
    }

    #[test]
    fn a_free_model_that_cannot_call_tools_is_not_usable() {
        // `nvidia/nemotron-3.5-content-safety:free` is free, text-out, and
        // lists no `tools` — Emma's loop is nothing but tool calls.
        let roster = fixture();
        let safety = roster
            .get("nvidia/nemotron-3.5-content-safety:free")
            .unwrap();
        assert!(
            safety.free && safety.text_out,
            "the fixture entry changed shape"
        );
        assert!(!safety.tools);
        assert!(!safety.usable());
    }

    /// **The count is a coincidence; the set is the answer.**
    ///
    /// On the live roster the `:free` suffix filter and the price-and-capability
    /// filter both return 18, and they are not the same 18. A test on a length
    /// passes over either. This one asserts the two disagreements directly: a
    /// usable model whose id does not end `:free`, and a `:free` id that is not
    /// usable.
    #[test]
    fn the_name_filter_and_this_filter_disagree_about_which_models() {
        let roster = fixture();
        let usable = ids(roster.usable());

        assert!(
            has(&usable, "openrouter/free"),
            "a usable model with no `:free` suffix was dropped: {usable:?}"
        );
        assert!(
            !has(&usable, "nvidia/nemotron-3.5-content-safety:free"),
            "a `:free` id that cannot call tools was offered: {usable:?}"
        );
        // …and the ordinary case still works, or the two assertions above are
        // satisfied by a filter that returns nothing.
        assert!(has(&usable, "z-ai/glm-5.2:free"), "{usable:?}");
        assert!(has(&usable, "google/gemma-4-31b-it:free"), "{usable:?}");
    }

    #[test]
    fn a_paid_model_is_not_free_however_small_the_price() {
        // `0.000000834` is not `0`. A parser that read the string as a bool, or
        // that treated "has a pricing block" as free, would offer this and bill
        // the user for a run they were told was free.
        let roster = fixture();
        let paid = roster.get("tencent/hy4-preview").unwrap();
        assert!(!paid.free);
        assert!(
            paid.tools && paid.text_out,
            "the fixture entry changed shape"
        );
        assert!(!paid.usable());
        assert!(!has(&ids(roster.free()), "tencent/hy4-preview"));
    }

    #[test]
    fn an_entry_with_no_pricing_at_all_is_not_free_by_default() {
        // The doubt resolves against "free": a missing price is unknown, and
        // guessing free is the direction that spends money silently.
        let roster = fixture();
        let unknown = roster.get("fictional/no-pricing-reported").unwrap();
        assert!(
            !unknown.free,
            "a model with no reported price was called free"
        );
        assert!(!unknown.usable());
        // It is still on the roster, with its ceilings read — a model with no
        // price is a model the user may still name by id.
        assert_eq!(unknown.max_output, Some(4_096));
        assert_eq!(unknown.context_length, Some(32_768));
    }

    #[test]
    fn a_key_this_parser_has_never_seen_does_not_lose_the_entry() {
        // The scar this crate already paid for: a strict decoder that passed
        // every test and then decoded no real tool call, because the API added
        // a key. The fixture entry carries an unknown top-level key, an unknown
        // key inside `architecture`, and an unknown price dimension.
        let roster = fixture();
        let future = roster.get("fictional/unknown-key-from-the-future").unwrap();
        assert!(future.free && future.tools && future.text_out);
        assert!(future.usable());
        assert_eq!(future.context_length, Some(65_536));
        assert_eq!(future.max_output, Some(8_192));
    }

    #[test]
    fn one_unreadable_entry_costs_that_entry_and_not_the_roster() {
        // The fixture's last entry has no `id`. Dropping the roster over it
        // would turn one bad row out of hundreds into "this provider has no
        // models", which is indistinguishable from a network failure.
        let roster = fixture();
        assert_eq!(
            roster.models.len(),
            9,
            "expected the id-less entry to be dropped"
        );
        assert!(roster.get("z-ai/glm-5.2:free").is_some());
        assert!(roster.models.iter().all(|m| !m.id.is_empty()));
    }

    #[test]
    fn the_ceilings_come_from_the_provider_rather_than_a_table() {
        // This is the whole point of the seam: these two numbers are what
        // `models::TABLE` hardcodes per model and transcribes by hand.
        let roster = fixture();
        let glm = roster.get("z-ai/glm-5.2:free").unwrap();
        assert_eq!(glm.context_length, Some(256_000));
        assert_eq!(glm.max_output, Some(230_400));

        // A null ceiling is `None` — unknown — and never a zero a caller would
        // send as `max_tokens`. `openrouter/free` is a router alias and reports
        // both as null, with a top-level `context_length` to fall back on.
        let router = roster.get("openrouter/free").unwrap();
        assert_eq!(router.max_output, None);
        assert_eq!(router.context_length, Some(200_000));
    }

    #[test]
    fn a_body_that_is_not_a_model_list_is_an_error_rather_than_an_empty_roster() {
        // An empty roster and a failed fetch look identical to a UI, and only
        // one of them should say "this provider has no models".
        assert!(Roster::parse("{\"error\":{\"message\":\"no\"}}", "x", 0).is_err());
        assert!(Roster::parse("not json at all", "x", 0).is_err());
        // …but a genuinely empty list is a roster, not an error.
        let empty = Roster::parse("{\"data\":[]}", "x", 0).unwrap();
        assert!(empty.models.is_empty());
    }

    #[test]
    fn a_roster_can_always_say_how_old_it_is() {
        let roster = fixture();
        assert_eq!(roster.fetched_at_ms, 1_700_000_000_000);
        assert_eq!(roster.age_ms(1_700_000_060_000), 60_000);
        // A clock that went backwards reads as fresh rather than as an age of
        // half a century.
        assert_eq!(roster.age_ms(1_600_000_000_000), 0);
        assert_eq!(roster.source, "fixture");
    }

    #[test]
    fn a_failed_fetch_renders_as_a_notice_and_never_carries_the_key() {
        const FAKE: &str = "sk-or-v1-NOTAREALKEYJUSTAFIXTURE";
        let _key = ApiKey::new(FAKE);
        let err = RosterError::Http {
            status: 401,
            body: format!("no auth credentials found for {FAKE}"),
        };
        let shown = format!("{err}");
        assert!(!shown.contains(FAKE), "{shown}");
        assert!(!format!("{err:?}").contains(FAKE), "{err:?}");
        // Anti-vacuity: every assertion above is satisfied by a formatter that
        // prints nothing. The notice must survive the scrub, and must say that
        // the provider is still usable by id.
        assert!(
            shown.contains("401") && shown.contains("no auth credentials"),
            "{shown}"
        );
        assert!(shown.contains("named by id"), "{shown}");
        assert!(
            format!("{}", RosterError::Transport("connection refused".into()))
                .contains("named by id")
        );
    }

    // -----------------------------------------------------------------------
    // Certification against the live API.
    //
    // `#[ignore]`d because it needs a key and the network, and it asserts
    // *relationships* rather than magnitudes: the roster changes without asking
    // us, so pinning "18 usable models" would fail for the wrong reason on the
    // first day OpenRouter adds one.
    //
    //     cargo test -p emma-llm --lib -- --ignored roster
    // -----------------------------------------------------------------------

    /// The environment variable naming a file that holds an OpenRouter key.
    ///
    /// **The path is not written down here.** It used to be an absolute one
    /// naming the owner's home directory and his secret store, which was
    /// correct on exactly one machine and told every reader of a public tree
    /// where to look. The key itself was never in the repository; the map to
    /// it was, which is the same mistake one step removed.
    ///
    /// Read at call time and never copied anywhere — not into a fixture, not
    /// into an assertion message. Unset means the test says so and certifies
    /// nothing, which is the honest outcome for a machine without a key.
    const KEY_FILE_ENV: &str = "EMMA_OPENROUTER_KEY_FILE";

    #[tokio::test]
    #[ignore = "hits the live OpenRouter API and needs a key on disk"]
    async fn the_live_roster_has_the_shape_this_module_claims() {
        let Ok(path) = std::env::var(KEY_FILE_ENV) else {
            eprintln!("{KEY_FILE_ENV} is unset, so nothing here is certified");
            return;
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            eprintln!("{KEY_FILE_ENV} names a file that cannot be read; nothing certified");
            return;
        };
        let key = ApiKey::new(raw.trim());
        let roster = fetch(OPENROUTER_BASE_URL, &key)
            .await
            .expect("the live roster fetched");

        // Relationships, not counts.
        assert!(roster.models.len() > 50, "{}", roster.models.len());
        let free: Vec<_> = roster.free().collect();
        let usable: Vec<_> = roster.usable().collect();
        assert!(!free.is_empty(), "no free models on the live roster");
        assert!(!usable.is_empty(), "no usable models on the live roster");
        assert!(usable.len() <= free.len(), "usable is a subset of free");
        assert!(
            usable.iter().all(|m| m.free && m.tools && m.text_out),
            "a model reached the usable set without all three properties"
        );
        assert!(
            usable.iter().any(|m| m.context_length.is_some()),
            "no usable model reported a context length, so the ceilings this \
             seam exists to discover are not being read"
        );
        // The freshness claim, against the real clock.
        assert!(roster.age_ms(now_ms()) < 120_000);
        assert!(roster.source.ends_with("/models"), "{}", roster.source);

        // Counts and ids only — never the key.
        eprintln!(
            "live roster: {} models, {} free, {} usable, from {}",
            roster.models.len(),
            free.len(),
            usable.len(),
            roster.source
        );
    }
}

// endregion: Tests
