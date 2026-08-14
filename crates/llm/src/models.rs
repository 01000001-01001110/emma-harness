//! What a model will accept, as opposed to what Emma would like to send.
//!
//! Two of the values in a [`Request`](crate::Request) are not properties of the
//! request at all — they are properties of the *model*. `max_tokens` has a
//! per-model ceiling and a request above it is an HTTP 400 before a single
//! token is generated; `output_config.effort` names a level that not every
//! model implements, and `xhigh` on a model without it is the same 400. Both
//! were single constants here until a user could choose a model, at which point
//! the first call on the wrong choice failed for a reason nobody had asked
//! about.
//!
//! So the request carries what the caller *wants* — a ceiling and a preferred
//! depth — and this module says what the chosen model will actually take.
//! [`AnthropicProvider::render`](crate::anthropic::AnthropicProvider) clamps one
//! against the other. Clamping happens there, at the wire boundary, rather than
//! in `Request::new`, because `Request` does not know the model: the provider
//! holds it, and the provider is the last place every request passes through no
//! matter who built it.
//!
//! **This table is a snapshot of facts that change without asking us.** Models
//! ship, ceilings move, effort levels are added — `xhigh` did not exist before
//! Opus 4.7. The values below were transcribed on 2026-08-11 from Anthropic's
//! published model list; they are correct on that date and only that date.
//!
//! **The seam that removes this table.** `GET /v1/models` reports both of these
//! per model — `max_tokens` as a field, and the effort levels under
//! `capabilities.effort.<level>.supported` — so a later stage can discover them
//! from the provider with the key the user already gave us and keep this table
//! only as the offline fallback. [`Limits`] is deliberately the shape that
//! response parses into, so that stage replaces the *lookup* and not the
//! clamping. See `notes/design-provider-and-model.md` §4.1.
//!
//! Note what is **not** here: the minimum cacheable prefix. It is the same
//! class of per-model value, it is wrong for the same reason, and the models
//! endpoint does not report it — so it cannot follow this table into discovery
//! and needs its own answer. `anthropic.rs`'s `MIN_CACHEABLE` owns it, as a
//! second longest-prefix table with the same rows and a *different* fallback
//! direction: this one guesses low for an unknown model because a high ceiling
//! 400s, that one guesses high because a low floor silently overpays. Adding a
//! model means adding a row to both.

use crate::Effort;

/// What one model accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest `max_tokens` this model accepts. Above it, HTTP 400.
    pub max_tokens: u32,
    /// Effort levels this model implements, ascending. **Not a ceiling** — a
    /// slice, because the set has holes: Opus 4.6 supports `max` but not
    /// `xhigh`, which arrived one model later. A ceiling would send `xhigh`
    /// there and earn the 400 this module exists to prevent.
    pub efforts: &'static [Effort],
}

/// What we send to a model this table has never heard of.
///
/// **A user can name any model string, including one released after this file
/// was written, and that case is the whole design.** Three answers were
/// available and the asymmetry between their failures picks the winner:
///
/// - *Assume the model is as capable as the newest one we know.* Wrong in the
///   one direction that cannot degrade gracefully: too high a `max_tokens`, or
///   an effort level the model does not implement, is an HTTP 400 on the first
///   call — no answer, and an error naming a value the user never set.
/// - *Refuse to run against an unknown model.* Turns every new model release
///   into a bug in Emma, and makes the table a gate rather than a hint. The
///   user who typed the id is likelier to be right about it than we are.
/// - **A conservative floor.** Too *low* a `max_tokens` truncates a long
///   answer, and omitting `effort` gets the provider's own default. Both are
///   degraded, both still produce a turn, and both are fixed by adding one row
///   below. That asymmetry — a low value works everywhere, a high value fails
///   everywhere it is wrong — is the whole argument.
///
/// 8,192 is the smallest output ceiling any Anthropic model has shipped with,
/// so no known model rejects it. `efforts: &[]` means no `output_config` is
/// sent at all, which every model accepts — including the ones (Sonnet 4.5,
/// Haiku 4.5) that reject the field outright.
///
/// The cost is real and worth naming: a user on a brand-new Opus gets 8,192
/// tokens and default effort until somebody edits this file. That is the price
/// of not guessing, and it is the reason the discovery seam above matters.
pub const UNKNOWN: Limits = Limits {
    max_tokens: 8_192,
    efforts: &[],
};

const ALL_FIVE: &[Effort] = &[
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::XHigh,
    Effort::Max,
];

/// `xhigh` arrived with Opus 4.7; the 4.6 generation has `max` without it.
const NO_XHIGH: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High, Effort::Max];

/// Longest-prefix table, so a dated snapshot (`claude-haiku-4-5-20251001`)
/// resolves to the same row as its alias. Prefix rather than equality because
/// the API accepts both spellings and a user pasting from the console gets the
/// dated one.
///
/// Only models whose numbers were verified are listed. A model left out is not
/// a model Emma refuses — it falls to [`UNKNOWN`], which is the point.
// `pub(crate)` only so `anthropic.rs` can check its `MIN_CACHEABLE` names the
// same models this does. The two tables are deliberately separate — see the
// module doc — and the whole cost of that split is that nothing but a test
// keeps their row sets together.
pub(crate) const TABLE: &[(&str, Limits)] = &[
    (
        "claude-fable-5",
        Limits {
            max_tokens: 128_000,
            efforts: ALL_FIVE,
        },
    ),
    (
        "claude-mythos-5",
        Limits {
            max_tokens: 128_000,
            efforts: ALL_FIVE,
        },
    ),
    (
        "claude-opus-5",
        Limits {
            max_tokens: 128_000,
            efforts: ALL_FIVE,
        },
    ),
    (
        "claude-opus-4-8",
        Limits {
            max_tokens: 128_000,
            efforts: ALL_FIVE,
        },
    ),
    (
        "claude-opus-4-7",
        Limits {
            max_tokens: 128_000,
            efforts: ALL_FIVE,
        },
    ),
    (
        "claude-opus-4-6",
        Limits {
            max_tokens: 128_000,
            efforts: NO_XHIGH,
        },
    ),
    (
        "claude-sonnet-5",
        Limits {
            max_tokens: 128_000,
            efforts: ALL_FIVE,
        },
    ),
    (
        "claude-sonnet-4-6",
        Limits {
            max_tokens: 128_000,
            efforts: NO_XHIGH,
        },
    ),
    // Sonnet 4.5 and Haiku 4.5 predate the effort parameter and reject it
    // outright — `efforts: &[]` is what stops Emma sending the field at all.
    (
        "claude-sonnet-4-5",
        Limits {
            max_tokens: 64_000,
            efforts: &[],
        },
    ),
    (
        "claude-haiku-4-5",
        Limits {
            max_tokens: 64_000,
            efforts: &[],
        },
    ),
];

/// What `model` accepts, or [`UNKNOWN`] if this table has never heard of it.
pub fn limits(model: &str) -> Limits {
    TABLE
        .iter()
        .filter(|(id, _)| model.starts_with(id))
        // Longest match wins, so `claude-opus-4-8` never resolves through a
        // shorter row that happens to prefix it.
        .max_by_key(|(id, _)| id.len())
        .map(|(_, l)| *l)
        .unwrap_or(UNKNOWN)
}

impl Limits {
    /// The caller's ceiling, lowered to the model's if the model's is lower.
    ///
    /// Only ever downward. A request asking for 32,000 on a model that caps at
    /// 128,000 still sends 32,000 — the ask is a budget as well as a limit, and
    /// raising it would spend the user's money on their behalf.
    pub fn clamp_max_tokens(&self, requested: u32) -> u32 {
        requested.min(self.max_tokens)
    }

    /// The best level this model supports that is no higher than `requested`,
    /// or `None` when there is none.
    ///
    /// `None` means *send no `output_config` at all* rather than "send low",
    /// and it covers two different situations on purpose: a model with no
    /// effort parameter, and a request whose asked-for level sits below
    /// everything the model implements. Both want the same thing — the
    /// provider's own default — and neither wants a level the caller did not
    /// ask for. Clamping upward to "the nearest supported" would send a model
    /// harder than the caller requested, which on a `Low` request for a cheap
    /// subagent is exactly the bill they were avoiding.
    pub fn clamp_effort(&self, requested: Effort) -> Option<Effort> {
        self.efforts
            .iter()
            .copied()
            .filter(|e| e.rank() <= requested.rank())
            .max_by_key(|e| e.rank())
    }
}

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The two guarantees worth guarding hardest, stated as names: a model with a
// lower ceiling never receives the caller's 32,000, and a model without `xhigh`
// never receives `xhigh`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lower_ceiling_is_never_handed_the_full_ask() {
        // Every row, plus the unknown fallback: nothing may come back above
        // what that model accepts, whatever the caller asked for.
        for (id, l) in TABLE {
            let got = limits(id).clamp_max_tokens(32_000);
            assert!(
                got <= l.max_tokens,
                "{id} accepts {} but would be sent {got}",
                l.max_tokens
            );
        }
        assert_eq!(limits("claude-haiku-4-5").clamp_max_tokens(32_000), 32_000);
        assert_eq!(limits("claude-haiku-4-5").clamp_max_tokens(200_000), 64_000);
        assert_eq!(limits("a-model-from-2029").clamp_max_tokens(32_000), 8_192);
    }

    #[test]
    fn the_ask_is_never_raised_to_the_ceiling() {
        // 32,000 on a 128,000 model stays 32,000. Clamping is a limit, not a
        // budget the model gets to spend.
        assert_eq!(limits("claude-opus-5").clamp_max_tokens(32_000), 32_000);
    }

    #[test]
    fn a_model_without_xhigh_is_never_sent_xhigh() {
        for (id, _) in TABLE {
            let l = limits(id);
            if let Some(chosen) = l.clamp_effort(Effort::XHigh) {
                assert!(
                    l.efforts.contains(&chosen),
                    "{id} would be sent {} which it does not support",
                    chosen.as_str()
                );
                assert!(
                    chosen.rank() <= Effort::XHigh.rank(),
                    "{id} would be sent {}, above the ask",
                    chosen.as_str()
                );
            }
        }
        // The hole in the ladder: 4.6 has `max` but not `xhigh`, so an xhigh
        // ask lands on `high` and not on `max`.
        assert_eq!(
            limits("claude-opus-4-6").clamp_effort(Effort::XHigh),
            Some(Effort::High)
        );
        assert_eq!(
            limits("claude-opus-5").clamp_effort(Effort::XHigh),
            Some(Effort::XHigh)
        );
    }

    #[test]
    fn a_model_with_no_effort_parameter_gets_none_of_it() {
        assert_eq!(limits("claude-haiku-4-5").clamp_effort(Effort::XHigh), None);
        assert_eq!(limits("claude-haiku-4-5").clamp_effort(Effort::Low), None);
        assert_eq!(limits("claude-sonnet-4-5").clamp_effort(Effort::Max), None);
    }

    #[test]
    fn an_unknown_model_gets_the_conservative_floor_not_the_newest_row() {
        let l = limits("claude-opus-9");
        assert_eq!(l, UNKNOWN);
        assert_eq!(l.clamp_max_tokens(32_000), 8_192);
        assert_eq!(l.clamp_effort(Effort::XHigh), None);
    }

    #[test]
    fn a_dated_snapshot_resolves_to_its_alias() {
        assert_eq!(
            limits("claude-haiku-4-5-20251001"),
            limits("claude-haiku-4-5")
        );
        assert_eq!(limits("claude-opus-4-5-20251101"), UNKNOWN);
    }

    #[test]
    fn no_row_shadows_another() {
        // Prefix matching is only unambiguous while no id prefixes another —
        // today none does, and `max_by_key` covers the day one is added. This
        // asserts the property rather than the tie-break, because a row like
        // `claude-opus` added later would silently swallow every Opus and no
        // other test in this file would notice.
        for (a, _) in TABLE {
            for (b, _) in TABLE {
                assert!(
                    a == b || !b.starts_with(a),
                    "{b} is shadowed by the shorter row {a}"
                );
            }
        }
        assert_eq!(limits("claude-opus-4-8").efforts, ALL_FIVE);
        assert_eq!(limits("claude-opus-4-6").efforts, NO_XHIGH);
    }

    #[test]
    fn a_request_below_every_supported_level_sends_no_effort() {
        // Contrived, because no shipped model starts above `low` — but the
        // rule it pins is that clamping never goes upward.
        let l = Limits {
            max_tokens: 1_000,
            efforts: &[Effort::High, Effort::Max],
        };
        assert_eq!(l.clamp_effort(Effort::Low), None);
        assert_eq!(l.clamp_effort(Effort::XHigh), Some(Effort::High));
    }
}

// endregion: Tests
