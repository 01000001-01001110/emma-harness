//! One real call to OpenRouter, ignored by default.
//!
//! Everything in `openai_compat`'s own test module drives a loopback socket, and
//! that is the right default: those claims are about the bytes this crate writes
//! and reads, and a live endpoint would test OpenRouter's uptime instead. What a
//! loopback stub cannot do is catch the class of defect this project has already
//! paid for twice — a decoder that passes every fixture and then meets a real
//! body with a key nobody modelled. Fixtures agree with their author.
//!
//! So this exists, `#[ignore]`d, to be run by hand when the wire is what is in
//! question:
//!
//! ```text
//! OPENROUTER_API_KEY=… cargo test -p emma-llm --test openrouter_live -- --ignored --nocapture
//! ```
//!
//! **It picks its model from the live roster rather than naming one.** A
//! hardcoded free id rots — OpenRouter renames models and retires the free tier
//! on individual ones without notice — and a test that fails because its model
//! moved teaches nothing about the decoder. `roster::fetch` already answers
//! "which ids cost nothing and accept tools"; this asks it. `EMMA_LIVE_MODEL`
//! overrides the choice for a run against a specific id.
//!
//! **The free tier is rate-limited hard.** `roster.rs` measured two of four live
//! calls answering 429 within a minute on 2026-08-30. A 429 here is reported as
//! what it is rather than being retried into a pass or asserted away: the call
//! reached the host, which is most of what was being asked.

use emma_llm::openai_compat::{OpenAiCompatProvider, OPENROUTER};
use emma_llm::roster::{fetch, OPENROUTER_BASE_URL};
use emma_llm::{ApiKey, LlmError, Message, Mode, Provider, Request};

/// The key, or a message saying how to supply one. Never read from a file here:
/// a test that goes looking on disk for a credential is a test that can leak one
/// into its own output on the day the path is wrong.
fn live_key() -> ApiKey {
    ApiKey::new(
        std::env::var(OPENROUTER.env_var)
            .expect("set OPENROUTER_API_KEY to run this; it is never read from a file here"),
    )
}

/// Ids the live roster says are free, tool-capable and text-producing, today.
///
/// Naming one in the source rots, and this is measured rather than argued:
/// `meta-llama/llama-3.3-70b-instruct:free` was the obvious hardcoded choice on
/// 2026-09-06 and answered *"This model is unavailable for free. The paid
/// version is available now"* — a 400, on a real call, for a model that a
/// fixture would have happily pretended still existed.
async fn usable_free_ids(key: &ApiKey, want: usize) -> Vec<String> {
    if let Ok(id) = std::env::var("EMMA_LIVE_MODEL") {
        return vec![id];
    }
    let roster = fetch(OPENROUTER_BASE_URL, key)
        .await
        .expect("the roster is what names a currently-free model");
    let ids: Vec<String> = roster.usable().take(want).map(|m| m.id.clone()).collect();
    assert!(!ids.is_empty(), "no free tool-capable model on the roster");
    println!("roster offered: {ids:?}");
    ids
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "reaches the real OpenRouter; run by hand with a key in the environment"]
async fn a_free_model_answers_and_reports_what_it_billed() {
    let key = live_key();
    let model = usable_free_ids(&key, 1).await.remove(0);

    let provider = OpenAiCompatProvider::new(OPENROUTER, key, Some(model.clone()));
    let mut request = Request::new("Answer in one short sentence.", Vec::new());
    request.max_tokens = 64;
    request.query = vec![Message::user("Say the single word: certified.")];

    match provider.send(request, Mode::Batch, None).await {
        Ok(turn) => {
            println!("model:  {model}");
            println!("stop:   {}", turn.stop_reason);
            println!("text:   {}", turn.text());
            println!("usage:  {:?}", turn.usage);
            assert!(!turn.text().trim().is_empty(), "an empty turn: {turn:?}");
            // The mapping this crate makes, checked against a real body rather
            // than a fixture: what the host billed as `prompt_tokens` is what
            // `billable_input_tokens()` reports, however the cache split fell.
            assert_eq!(
                turn.usage.billable_input_tokens(),
                turn.usage.input_tokens + turn.usage.cache_read_input_tokens
            );
            assert!(turn.usage.output_tokens > 0, "{:?}", turn.usage);
        }
        // Not a pass, and not a failure of this crate either. Said plainly.
        Err(e @ LlmError::RateLimited { .. }) => panic!(
            "the free tier refused this call: {e}\n\
             The request reached the host; nothing about the decoder was tested."
        ),
        Err(e) => panic!("{e}"),
    }
}

/// The half a fixture cannot certify: a real host emitting a real tool call.
///
/// The scar this answers is on the record twice in this project — a strict wire
/// decoder that passed every test and then decoded no real tool call, because
/// the API put a key on the block that nobody had modelled. `arguments` being a
/// JSON *string* rather than an object is the same shape of assumption, and the
/// only place to check it is against a host that actually sends one.
///
/// A small free model that answers in prose rather than calling the tool is not
/// a failure of this crate, so the run walks the roster until one calls or the
/// candidates are spent, and says which happened. What is asserted is the
/// decode, on whichever model actually produced a call.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "reaches the real OpenRouter; run by hand with a key in the environment"]
async fn a_real_tool_call_decodes_into_arguments_the_loop_can_use() {
    let key = live_key();
    let candidates = usable_free_ids(&key, 4).await;

    for model in &candidates {
        let provider = OpenAiCompatProvider::new(OPENROUTER, key.clone(), Some(model.clone()));
        let mut request = Request::new(
            "Use the Read tool when asked to read a file. Do not answer in prose.",
            vec![serde_json::json!({
                "name": "Read",
                "description": "Read a file from disk.",
                "input_schema": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }
            })],
        );
        request.max_tokens = 128;
        request.query = vec![Message::user("Read the file at src/main.rs.")];

        let turn = match provider.send(request, Mode::Batch, None).await {
            Ok(turn) => turn,
            // A free model that has stopped being free, or is rate-limited, is
            // this roster's ordinary weather. Say so and try the next one.
            Err(e) => {
                println!("{model}: {e}");
                continue;
            }
        };
        let calls = turn.tool_calls();
        let Some(call) = calls.first() else {
            println!("{model}: answered in prose, no call to decode");
            continue;
        };
        println!("model: {model}");
        println!("stop:  {}", turn.stop_reason);
        println!("call:  {call:?}");
        assert_eq!(turn.stop_reason, "tool_use");
        assert_eq!(call.name, "Read");
        assert!(!call.id.is_empty(), "a call with no id: {call:?}");
        // The claim: `arguments` arrived as a JSON string on the wire and came
        // out as the object the loop hands a tool, not as the string itself.
        assert!(call.input.is_object(), "{:?}", call.input);
        return;
    }
    panic!("none of {candidates:?} emitted a tool call; the decoder was not exercised");
}
