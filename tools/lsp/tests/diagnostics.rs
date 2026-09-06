//! `Diagnostics`, and the distinction the whole tool exists to preserve.
//!
//! Diagnostics are the one thing in LSP that nobody asks for. The server
//! publishes when it feels like it, so "no diagnostics" arrives on the wire in
//! two forms that a careless client would flatten into one: an empty array,
//! which is a clean bill of health, and nothing at all, which is a question
//! that was never answered. A tool that reported the second as the first would
//! be inventing a clean result out of a timeout, and a model would ship on it.
//!
//! Every case here is driven through the fake, so none of it needs a language
//! server installed. `Publishes::Nothing` and `Publishes::Clean` are the two
//! that matter; they produce results with no diagnostics in them and they must
//! not read alike.

mod support;

use std::sync::Arc;

use emma_tool_api::Tool;
use emma_tools_lsp::client::{DIAGNOSTICS_WAIT_ENV, READY_TIMEOUT_ENV};
use emma_tools_lsp::{Diagnostics, Pool};
use serde_json::{json, Value};
use support::{Fake, Indexing, Publishes, Sandbox};

const SOURCE: &str = "pub fn main() {}\n";

/// The result with `\` rewritten to `/`.
///
/// `path::display` spells a relative path with the host separator, so
/// `src/main.rs` arrives as `src\main.rs` on Windows. Asserting the unix
/// spelling against a normalised copy keeps the fixtures readable and keeps the
/// assertion about the rendering rather than about the host.
fn slashed(out: &str) -> String {
    out.replace('\\', "/")
}

/// A sandbox with one file of `language`, and a pool holding a fake for it.
///
/// The wait is turned down to fifty milliseconds. The silence case would
/// otherwise cost the suite twenty seconds to prove that nothing happened.
async fn fixture(
    language: &str,
    relative: &str,
    publishes: Publishes,
    enabled: &[&str],
) -> (Sandbox, Arc<Pool>) {
    std::env::set_var(READY_TIMEOUT_ENV, "300");
    std::env::set_var(DIAGNOSTICS_WAIT_ENV, "50");
    let sandbox = Sandbox::new();
    sandbox.write(relative, SOURCE);
    let root = sandbox.canonical();

    let client = Fake::new(Indexing::Finishes)
        .publishing(publishes)
        .start_as(language, &root)
        .await;
    let pool = Arc::new(Pool::with_enabled(enabled.iter().map(|s| s.to_string())));
    pool.adopt(&root, client).await;
    (sandbox, pool)
}

async fn run(pool: Arc<Pool>, sandbox: &Sandbox, relative: &str) -> String {
    Diagnostics::new(pool)
        .invoke(&sandbox.ctx, json!({ "file_path": relative }))
        .await
        .expect("no fault")
        .expect("the fake answers")
        .content
}

/// **The core protocol path.** A publication arrives unasked-for, is matched to
/// the file that caused it, and comes back with positions a model can act on.
#[tokio::test]
async fn a_publication_is_matched_to_its_file_and_rendered_one_based() {
    let items = vec![json!({
        "range": { "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 8 } },
        "severity": 1,
        "source": "rustc",
        "code": "E0425",
        "message": "cannot find value `nope` in this scope",
    })];
    let (sandbox, pool) = fixture("rust", "src/main.rs", Publishes::These(items), &["rust"]).await;
    let out = slashed(&run(pool, &sandbox, "src/main.rs").await);

    assert!(out.contains("1 diagnostic(s)"), "{out}");
    // 0-based on the wire, 1-based in the result, matching Read and Grep.
    assert!(out.contains("1:5 error [rustc:E0425]"), "{out}");
    assert!(out.contains("cannot find value"), "{out}");
    assert!(out.contains("src/main.rs"), "{out}");
}

/// An empty publication is an answer, and says so in words that cannot be
/// confused with the silence case below.
#[tokio::test]
async fn an_empty_publication_is_a_clean_result_and_says_why() {
    let (sandbox, pool) = fixture("rust", "src/main.rs", Publishes::Clean, &["rust"]).await;
    let out = run(pool, &sandbox, "src/main.rs").await;

    assert!(out.contains("reported no problems"), "{out}");
    assert!(out.contains("published an empty diagnostic set"), "{out}");
    assert!(!out.contains("not a clean result"), "{out}");
}

/// **The case this tool would be dishonest without.** The server said nothing.
/// The result must not read as clean, must name the wait it gave up after, and
/// must say how to wait longer.
#[tokio::test]
async fn silence_is_not_a_clean_result() {
    let (sandbox, pool) = fixture("rust", "src/main.rs", Publishes::Nothing, &["rust"]).await;
    let out = run(pool, &sandbox, "src/main.rs").await;

    assert!(out.contains("published no diagnostics"), "{out}");
    assert!(out.contains("not a clean result"), "{out}");
    assert!(out.contains(DIAGNOSTICS_WAIT_ENV), "{out}");
    // The sentence the clean case uses must be nowhere near this one.
    assert!(!out.contains("reported no problems"), "{out}");
}

/// The two empty results are different strings. Stated as its own test because
/// it is the invariant, and a future edit that makes them converge would
/// otherwise pass every test above.
#[tokio::test]
async fn the_two_empty_results_do_not_read_alike() {
    let (a_box, a_pool) = fixture("rust", "src/main.rs", Publishes::Clean, &["rust"]).await;
    let clean = run(a_pool, &a_box, "src/main.rs").await;
    let (b_box, b_pool) = fixture("rust", "src/main.rs", Publishes::Nothing, &["rust"]).await;
    let silent = run(b_pool, &b_box, "src/main.rs").await;
    assert_ne!(clean, silent);
}

/// A cut names the cap, the loss and the remedy — the same obligation the other
/// two renderers carry, asserted on the outcome rather than on the source.
///
/// A generated file or an uninitialised terraform directory produces hundreds
/// of diagnostics, so this cap is reached in ordinary use rather than in a
/// pathological one, and `MAX_DIAGNOSTICS` of them with nothing said about the
/// rest is a result that reads complete.
#[tokio::test]
async fn a_capped_publication_names_the_cap_the_loss_and_the_remedy() {
    let max = emma_tools_lsp::render::MAX_DIAGNOSTICS;
    let items: Vec<Value> = (0..max + 7)
        .map(|i| {
            json!({
                "range": { "start": { "line": i, "character": 0 } },
                "severity": 2,
                "message": format!("problem {i}"),
            })
        })
        .collect();
    let (sandbox, pool) = fixture("rust", "src/main.rs", Publishes::These(items), &["rust"]).await;
    let out = Diagnostics::new(pool)
        .invoke(&sandbox.ctx, json!({ "file_path": "src/main.rs" }))
        .await
        .expect("no fault")
        .expect("the fake answers");

    assert!(out.truncated, "a capped result must be marked truncated");
    let why = out
        .truncation
        .as_deref()
        .expect("a capped diagnostics result must name its limit, not just claim a cut");
    assert!(why.contains(&max.to_string()), "the cap: {why:?}");
    assert!(why.contains(&(max + 7).to_string()), "the loss: {why:?}");
    assert!(
        why.contains("No argument raises"),
        "the remedy, or the honest statement that there is none: {why:?}"
    );

    // The positive control. A renderer that flagged everything truncated would
    // satisfy every assertion above.
    let (sandbox, pool) = fixture(
        "rust",
        "src/main.rs",
        Publishes::These(vec![json!({
            "range": { "start": { "line": 0, "character": 0 } },
            "message": "just the one",
        })]),
        &["rust"],
    )
    .await;
    let out = Diagnostics::new(pool)
        .invoke(&sandbox.ctx, json!({ "file_path": "src/main.rs" }))
        .await
        .expect("no fault")
        .expect("the fake answers");
    assert!(
        !out.truncated,
        "a result that fitted was reported as cut: {:?}",
        out.truncation
    );
}

/// A language that is wired and switched off is refused by name, with the fix.
///
/// Terraform is the case with a reason attached: it is off by default because
/// its server may reach the network, and these tools declare that they do not.
#[tokio::test]
async fn a_disabled_language_is_refused_naming_the_settings_key() {
    let (sandbox, pool) = fixture(
        "terraform",
        "main.tf",
        Publishes::Clean,
        // The default set, which does not include terraform.
        &["rust", "bash", "powershell", "python"],
    )
    .await;
    let err = Diagnostics::new(pool)
        .invoke(&sandbox.ctx, json!({ "file_path": "main.tf" }))
        .await
        .expect("no fault")
        .expect_err("terraform is off");
    assert_eq!(err.kind(), "tool_unavailable");
    assert!(err.detail().contains("lsp.enabled"), "{err}");
    assert!(err.detail().contains("\"terraform\""), "{err}");
    assert!(err.detail().contains("reach the network"), "{err}");
}

/// The same language, enabled, answers. Without this the test above would pass
/// for a build where terraform simply did not work.
#[tokio::test]
async fn the_same_language_enabled_answers() {
    let items: Vec<Value> = vec![json!({
        "range": { "start": { "line": 2, "character": 0 } },
        "severity": 2,
        "source": "terraform-ls",
        "message": "Missing required argument",
    })];
    let (sandbox, pool) = fixture(
        "terraform",
        "main.tf",
        Publishes::These(items),
        &["rust", "terraform"],
    )
    .await;
    let out = run(pool, &sandbox, "main.tf").await;
    assert!(out.contains("3:1 warning [terraform-ls]"), "{out}");
    assert!(out.contains("Missing required argument"), "{out}");
    // The language's own caveat travels with every result it produced.
    assert!(out.contains(".terraform/"), "{out}");
}

/// YAML is not automatically Ansible, and the refusal teaches the rule.
#[tokio::test]
async fn yaml_outside_an_ansible_tree_is_refused_with_the_reason() {
    let (sandbox, pool) = fixture(
        "ansible",
        "docker-compose.yml",
        Publishes::Clean,
        &["ansible"],
    )
    .await;
    let err = Diagnostics::new(pool)
        .invoke(&sandbox.ctx, json!({ "file_path": "docker-compose.yml" }))
        .await
        .expect("no fault")
        .expect_err("a compose file is not a playbook");
    assert!(err.detail().contains("ansible.cfg"), "{err}");
    assert!(err.detail().contains("playbooks"), "{err}");
}

/// The same file under `playbooks/` is Ansible's, which is the other half of
/// the heuristic and the half that has to keep working.
#[tokio::test]
async fn yaml_inside_an_ansible_tree_is_served() {
    let (sandbox, pool) = fixture(
        "ansible",
        "playbooks/site.yml",
        Publishes::Clean,
        &["ansible"],
    )
    .await;
    let out = run(pool, &sandbox, "playbooks/site.yml").await;
    assert!(out.contains("reported no problems"), "{out}");
}

/// The editor's half of the same map: `published_for` reads what arrived
/// without waiting, and `did_close` forgets it.
///
/// This is what the Code page's LSP bridge consumes, and none of the tool tests
/// reach it. The rule is the `Option`'s, again: after a close, the answer is
/// `None` — "nothing has been published" — and not an empty list.
#[tokio::test]
async fn the_editor_half_reads_the_map_and_a_close_forgets_it() {
    let items = vec![json!({
        "range": { "start": { "line": 0, "character": 0 } },
        "severity": 1,
        "message": "something",
    })];
    let (sandbox, pool) = fixture("rust", "src/main.rs", Publishes::These(items), &["rust"]).await;
    let file = sandbox.canonical().join("src").join("main.rs");
    let uri = emma_tools_lsp::doc::to_uri(&file);

    // Nothing has been sent yet, so nothing has been published.
    let client = pool
        .client(
            &sandbox.canonical(),
            emma_tools_lsp::lang::by_key("rust").unwrap(),
        )
        .await
        .expect("adopted");
    assert_eq!(client.published_for(&uri), None);

    // A tool call syncs the document, which is what makes the fake publish.
    let _ = run(pool.clone(), &sandbox, "src/main.rs").await;
    assert_eq!(
        client.published_for(&uri).map(|v| v.len()),
        Some(1),
        "the publication was not recorded for the URI that caused it"
    );

    client.did_close(&file);
    assert_eq!(
        client.published_for(&uri),
        None,
        "a closed document's diagnostics describe a version nothing is looking at"
    );
}

/// **The defect no fake caught until the fake was taught to lie the way a real
/// server does.**
///
/// rust-analyzer spells the Windows drive letter lower case in everything it
/// sends back, so a publication for `file:///C:/w/src/main.rs` arrives as
/// `file:///c:/w/src/main.rs`. Keyed on the raw URI string those are two
/// different files, and `Diagnostics` reports the honest silence sentence
/// forever about a file the server has already answered about — the worst
/// possible shape, because the result is a correct-looking "I was told
/// nothing".
///
/// Measured on 2026-09-06 against the real server: four `publishDiagnostics`
/// notifications received, two twenty-second waits, both reported as silence.
///
/// This is the one case in the file that must be driven with
/// [`Fake::spelling_uris_as_a_real_server_does`], because a fake that echoes
/// the client's own URI makes the bug invisible.
#[tokio::test]
async fn a_publication_is_found_when_the_server_spells_the_uri_its_own_way() {
    std::env::set_var(READY_TIMEOUT_ENV, "300");
    std::env::set_var(DIAGNOSTICS_WAIT_ENV, "50");
    let sandbox = Sandbox::new();
    sandbox.write("src/main.rs", SOURCE);
    let root = sandbox.canonical();

    let client = Fake::new(Indexing::Finishes)
        .publishing(Publishes::These(vec![json!({
            "range": { "start": { "line": 0, "character": 0 } },
            "severity": 1,
            "message": "spelled differently on the way back",
        })]))
        .spelling_uris_as_a_real_server_does()
        .start_as("rust", &root)
        .await;
    let pool = Arc::new(Pool::with_enabled(["rust".to_string()]));
    pool.adopt(&root, client).await;

    let out = run(pool, &sandbox, "src/main.rs").await;
    assert!(
        !out.contains("published no diagnostics"),
        "a publication that arrived was not found, because the server spelled \
         the URI its own way: {out}"
    );
    assert!(out.contains("spelled differently on the way back"), "{out}");
}
