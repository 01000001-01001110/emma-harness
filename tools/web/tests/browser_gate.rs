//! The decisions the five browser tools make before a browser is involved.
//!
//! Everything here runs with **no Chrome and no network**, and that is a
//! constraint rather than a convenience: the answers being pinned are the ones
//! the approval gate reads *before* the call — `network_target`, `ToolMeta`,
//! and `validate_args`. A test that had to start a browser to establish them
//! would be certifying the browser, not the consent boundary.
//!
//! `tests/browser_lifecycle.rs` covers what needs a real process (a Chrome
//! never outlives the run) and `tests/browser_live.rs` covers what needs the
//! real internet (a real site's selectors resolve). Neither of those can say
//! whether the gate was asked the right question, because by the time they run
//! the question has already been answered.
//!
//! **Every refusal here has its ordinary case asserted next to it.** A gate
//! that refuses everything satisfies a suite of refusals, and this crate's own
//! record already contains a source-grep test that passed while the thing it
//! guarded was switched off.

use emma_tool_api::{Tool, ToolCtx, ToolError};
use emma_tools_web::browser::browser_tools;
use emma_tools_web::BrowserPool;
use serde_json::json;
use std::sync::Arc;

fn ctx() -> ToolCtx {
    ToolCtx {
        cwd: std::env::temp_dir(),
        session_id: "gate".into(),
        turn_id: "turn-1".into(),
        background: Default::default(),
    }
}

/// The five, with the pool they share. No allowlist, which is the shape a user
/// who has never written one is in.
fn surface() -> (Vec<Arc<dyn Tool>>, Arc<BrowserPool>) {
    browser_tools(None)
}

fn tool(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
    tools
        .iter()
        .find(|t| t.name() == name)
        .unwrap_or_else(|| panic!("no {name} in the browser surface"))
        .clone()
}

// region: BrowserOpen names where it is going
// ---------------------------------------------------------------------------
// BrowserOpen names where it is going
//
// The only one of the five whose destination is in its own arguments, and the
// only one whose `network_target` had no test at all.
// ---------------------------------------------------------------------------

/// **What breaks if this fails:** the user is asked to approve a browser
/// starting, and the prompt names the wrong host — or names none, in which case
/// `approval::egress` fail-closes and `BrowserOpen` stops working entirely with
/// a message blaming a defect in the tool.
///
/// The host is read from the URL, not from the string: `https://good.example@evil.example/`
/// is a userinfo trick that reads as `good.example` to a person skimming and
/// resolves to `evil.example`. `url::Url` is what settles it, and this asserts
/// the tool asks it rather than doing its own splitting.
#[test]
fn opening_asks_about_the_host_of_the_url_it_was_given() {
    let (tools, _pool) = surface();
    let open = tool(&tools, "BrowserOpen");

    let target = open
        .network_target(&json!({ "url": "https://example.com/some/page?q=1" }))
        .expect("BrowserOpen named no host for an ordinary URL");
    assert_eq!(target.host, "example.com");
    assert!(
        target.detail.contains("https://example.com/some/page?q=1"),
        "the prompt does not show which URL is about to be opened: {}",
        target.detail
    );

    assert_eq!(
        open.network_target(&json!({ "url": "https://good.example@evil.example/" }))
            .expect("no host")
            .host,
        "evil.example",
        "the host was taken from the text of the URL rather than from parsing it"
    );

    // A URL with no host cannot be approved, and saying so is the fail-closed
    // answer `approval::egress` is built around. Guessing a host here would be
    // the one genuinely dangerous alternative.
    for hostless in ["file:///etc/passwd", "not a url", ""] {
        assert!(
            open.network_target(&json!({ "url": hostless })).is_none(),
            "BrowserOpen invented a host for {hostless:?}"
        );
    }

    // `chrome://settings` is the odd one and is left asserted rather than
    // tidied: `url::Url` parses it and hands back `settings` as the host, so the
    // prompt would say "settings" and a remembered rule would be written against
    // a host that does not exist. It is harmless only because the scheme is
    // refused one layer down, before Chrome starts — which the next test
    // asserts. If that ordering ever changes, this line is the reminder that
    // `network_target` never looked at the scheme.
    assert_eq!(
        open.network_target(&json!({ "url": "chrome://settings" }))
            .expect("no host")
            .host,
        "settings"
    );
}

/// **What breaks if this fails:** a malformed call reaches `pool.open`, which
/// starts a Chrome to discover the argument was wrong — a 300 MB process and a
/// cold start spent on a typo.
#[test]
fn opening_refuses_a_call_it_cannot_act_on_and_accepts_an_ordinary_one() {
    let (tools, _pool) = surface();
    let open = tool(&tools, "BrowserOpen");

    let refused = [
        (json!({}), "requires url"),
        (json!({ "url": "" }), "empty"),
        (json!({ "url": "   " }), "empty"),
        (
            json!({ "url": "https://example.com/", "headful": "yes" }),
            "headful",
        ),
        (
            json!({ "url": "https://example.com/", "headfull": true }),
            "headfull",
        ),
        (json!({ "url": 7 }), "string"),
    ];
    for (args, expected) in refused {
        let err = open
            .validate_args(&args)
            .expect_err(&format!("{args} validated"));
        assert_eq!(err.kind(), "bad_arguments", "{args}");
        assert!(
            err.detail().contains(expected),
            "the message did not say what was wrong ({expected}): {}",
            err.detail()
        );
    }

    // The control. Without these two lines the assertions above are satisfied
    // by a `validate_args` that returns an error unconditionally.
    open.validate_args(&json!({ "url": "https://example.com/" }))
        .expect("an ordinary open was refused");
    open.validate_args(&json!({ "url": "https://example.com/", "headful": true }))
        .expect("a headful open was refused");
}

/// **What breaks if this fails:** `file:///`, `chrome://` and loopback URLs
/// reach a real browser. The refusal has to happen *before* the process starts,
/// because a refusal after it is a browser that was launched at a model's
/// request pointing at the user's disk.
///
/// The pool being empty afterwards is the half that says nothing was started.
#[tokio::test]
async fn opening_a_refused_url_starts_no_browser() {
    let (tools, pool) = surface();
    let open = tool(&tools, "BrowserOpen");

    for url in [
        "file:///etc/passwd",
        "chrome://settings",
        "http://localhost:8080/admin",
        "http://127.0.0.1:9/x",
        "ftp://example.com/x",
    ] {
        let err = open
            .invoke(&ctx(), json!({ "url": url }))
            .await
            .expect("a refusal must not end the turn")
            .expect_err("a refused URL opened a browser");
        assert_eq!(err.kind(), "bad_arguments", "{url}: {err}");
        assert!(
            err.detail().contains("refused"),
            "{url}: the message does not say it was refused: {}",
            err.detail()
        );
    }
    assert!(
        pool.is_empty(),
        "a refused open left a session in the registry, which means a Chrome was started"
    );

    // The control, and it cannot be an `invoke` — an accepted URL launches a
    // browser, which is the thing this file will not do. So it is asserted one
    // layer down, against the same policy object the open path uses: an
    // ordinary https URL is not refused.
    pool.policy()
        .expect("policy")
        .check("https://example.com/")
        .expect("an ordinary https URL was refused by the same check");
}

// endregion: BrowserOpen names where it is going

// region: Acting is confined to the user's own domain list
// ---------------------------------------------------------------------------
// Acting is confined to the user's own domain list
//
// chromehand's ADR-4, which `BrowserAct` and `BrowserFill` inherit by passing
// `BrowserPool::policy()` into `actions::click` and `forms::fill`. The pool's
// own test covers the no-allowlist refusal; what was missing was the half that
// says the list is a list rather than a switch.
// ---------------------------------------------------------------------------

/// **What breaks if this fails:** either the model can click and type on any
/// site on the internet — the allowlist stops being a boundary — or it can act
/// on none of them, and the feature is dead while its tests stay green.
///
/// The subdomain case is the one worth stating: `{"domains": ["example.com"]}`
/// covers `www.example.com`, and must not cover `example.com.evil.test`, which
/// a naive `contains` or `ends_with` on the bare domain would wave through.
#[test]
fn acting_is_allowed_on_the_listed_domains_and_refused_off_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let list = dir.path().join("browser-allowlist.json");
    std::fs::write(&list, r#"{ "domains": ["example.com", "iana.org"] }"#).unwrap();

    let pool = BrowserPool::new(Some(list.clone()));
    let policy = pool.policy().expect("the allowlist did not load");

    for allowed in [
        "https://example.com/x",
        "https://www.example.com/x",
        "https://deep.sub.example.com/x",
        "https://iana.org/",
    ] {
        policy
            .check_interaction(allowed)
            .unwrap_or_else(|e| panic!("{allowed} is on the list and was refused: {e}"));
    }

    for refused in [
        "https://evil.test/x",
        "https://example.com.evil.test/x",
        "https://notexample.com/x",
    ] {
        let why = policy
            .check_interaction(refused)
            .expect_err(&format!("{refused} is not on the list and was allowed"));
        assert!(
            why.contains("allowlist"),
            "{refused}: the refusal does not say why: {why}"
        );
        assert!(
            why.contains(&list.display().to_string()),
            "{refused}: the refusal does not name the file to edit: {why}"
        );
    }

    // **And once a list exists it binds reading too**, which is not what the
    // asymmetry in `BrowserPool::policy`'s doc reads like at a glance: the doc
    // says reading is unrestricted *without* an allowlist, and `Policy::check`
    // runs `check_allowlist` unconditionally. So writing a list to enable
    // clicking on one site also stops `BrowserRead` and `WebFetch` reaching
    // every other site. That is a defensible design and a surprising one, and it
    // is asserted here so it is a decision somebody made rather than a thing
    // nobody noticed.
    let why = policy
        .check("https://evil.test/x")
        .expect_err("an allowlist stopped binding reads");
    assert!(why.contains("not in allowlist"), "{why}");
}

/// **What breaks if this fails:** a user who has never written an allowlist
/// gets an unbounded `BrowserAct`, and the second key the design leans on —
/// "a file in `~/.emma/` that no answer at a prompt can write" — is not there.
///
/// The message has to name the file, because a refusal the user cannot act on
/// is indistinguishable from the feature being broken.
#[test]
fn with_no_allowlist_acting_is_refused_and_reading_is_not() {
    let policy = BrowserPool::new(None).policy().expect("policy");
    let why = policy
        .check_interaction("https://example.com/")
        .expect_err("interaction was allowed with no allowlist in force");
    assert!(why.contains("allowlist"), "{why}");
    assert!(
        why.contains("browser-allowlist.json"),
        "the refusal does not name a file the user could create: {why}"
    );
    policy
        .check("https://example.com/")
        .expect("reading was refused with no allowlist, which is not ADR-4");
}

// endregion: Acting is confined to the user's own domain list

// region: BrowserClose is the one that must never ask
// ---------------------------------------------------------------------------
// BrowserClose is the one that must never ask
//
// Its `meta` carries the argument in full: a prompt on cleanup answered `n`
// leaves a browser running with an unauthenticated CDP port. What had no test
// was the other half of that — that it also never *fails*, because a close
// that errors is a close the model retries instead of a machine that is tidy.
// ---------------------------------------------------------------------------

/// **What breaks if this fails:** cleanup starts asking. Either
/// `reaches_network` flips, or `network_target` starts answering, and either
/// one routes `BrowserClose` into `approval::egress` — where the user can say
/// no to killing a browser that is holding a remote-control port open.
#[test]
fn closing_has_nothing_for_the_gate_to_ask_about() {
    let (tools, _pool) = surface();
    let close = tool(&tools, "BrowserClose");
    assert!(!close.meta().reaches_network);
    assert!(close.meta().read_only);
    assert!(
        close.network_target(&json!({ "session": "s1" })).is_none(),
        "BrowserClose named a host, which is what puts a call in front of the egress gate"
    );
    assert!(close.network_target(&json!({})).is_none());
}

/// **What breaks if this fails:** the model loops. An id that is already gone is
/// the state the caller wanted, and returning `is_error` for it teaches the
/// model to retry a close that has already happened — burning turns at the end
/// of every goal, which is exactly where nobody is watching.
#[tokio::test]
async fn closing_something_that_is_not_there_is_a_result_and_not_an_error() {
    let (tools, pool) = surface();
    let close = tool(&tools, "BrowserClose");
    assert!(pool.is_empty());

    let by_id = close
        .invoke(&ctx(), json!({ "session": "gone" }))
        .await
        .expect("close must not end the turn")
        .expect("closing an unknown session was reported as a tool error");
    assert!(
        by_id.content.contains("gone"),
        "the answer does not say which session: {}",
        by_id.content
    );

    let all = close
        .invoke(&ctx(), json!({}))
        .await
        .expect("close must not end the turn")
        .expect("closing an empty pool was reported as a tool error");
    assert!(
        all.content.to_lowercase().contains("no browser sessions"),
        "{}",
        all.content
    );

    // …and it still refuses arguments it does not understand, so "never errors"
    // does not quietly become "never reads its arguments". A `sessions: [...]`
    // that is ignored is a close that silently closes everything.
    let err: ToolError = close
        .validate_args(&json!({ "sessions": ["a", "b"] }))
        .expect_err("an unknown parameter was ignored");
    assert!(err.detail().contains("sessions"), "{}", err.detail());
}

// endregion: BrowserClose is the one that must never ask
