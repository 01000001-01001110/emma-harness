//! Offline fixture tests: a tiny local HTTP server serves tests/fixtures/*,
//! and the COMPILED BINARY is exercised end-to-end — policy refusals, the
//! stateless digest, and the full session loop (open → navigate → digest →
//! click → wait-for → type → select → refusals → history → screenshot →
//! close). Local URLs require --allow-local (the test escape hatch the policy
//! exists for); interaction verbs additionally get a fixture allowlist.
//!
//! Chrome must be installed (same requirement as the binary itself). No
//! network access: everything is served from 127.0.0.1.
//!
//! **Vendored with the fork** — see `VENDOR.md`. These are the tests that lock
//! the safety hardening in place: the localhost and scheme refusals, the allowlist
//! requirement on interaction verbs, `click` refusing submit controls, `type`
//! and `fill` refusing password fields and newline injection, and the two-key
//! auto-submit rule. None of that is reachable from an Emma tool in this pass
//! and it is tested anyway, because hardening whose tests were left behind is
//! hardening nobody can trust later.
//!
//! They drive the CLI rather than the library on purpose: that is how they
//! were written, it is the layer where the exit-code contract is observable,
//! and rewriting fifty assertions during a fork is how a suite quietly stops
//! testing what it used to.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;

// region: The fixture server
// ---------------------------------------------------------------------------
// The fixture server
//
// Scaffolding, and more of it than usual for a reason: this suite drives a
// real Chrome against a real socket, so the server has to survive what a
// browser actually does — speculative connections that send nothing, POST
// bodies split across writes. Both of those cost a debugging session and the
// comments below are what remains of them.
// ---------------------------------------------------------------------------

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_browser-miner")
}

/// Minimal fixture HTTP server on an ephemeral port; serves tests/fixtures/.
fn serve_fixtures() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            // One thread per connection: Chrome opens speculative sockets that
            // send nothing — serially blocking on them starves real requests
            // (root cause of a CI-only flake; 5s read timeout as backstop).
            std::thread::spawn(move || handle_conn(stream));
        }
    });
    (port, handle)
}

fn handle_conn(mut stream: std::net::TcpStream) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    // Read until end of headers, then any Content-Length body (POSTs
    // from Chrome arrive in multiple writes; a single read + close
    // yields chrome-error:// pages).
    let mut raw = Vec::new();
    let mut buf = [0u8; 2048];
    let header_end = loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break None;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break Some(pos + 4);
        }
        if raw.len() > 65536 {
            break None;
        }
    };
    let Some(header_end) = header_end else { return };
    let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let content_length: usize = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while raw.len() < header_end + content_length {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
    }
    let path = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/");
    if path == "/submit" {
        let body = b"<!doctype html><html><head><title>Submitted</title></head><body><main>SUBMITTED_MARKER</main></body></html>";
        let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
        let _ = stream.write_all(body);
        return;
    }
    let file = match path.trim_start_matches('/') {
        "" | "form.html" => "form.html",
        "page2.html" => "page2.html",
        "shadow.html" => "shadow.html",
        "framed.html" => "framed.html",
        "wizard.html" => "wizard.html",
        "no-submit.html" => "no-submit.html",
        "blocked.html" => "blocked.html",
        "long-title.html" => "long-title.html",
        _ => "",
    };
    let fixture_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let (status, body) = if file.is_empty() {
        ("404 Not Found".to_string(), Vec::new())
    } else {
        match std::fs::read(fixture_dir.join(file)) {
            Ok(b) => ("200 OK".to_string(), b),
            Err(_) => ("404 Not Found".to_string(), Vec::new()),
        }
    };
    let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    status,
                    body.len()
                )
                .as_bytes(),
            );
    let _ = stream.write_all(&body);
}

fn run(args: &[&str]) -> (i32, serde_json::Value, String) {
    run_in(None, args)
}

fn run_in(cwd: Option<&std::path::Path>, args: &[&str]) -> (i32, serde_json::Value, String) {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let out = cmd.output().expect("spawn binary");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let json = serde_json::from_str(&stdout)
        .or_else(|_| serde_json::from_str(&stderr))
        .unwrap_or(serde_json::Value::Null);
    (out.status.code().unwrap_or(-1), json, stdout + &stderr)
}

fn allowlist_file(dir: &std::path::Path) -> String {
    let p = dir.join("allowlist.json");
    std::fs::write(&p, r#"{ "domains": ["127.0.0.1"] }"#).unwrap();
    p.display().to_string()
}

// endregion: The fixture server

// region: Policy refusals, before Chrome starts
// ---------------------------------------------------------------------------
// Policy refusals: no Chrome, no network
//
// The cheapest and most important tests in the file. Each one asserts that a
// URL the tool may not touch is rejected by policy alone — no browser is
// launched, nothing is requested — and that the refusal exits 2 rather than 3,
// because a refusal is a decision and not a malfunction.
// ---------------------------------------------------------------------------

#[test]
fn policy_refuses_localhost_without_allow_local() {
    let (code, json, _) = run(&["verify", "http://localhost:9/x"]);
    assert_eq!(code, 2);
    assert_eq!(json["error"], "policy_refused");
}

#[test]
fn policy_refuses_non_http_scheme() {
    let (code, _, raw) = run(&["digest", "file:///etc/passwd"]);
    assert_eq!(code, 2, "raw: {}", raw);
}

#[test]
fn policy_refuses_domain_not_in_allowlist() {
    let dir = std::env::temp_dir().join(format!("bm-test-allow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let allow = allowlist_file(&dir);
    let (code, json, _) = run(&["verify", "https://example.org/", "--allowlist", &allow]);
    assert_eq!(code, 2);
    assert!(json["detail"]
        .as_str()
        .unwrap()
        .contains("not in allowlist"));
}

#[test]
fn interaction_verbs_require_allowlist() {
    // Read the name with care: what this actually pins is that an interaction
    // verb naming a session that does not exist exits 2 rather than 0 or 3 —
    // session connect happens before the allowlist check, so the refusal
    // observed here is the missing session, not the missing allowlist. The
    // allowlist rule itself is asserted against a live session in
    // `interaction_without_allowlist_refused_on_live_session` below. Both are
    // needed; neither covers the other.
    let (code, _, _) = run(&["click", "--session", "nonexistent", "--selector", "#x"]);
    assert_eq!(code, 2);
}

// endregion: Policy refusals, before Chrome starts

// region: Reading a page, and driving a session
// ---------------------------------------------------------------------------
// The full loop against the fixture server (needs Chrome)
//
// From here down every test launches a real browser. This first group covers
// the stateless read and then the session loop end to end — navigate, wait,
// digest, click, type, select, back, screenshot — plus the delta path that
// makes a long session affordable. Between them they prove that state persists
// across processes, which is the entire premise of session mode.
// ---------------------------------------------------------------------------

/// The path Emma's `WebFetch` would actually take, one layer down: launch,
/// render, extract, tear down. Note the password assertion — the field is
/// INVENTORIED and that is correct. Refusing to report that a login form
/// exists would make the page read dishonest; what is refused is typing into
/// it, which `session_loop_click_type_select_refusals_history` covers.
#[test]
fn stateless_digest_and_verify_on_fixture() {
    let (port, _h) = serve_fixtures();
    let url = format!("http://127.0.0.1:{}/form.html", port);

    let (code, d, raw) = run(&["digest", &url, "--allow-local"]);
    assert_eq!(code, 0, "raw: {}", raw);
    assert_eq!(d["outcome"], "verified");
    assert_eq!(d["http_status"], 200);
    let text = d["digest"]["text"].as_str().unwrap();
    assert!(text.contains("Main content paragraph"));
    assert!(
        !text.contains("Boilerplate nav"),
        "nav must be stripped from text"
    );
    let fields = d["digest"]["interactive"]["fields"].as_array().unwrap();
    assert!(fields.iter().any(|f| f["id"] == "name"));
    assert!(
        fields.iter().any(|f| f["type"] == "password"),
        "password INVENTORIED (typing into it is what's refused)"
    );
    let buttons = d["digest"]["interactive"]["buttons"].as_array().unwrap();
    assert!(buttons.iter().any(|b| b["label"] == "Show more"));

    let (code, v, _) = run(&["verify", &url, "--allow-local"]);
    assert_eq!(code, 0);
    assert_eq!(v["outcome"], "verified");
    assert_eq!(v["looks_blocked"], false);
    assert!(
        v.get("digest").is_none(),
        "verify must omit the digest payload"
    );
}

#[test]
fn session_loop_click_type_select_refusals_history() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);
    let dir = std::env::temp_dir().join(format!("bm-test-loop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let allow = allowlist_file(&dir);
    let shot = dir.join("shot.png").display().to_string();

    // open
    let (code, s, raw) = run(&["session", "open"]);
    assert_eq!(code, 0, "session open failed: {}", raw);
    let id = s["id"].as_str().expect("session id").to_string();

    let r = |args: &[&str]| {
        let mut full: Vec<&str> = args.to_vec();
        full.extend_from_slice(&["--session", &id, "--allow-local", "--allowlist", &allow]);
        run(&full)
    };

    // navigate → observed 200
    let (code, nav, raw) = r(&["navigate", &format!("{}/form.html", base)]);
    assert_eq!(code, 0, "navigate: {}", raw);
    assert_eq!(nav["http_status"], 200);
    assert_eq!(nav["outcome"], "verified");
    assert_eq!(nav["page_title"], "Fixture Form");

    // wait-for --url-pattern: current URL matches
    let (code, w, raw) = r(&[
        "wait-for",
        "--url-pattern",
        r"form\.html",
        "--timeout-ms",
        "5000",
    ]);
    assert_eq!(code, 0, "wait-for url-pattern match: {}", raw);
    assert_eq!(w["met"], true);
    assert_eq!(w["condition"]["url_pattern"], "form\\.html");

    // wait-for --url-pattern: no match → timeout result, exit 0
    let (code, w, raw) = r(&[
        "wait-for",
        "--url-pattern",
        "definitely-not",
        "--timeout-ms",
        "1500",
    ]);
    assert_eq!(
        code, 0,
        "wait-for url-pattern timeout must be result: {}",
        raw
    );
    assert_eq!(w["met"], false);

    // wait-for --network-idle: quiet after navigate
    let (code, w, raw) = r(&["wait-for", "--network-idle", "--timeout-ms", "5000"]);
    assert_eq!(code, 0, "wait-for network-idle: {}", raw);
    assert_eq!(w["met"], true);
    assert_eq!(w["condition"]["network_idle"], true);

    // invalid regex → exit 2
    let (code, j, _) = r(&["wait-for", "--url-pattern", "[", "--timeout-ms", "1000"]);
    assert_eq!(code, 2, "invalid regex must exit 2");
    assert!(j["detail"]
        .as_str()
        .unwrap()
        .contains("invalid --url-pattern regex"));

    // in-session digest: content read, no outcome, DOM stays intact
    let (code, d, _) = r(&["digest"]);
    assert_eq!(code, 0);
    assert!(d.get("outcome").is_none());
    assert!(d["digest"]["text"]
        .as_str()
        .unwrap()
        .contains("Main content paragraph"));

    // click non-submit control → revealed content appears
    let (code, c, raw) = r(&["click", "--selector", "#reveal"]);
    assert_eq!(code, 0, "click: {}", raw);
    assert_eq!(c["clicked"], true);
    let (code, w, _) = r(&[
        "wait-for",
        "--text",
        "EXTRA_CONTENT_MARKER",
        "--timeout-ms",
        "5000",
    ]);
    assert_eq!(code, 0);
    assert_eq!(w["met"], true);

    // the DOM survived the earlier digest (non-destructive extraction):
    let (_, d2, _) = r(&["digest"]);
    assert!(d2["digest"]["text"]
        .as_str()
        .unwrap()
        .contains("EXTRA_CONTENT_MARKER"));

    // type into a text field; live re-read confirms
    let (code, t, raw) = r(&["type", "--selector", "#name", "--text", "Ada Lovelace"]);
    assert_eq!(code, 0, "type: {}", raw);
    assert_eq!(t["ok"], true);
    assert_eq!(t["now_contains"], "Ada Lovelace");

    // select by visible text
    let (code, sel, _) = r(&["select", "--selector", "#country", "--value", "Canada"]);
    assert_eq!(code, 0);
    assert_eq!(sel["ok"], true);
    assert_eq!(sel["value"], "ca");

    // STRUCTURAL REFUSALS — submit control and password field
    let (code, j, _) = r(&["click", "--selector", "#send"]);
    assert_eq!(code, 2, "submit-typed control must be refused");
    let detail = j["detail"].as_str().unwrap();
    assert!(detail.contains("submit"));
    // The refusal redirects the user to the `submit` verb, so it must not also
    // tell them that verb is missing: `forms::submit` is implemented, wired
    // into this same binary, and covered three tests down this file.
    assert!(
        !detail.contains("not built") && !detail.contains("does not exist"),
        "click's refusal must not claim `submit` is unbuilt: {detail}"
    );
    let (code, j, _) = r(&["type", "--selector", "#pw", "--text", "hunter2"]);
    assert_eq!(code, 2, "password field must be refused");
    assert!(j["detail"].as_str().unwrap().contains("password"));

    // link click → page2, digest sees the marker, back returns
    let (code, _, raw) = r(&["click", "--selector", "#next"]);
    assert_eq!(code, 0, "link click: {}", raw);
    let (_, w2, _) = r(&[
        "wait-for",
        "--text",
        "PAGE_TWO_MARKER",
        "--timeout-ms",
        "5000",
    ]);
    assert_eq!(w2["met"], true);
    let (code, b, _) = r(&["back"]);
    assert_eq!(code, 0);
    assert!(b["final_url"].as_str().unwrap().contains("form.html"));

    // screenshot evidence
    let (code, sc, raw) = r(&["screenshot", "--out", &shot]);
    assert_eq!(code, 0, "screenshot: {}", raw);
    assert!(sc["bytes"].as_u64().unwrap() > 1000);
    assert!(std::path::Path::new(&shot).exists());

    // close
    let (code, cl, _) = run(&["session", "close", &id]);
    assert_eq!(code, 0);
    assert_eq!(cl["closed"][0]["closed"], true);
}

#[test]
fn digest_delta_session() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);
    let dir = std::env::temp_dir().join(format!("bm-test-delta-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let allow = allowlist_file(&dir);

    let (code, s, raw) = run(&["session", "open"]);
    assert_eq!(code, 0, "session open failed: {}", raw);
    let id = s["id"].as_str().expect("session id").to_string();

    let r = |args: &[&str]| {
        let mut full: Vec<&str> = args.to_vec();
        full.extend_from_slice(&["--session", &id, "--allow-local", "--allowlist", &allow]);
        run(&full)
    };

    let (code, _, raw) = r(&["navigate", &format!("{}/form.html", base)]);
    assert_eq!(code, 0, "navigate: {}", raw);

    // First --delta call: no prior snapshot → baseline, full digest returned.
    let (code, d1, raw) = r(&["digest", "--delta"]);
    assert_eq!(code, 0, "first delta digest: {}", raw);
    assert_eq!(d1["delta"]["baseline"], true);
    assert!(
        d1.get("digest").is_some(),
        "baseline must return the full digest"
    );
    assert!(d1["digest"]["interactive"]["buttons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["label"] == "Show more"));

    // Reveal hidden content.
    let (code, c, raw) = r(&["click", "--selector", "#reveal"]);
    assert_eq!(code, 0, "click reveal: {}", raw);
    assert_eq!(c["clicked"], true);
    let (code, w, _) = r(&[
        "wait-for",
        "--text",
        "EXTRA_CONTENT_MARKER",
        "--timeout-ms",
        "5000",
    ]);
    assert_eq!(code, 0);
    assert_eq!(w["met"], true);

    // Second --delta call: text changed, stable elements are NOT added.
    let (code, d2, raw) = r(&["digest", "--delta"]);
    assert_eq!(code, 0, "second delta digest: {}", raw);
    assert_eq!(d2["delta"]["baseline"], false);
    assert_eq!(d2["delta"]["text_changed"], true);
    assert!(
        d2["delta"]["text"]
            .as_str()
            .unwrap()
            .contains("EXTRA_CONTENT_MARKER"),
        "revealed text must appear in delta"
    );
    let added_selectors: Vec<&str> = d2["delta"]["added"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["selector"].as_str())
        .collect();
    assert!(
        !added_selectors.contains(&"#reveal"),
        "stable #reveal must not be in added"
    );
    assert!(
        !added_selectors.contains(&"#next"),
        "stable #next must not be in added"
    );
    assert!(
        !added_selectors.contains(&"#name"),
        "stable #name must not be in added"
    );

    // Third --delta call with no intervening action: idempotent empty diff.
    let (code, d3, raw) = r(&["digest", "--delta"]);
    assert_eq!(code, 0, "third delta digest: {}", raw);
    assert_eq!(d3["delta"]["baseline"], false);
    assert_eq!(d3["delta"]["text_changed"], false);
    assert!(d3["delta"]["added"].as_array().unwrap().is_empty());
    assert!(d3["delta"]["removed"].as_array().unwrap().is_empty());
    assert!(d3["delta"]["changed"].as_array().unwrap().is_empty());

    let _ = run(&["session", "close", &id]);
    let _ = std::fs::remove_file(format!(".browser-miner/session-{}.digest.json", id));
}

// endregion: Reading a page, and driving a session

// region: Forms, shadow roots and frames
// ---------------------------------------------------------------------------
// Forms, shadow roots and frames
//
// The parts of a real page that a naive extractor silently misses. Shadow
// roots and same-origin frames are invisible to an ordinary `querySelector`,
// so these assert both that their contents are found AND that the selectors
// handed back can be acted on afterwards — an inventory listing something
// unreachable is worse than not listing it. Closed roots stay absent, which is
// correct rather than a gap.
// ---------------------------------------------------------------------------

#[test]
fn extract_form_fill_and_submit_two_key() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);
    let dir = std::env::temp_dir().join(format!("bm-test-forms-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let allow = allowlist_file(&dir);

    // ── extract-form (stateless): deepened inventory ────────────────────
    let (code, ef, raw) = run(&[
        "extract-form",
        &format!("{}/form.html", base),
        "--allow-local",
    ]);
    assert_eq!(code, 0, "extract-form: {}", raw);
    let form = &ef["form"];
    let fields = form["fields"].as_array().unwrap();
    // radio group collapsed to one logical field with options + legend group
    let radio = fields
        .iter()
        .find(|f| f["kind"] == "radio_group")
        .expect("radio_group");
    assert_eq!(radio["name"], "auth");
    assert_eq!(radio["options"].as_array().unwrap().len(), 2);
    assert!(radio["group"]
        .as_str()
        .unwrap()
        .contains("Work authorization"));
    // password field carries the structural-refusal marker
    let pw = fields
        .iter()
        .find(|f| f["kind"] == "password")
        .expect("password field");
    assert!(pw["fill_refused"].as_str().is_some());
    // file input with accept
    let file = fields
        .iter()
        .find(|f| f["kind"] == "file")
        .expect("file field");
    assert_eq!(file["accept"], ".pdf");
    // wizard signals detected
    assert_eq!(form["wizard"]["likely"], true);
    assert!(form["wizard"]["nav_buttons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["label"] == "Next"));
    // form lists its submit control
    assert!(!form["forms"][0]["submit_controls"]
        .as_array()
        .unwrap()
        .is_empty());

    // ── fill: types, checks, selects, uploads; refuses passwords ────────
    let resume = dir.join("resume.pdf");
    std::fs::write(&resume, b"%PDF-1.4 fixture").unwrap();
    let values = dir.join("values.json");
    std::fs::write(
        &values,
        serde_json::json!({
            "fields": [
                { "selector": "#name", "value": "Ada Lovelace" },
                { "selector": "#country", "value": "Canada" },
                { "selector": "#auth-yes", "checked": true },
                { "selector": "#remote_ok", "checked": true },
                { "selector": "#resume", "file": resume.to_string_lossy() },
                { "selector": "#pw", "value": "hunter2" },
                { "selector": "#no-such-field", "value": "x" }
            ]
        })
        .to_string(),
    )
    .unwrap();

    let (code, s, raw) = run(&["session", "open"]);
    assert_eq!(code, 0, "session open: {}", raw);
    let id = s["id"].as_str().unwrap().to_string();
    let (code, _, _) = run(&[
        "navigate",
        &format!("{}/form.html", base),
        "--session",
        &id,
        "--allow-local",
    ]);
    assert_eq!(code, 0);

    let (code, f, raw) = run(&[
        "fill",
        "--session",
        &id,
        "--values",
        &values.to_string_lossy(),
        "--allow-local",
        "--allowlist",
        &allow,
    ]);
    assert_eq!(code, 0, "fill: {}", raw);
    let results = f["results"].as_array().unwrap();
    let by_sel = |sel: &str| results.iter().find(|r| r["selector"] == sel).unwrap();
    assert_eq!(by_sel("#name")["ok"], true);
    assert_eq!(by_sel("#name")["now_contains"], "Ada Lovelace");
    assert_eq!(by_sel("#country")["ok"], true);
    assert_eq!(by_sel("#auth-yes")["now_contains"], "checked");
    assert_eq!(by_sel("#remote_ok")["now_contains"], "checked");
    assert_eq!(by_sel("#resume")["now_contains"], "resume.pdf");
    // password: per-field structural refusal, NOT filled
    assert_eq!(by_sel("#pw")["ok"], false);
    assert!(by_sel("#pw")["refused"]
        .as_str()
        .unwrap()
        .contains("password"));
    // unmatched selector reported, never silently skipped
    assert_eq!(by_sel("#no-such-field")["ok"], false);
    assert_eq!(f["fields_ok"], 5);

    // ── submit: two-key refusals ────────────────────────────────────────
    // key 1 only (flag) → refused naming key 2 (no user config in test cwd)
    let (code, j, _) = run(&[
        "submit",
        "--session",
        &id,
        "--selector",
        "#send",
        "--yes-actually-submit",
        "--allow-local",
        "--allowlist",
        &allow,
    ]);
    assert_eq!(code, 2, "flag without config must be refused");
    assert!(j["detail"].as_str().unwrap().contains("allow_auto_submit"));
    // no keys, headless session → observe mode refused (needs headful)
    let (code, j, _) = run(&[
        "submit",
        "--session",
        &id,
        "--selector",
        "#send",
        "--allow-local",
        "--allowlist",
        &allow,
    ]);
    assert_eq!(code, 2, "observe mode on headless session must be refused");
    assert!(j["detail"].as_str().unwrap().contains("headful"));
    // CERTIFY: the fill left the form UNSUBMITTED — still on form.html
    let (_, d, _) = run(&["digest", "--session", &id, "--allow-local"]);
    assert!(
        d["final_url"].as_str().unwrap().contains("form.html"),
        "fill must never submit"
    );

    let _ = run(&["session", "close", &id]);
}

#[test]
fn shadow_dom_digest_and_actions() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);
    let dir = std::env::temp_dir().join(format!("bm-test-shadow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let allow = allowlist_file(&dir);

    // digest sees open-shadow elements with >>> selectors and in_shadow flag.
    let (code, d, raw) = run(&["digest", &format!("{}/shadow.html", base), "--allow-local"]);
    assert_eq!(code, 0, "digest shadow.html: {}", raw);

    let interactive = &d["digest"]["interactive"];
    let links = interactive["links"].as_array().unwrap();
    let shadow_link = links.iter().find(|l| l["in_shadow"] == true);
    assert!(
        shadow_link.is_some(),
        "shadow link missing; got: {:?}",
        links
    );
    assert!(
        shadow_link.unwrap()["selector"]
            .as_str()
            .unwrap()
            .contains(" >>> "),
        "shadow selector must contain >>>"
    );

    let buttons = interactive["buttons"].as_array().unwrap();
    assert!(
        buttons
            .iter()
            .any(|b| b["in_shadow"] == true && b["selector"].as_str().unwrap().contains(" >>> ")),
        "shadow button missing; got: {:?}",
        buttons
    );

    let fields = interactive["fields"].as_array().unwrap();
    assert!(
        fields
            .iter()
            .any(|f| f["in_shadow"] == true && f["selector"].as_str().unwrap().contains(" >>> ")),
        "shadow field missing; got: {:?}",
        fields
    );

    // Closed shadow root is unobservable.
    assert!(
        !buttons
            .iter()
            .any(|b| b["label"].as_str().unwrap_or("").contains("Closed")),
        "closed-root button must not appear in digest"
    );

    // session: navigate, click shadow button, wait for the marker.
    let (code, s, raw) = run(&["session", "open"]);
    assert_eq!(code, 0, "session open: {}", raw);
    let id = s["id"].as_str().unwrap().to_string();

    let r = |args: &[&str]| {
        let mut full: Vec<&str> = args.to_vec();
        full.extend_from_slice(&["--session", &id, "--allow-local", "--allowlist", &allow]);
        run(&full)
    };

    let shadow_button_sel = shadow_link.unwrap()["selector"]
        .as_str()
        .unwrap()
        .rsplit(" >>> ")
        .nth(1)
        .map(|host| format!("{} >>> #shadow-button", host))
        .unwrap_or_else(|| "#shadow-host >>> #shadow-button".to_string());

    let (code, _, _) = r(&["navigate", &format!("{}/shadow.html", base)]);
    assert_eq!(code, 0);

    let (code, c, raw) = r(&["click", "--selector", &shadow_button_sel]);
    assert_eq!(code, 0, "click shadow button: {}", raw);
    assert_eq!(c["clicked"], true);

    let (code, w, _) = r(&[
        "wait-for",
        "--text",
        "SHADOW_CLICK_MARKER",
        "--timeout-ms",
        "5000",
    ]);
    assert_eq!(code, 0);
    assert_eq!(w["met"], true);

    // type into the shadow text input.
    let shadow_input_sel = shadow_link.unwrap()["selector"]
        .as_str()
        .unwrap()
        .rsplit(" >>> ")
        .nth(1)
        .map(|host| format!("{} >>> #shadow-input", host))
        .unwrap_or_else(|| "#shadow-host >>> #shadow-input".to_string());

    let (code, t, raw) = r(&[
        "type",
        "--selector",
        &shadow_input_sel,
        "--text",
        "hello shadow",
    ]);
    assert_eq!(code, 0, "type shadow input: {}", raw);
    assert_eq!(t["ok"], true);
    assert!(t["now_contains"].as_str().unwrap().contains("hello shadow"));

    let _ = run(&["session", "close", &id]);
}

#[test]
fn same_origin_iframe_digest_and_actions() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);
    let dir = std::env::temp_dir().join(format!("bm-test-iframe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let allow = allowlist_file(&dir);

    // digest framed.html: same-origin iframe fields are visible; cross-origin frame is reported.
    let (code, d, raw) = run(&["digest", &format!("{}/framed.html", base), "--allow-local"]);
    assert_eq!(code, 0, "digest framed.html: {}", raw);

    let fields = d["digest"]["interactive"]["fields"].as_array().unwrap();
    let frame_field = fields.iter().find(|f| {
        f["in_frame"] == true
            && f["selector"].as_str().unwrap_or("").contains(" ||| ")
            && f["selector"].as_str().unwrap_or("").contains("#name")
    });
    assert!(
        frame_field.is_some(),
        "frame field #name missing; got: {:?}",
        fields
    );

    let frames_unreadable = d["digest"]["frames_unreadable"].as_array().unwrap();
    assert_eq!(
        frames_unreadable.len(),
        1,
        "expected one unreadable cross-origin frame; got: {:?}",
        frames_unreadable
    );
    assert!(frames_unreadable[0]["selector"]
        .as_str()
        .unwrap_or("")
        .contains("cross-origin-frame"));

    // extract-form includes frame fields.
    let (code, ef, raw) = run(&[
        "extract-form",
        &format!("{}/framed.html", base),
        "--allow-local",
    ]);
    assert_eq!(code, 0, "extract-form framed.html: {}", raw);
    let ef_fields = ef["form"]["fields"].as_array().unwrap();
    assert!(
        ef_fields.iter().any(
            |f| f["in_frame"] == true && f["selector"].as_str().unwrap_or("").contains(" ||| ")
        ),
        "extract-form must include frame fields; got: {:?}",
        ef_fields
    );

    // session: navigate, type into frame field, refuse password in frame.
    let (code, s, raw) = run(&["session", "open"]);
    assert_eq!(code, 0, "session open: {}", raw);
    let id = s["id"].as_str().unwrap().to_string();

    let r = |args: &[&str]| {
        let mut full: Vec<&str> = args.to_vec();
        full.extend_from_slice(&["--session", &id, "--allow-local", "--allowlist", &allow]);
        run(&full)
    };

    let (code, _, _) = r(&["navigate", &format!("{}/framed.html", base)]);
    assert_eq!(code, 0);

    let frame_name_sel = "#apply-frame ||| #name";
    let (code, t, raw) = r(&[
        "type",
        "--selector",
        frame_name_sel,
        "--text",
        "Ada Lovelace",
    ]);
    assert_eq!(code, 0, "type frame #name: {}", raw);
    assert_eq!(t["ok"], true);
    assert!(t["now_contains"].as_str().unwrap().contains("Ada Lovelace"));

    let (code, j, _) = r(&[
        "type",
        "--selector",
        "#apply-frame ||| #pw",
        "--text",
        "hunter2",
    ]);
    assert_eq!(code, 2, "password field in frame must be refused");
    assert!(j["detail"].as_str().unwrap().contains("password"));

    let _ = run(&["session", "close", &id]);
}

// endregion: Forms, shadow roots and frames

// region: The two-key rule, and the allowlist on a live session
// ---------------------------------------------------------------------------
// The two-key rule, and the allowlist on a live session
//
// The gates that stand between an agent and something irreversible happening
// on somebody else's server. Both halves are needed: that both keys together
// do permit a submit, and that either one alone does not — and separately,
// that a live session with no allowlist refuses to be interacted with at all.
// ---------------------------------------------------------------------------

/// The positive half of the two-key rule: with BOTH keys turned, a submit
/// really does submit, and it is logged. The refusals are cheap to keep
/// correct; a gate that refuses everything, including the case it was built to
/// permit, passes every refusal test and is still broken. This test and the
/// domain-scoping half below are what stop the rule from being decorative.
#[test]
fn submit_auto_with_both_keys_submits_and_logs() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);
    // Fresh working directory holding the USER-owned config (key 2) + allowlist.
    let cwd = std::env::temp_dir().join(format!("bm-test-submit-{}", std::process::id()));
    let data = cwd.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        data.join("browser-miner-config.json"),
        r#"{ "allow_auto_submit": true, "allow_auto_submit_domains": ["127.0.0.1"] }"#,
    )
    .unwrap();
    std::fs::write(
        data.join("browser-allowlist.json"),
        r#"{ "domains": ["127.0.0.1"] }"#,
    )
    .unwrap();
    let cwd = cwd.as_path();

    let (code, s, raw) = run_in(Some(cwd), &["session", "open"]);
    assert_eq!(code, 0, "session open: {}", raw);
    let id = s["id"].as_str().unwrap().to_string();
    let (code, _, _) = run_in(
        Some(cwd),
        &[
            "navigate",
            &format!("{}/form.html", base),
            "--session",
            &id,
            "--allow-local",
        ],
    );
    assert_eq!(code, 0);
    // #name is required — native validation blocks an empty submit, so fill it.
    let (code, t, raw) = run_in(
        Some(cwd),
        &[
            "type",
            "--session",
            &id,
            "--selector",
            "#name",
            "--text",
            "Ada",
            "--allow-local",
        ],
    );
    assert_eq!(code, 0, "type: {}", raw);
    assert_eq!(t["ok"], true);

    let (code, j, raw) = run_in(
        Some(cwd),
        &[
            "submit",
            "--session",
            &id,
            "--selector",
            "#send",
            "--yes-actually-submit",
            "--allow-local",
        ],
    );
    assert_eq!(code, 0, "auto submit with both keys: {}", raw);
    assert_eq!(j["mode"], "auto");
    assert_eq!(j["submitted"], true);
    assert!(
        j["final_url"].as_str().unwrap().contains("/submit"),
        "form action navigated; got: {}",
        j
    );
    // every auto-submit is logged
    let log = std::fs::read_to_string(cwd.join("data/browser-miner-submit-log.jsonl")).unwrap();
    assert!(log.contains("\"mode\":\"auto\"") && log.contains("127.0.0.1"));

    // domain scoping: a domain OUTSIDE allow_auto_submit_domains is refused
    std::fs::write(
        cwd.join("data/browser-miner-config.json"),
        r#"{ "allow_auto_submit": true, "allow_auto_submit_domains": ["example.com"] }"#,
    )
    .unwrap();
    let (code, _, raw) = run_in(
        Some(cwd),
        &[
            "navigate",
            &format!("{}/form.html", base),
            "--session",
            &id,
            "--allow-local",
        ],
    );
    assert_eq!(code, 0, "re-navigate before scoping check: {}", raw);
    let (code, j, raw) = run_in(
        Some(cwd),
        &[
            "submit",
            "--session",
            &id,
            "--selector",
            "#send",
            "--yes-actually-submit",
            "--allow-local",
        ],
    );
    assert_eq!(code, 2, "out-of-scope domain must be refused; got: {}", raw);
    assert!(j["detail"]
        .as_str()
        .unwrap()
        .contains("allow_auto_submit_domains"));

    let _ = run_in(Some(cwd), &["session", "close", &id]);
}

#[test]
fn interaction_without_allowlist_refused_on_live_session() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);

    let (code, s, raw) = run(&["session", "open"]);
    assert_eq!(code, 0, "session open failed: {}", raw);
    let id = s["id"].as_str().unwrap().to_string();

    let (code, _, _) = run(&[
        "navigate",
        &format!("{}/form.html", base),
        "--session",
        &id,
        "--allow-local",
    ]);
    assert_eq!(code, 0);

    // click WITHOUT any allowlist → policy refusal naming the file
    let (code, j, _) = run(&[
        "click",
        "--selector",
        "#reveal",
        "--session",
        &id,
        "--allow-local",
    ]);
    assert_eq!(code, 2);
    assert!(j["detail"].as_str().unwrap().contains("allowlist"));

    let _ = run(&["session", "close", &id]);
}

// endregion: The two-key rule, and the allowlist on a live session

// region: A multi-step form, and the user's own Chrome
// ---------------------------------------------------------------------------
// A multi-step form, and the user's own Chrome
//
// The two longest scenarios. The wizard walks three steps forward and back
// again and re-reads every value, which is the only way to prove that state
// really survives — a single step proves nothing a fresh page load would not.
// Attach mode proves the opposite kind of property: that closing a session
// Emma did not open leaves the user's browser untouched.
// ---------------------------------------------------------------------------

#[test]
fn wizard_loop_state_persists_across_steps() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);
    let dir = std::env::temp_dir().join(format!("bm-test-wizard-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let allow = allowlist_file(&dir);

    let (code, s, raw) = run(&["session", "open"]);
    assert_eq!(code, 0, "session open: {}", raw);
    let id = s["id"].as_str().unwrap().to_string();

    let r = |args: &[&str]| {
        let mut full: Vec<&str> = args.to_vec();
        full.extend_from_slice(&["--session", &id, "--allow-local", "--allowlist", &allow]);
        run(&full)
    };

    // 1. navigate and extract-form: wizard signals on step 1.
    let (code, _, raw) = r(&["navigate", &format!("{}/wizard.html", base)]);
    assert_eq!(code, 0, "navigate: {}", raw);

    let (code, ef, raw) = r(&["extract-form"]);
    assert_eq!(code, 0, "extract-form: {}", raw);
    let wizard = &ef["form"]["wizard"];
    assert_eq!(wizard["likely"], true);
    let indicators = wizard["indicators"].as_array().unwrap();
    assert!(
        indicators
            .iter()
            .any(|i| i.as_str().unwrap().contains("Step 1 of 3")),
        "step indicator missing; got: {:?}",
        indicators
    );
    let nav_buttons = wizard["nav_buttons"].as_array().unwrap();
    assert!(
        nav_buttons
            .iter()
            .any(|b| b["label"] == "Next" && b["selector"] == "#to-step-2"),
        "Next nav button missing; got: {:?}",
        nav_buttons
    );

    // 2. type name; Next becomes enabled.
    let (code, t, raw) = r(&["type", "--selector", "#w-name", "--text", "Ada"]);
    assert_eq!(code, 0, "type name: {}", raw);
    assert_eq!(t["ok"], true);
    assert!(t["now_contains"].as_str().unwrap().contains("Ada"));

    let (code, ef2, raw) = r(&["extract-form"]);
    assert_eq!(code, 0, "extract-form after name: {}", raw);
    let next_btn = ef2["form"]["wizard"]["nav_buttons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["selector"] == "#to-step-2")
        .cloned()
        .expect("Next button in nav_buttons");
    assert_eq!(
        next_btn["disabled"], false,
        "Next must be enabled after name is filled"
    );

    // 3. advance to step 2.
    let (code, _, raw) = r(&["click", "--selector", "#to-step-2"]);
    assert_eq!(code, 0, "click next to step 2: {}", raw);
    let (code, w, raw) = r(&["wait-for", "--text", "Step 2 of 3", "--timeout-ms", "5000"]);
    assert_eq!(code, 0, "wait-for step 2: {}", raw);
    assert_eq!(w["met"], true);

    // 4. select country and advance to step 3.
    let (code, sel, raw) = r(&["select", "--selector", "#w-country", "--value", "Canada"]);
    assert_eq!(code, 0, "select country: {}", raw);
    assert_eq!(sel["ok"], true);
    let (code, _, raw) = r(&["click", "--selector", "#to-step-3"]);
    assert_eq!(code, 0, "click next to step 3: {}", raw);
    let (code, w, raw) = r(&["wait-for", "--text", "Step 3 of 3", "--timeout-ms", "5000"]);
    assert_eq!(code, 0, "wait-for step 3: {}", raw);
    assert_eq!(w["met"], true);

    // 5. check agree; submit becomes enabled (but do not click it).
    let agree_values = dir.join("agree.json");
    std::fs::write(
        &agree_values,
        serde_json::json!({ "fields": [{ "selector": "#w-agree", "checked": true }] }).to_string(),
    )
    .unwrap();
    let (code, f, raw) = r(&["fill", "--values", &agree_values.to_string_lossy()]);
    assert_eq!(code, 0, "fill agree: {}", raw);
    let agree_result = f["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["selector"] == "#w-agree")
        .expect("agree fill result");
    assert_eq!(agree_result["ok"], true);
    assert_eq!(agree_result["now_contains"], "checked");

    let (code, w, raw) = r(&[
        "wait-for",
        "--selector",
        "#w-submit:not([disabled])",
        "--timeout-ms",
        "5000",
    ]);
    assert_eq!(code, 0, "wait-for submit enabled: {}", raw);
    assert_eq!(w["met"], true);

    // 6. STATE HOLDS: go back to step 1 twice; name still contains Ada.
    let (code, _, raw) = r(&["click", "--selector", "#back-to-1"]);
    assert_eq!(code, 0, "click back first: {}", raw);
    let (code, _, raw) = r(&["click", "--selector", "#back-to-1"]);
    assert_eq!(code, 0, "click back second: {}", raw);
    let (code, w_back, raw) = r(&["wait-for", "--text", "Step 1 of 3", "--timeout-ms", "5000"]);
    assert_eq!(code, 0, "wait-for back on step 1: {}", raw);
    assert_eq!(w_back["met"], true);

    let (code, ef_back, raw) = r(&["extract-form"]);
    assert_eq!(code, 0, "extract-form back on step 1: {}", raw);
    let name_field = ef_back["form"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["selector"] == "#w-name")
        .expect("name field");
    assert_eq!(
        name_field["value"], "Ada",
        "name value must persist across wizard steps"
    );

    // 7. return to step 3 and read all three values back; no submit.
    let (code, _, raw) = r(&["click", "--selector", "#to-step-2"]);
    assert_eq!(code, 0, "click forward to step 2: {}", raw);
    let (code, _, raw) = r(&["click", "--selector", "#to-step-3"]);
    assert_eq!(code, 0, "click forward to step 3: {}", raw);
    let (code, w_end, raw) = r(&["wait-for", "--text", "Step 3 of 3", "--timeout-ms", "5000"]);
    assert_eq!(code, 0, "wait-for end on step 3: {}", raw);
    assert_eq!(w_end["met"], true);

    let (code, ef_end, raw) = r(&["extract-form"]);
    assert_eq!(code, 0, "extract-form end: {}", raw);
    let fields = ef_end["form"]["fields"].as_array().unwrap();
    let by_sel = |sel: &str| fields.iter().find(|f| f["selector"] == sel).expect(sel);
    assert_eq!(by_sel("#w-name")["value"], "Ada");
    assert_eq!(by_sel("#w-country")["value"], "ca");
    assert_eq!(by_sel("#w-agree")["checked"], true);

    let _ = run(&["session", "close", &id]);
}

/// Attach mode drives Chrome the user started, as the user's own logged-in
/// identity, so the rule it must never break is that closing an attached
/// session leaves that Chrome running. This test proves it the only way that
/// means anything: it closes the attached session and then uses the underlying
/// browser again. It also holds the consent banner to the stderr contract —
/// the banner is the user's one warning, and a banner nothing asserts on is a
/// banner that can quietly stop printing.
#[test]
fn attach_mode_opt_in_and_safe_close() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);

    // Spawn a managed session so we have a real ws:// endpoint to attach to.
    let (code, managed, raw) = run(&["session", "open"]);
    assert_eq!(code, 0, "managed session open failed: {}", raw);
    let managed_id = managed["id"]
        .as_str()
        .expect("managed session id")
        .to_string();
    let ws_url = managed["ws_url"]
        .as_str()
        .expect("managed ws_url")
        .to_string();
    assert_eq!(managed["attached"], false);
    assert_eq!(managed["managed"], true);

    // Attach to the managed session's endpoint.
    let (code, attached, raw) = run(&["session", "open", "--attach", &ws_url]);
    assert_eq!(code, 0, "attach session open failed: {}", raw);
    assert_eq!(
        attached["attached"], true,
        "attach output must flag attached"
    );
    assert_eq!(
        attached["managed"], false,
        "attach output must flag unmanaged"
    );
    let attached_id = attached["id"]
        .as_str()
        .expect("attached session id")
        .to_string();

    // Session file records the attach mode.
    let sf_path = format!(".browser-miner/session-{}.json", attached_id);
    let sf_raw = std::fs::read_to_string(&sf_path).expect("attached session file");
    let sf: serde_json::Value = serde_json::from_str(&sf_raw).expect("parse attached session file");
    assert_eq!(sf["attached"], true);
    assert_eq!(sf["managed"], false);

    // Consent banner printed to stderr on attach invocation.
    assert!(
        raw.contains("ATTACH MODE"),
        "attach invocation must print consent banner on stderr; got: {}",
        raw
    );
    assert!(
        raw.contains("cookies or credentials"),
        "consent banner must mention credential handling; got: {}",
        raw
    );

    // Digest via the attached session works and is auditable.
    let (code, _, raw) = run(&[
        "navigate",
        &format!("{}/form.html", base),
        "--session",
        &attached_id,
        "--allow-local",
    ]);
    assert_eq!(code, 0, "navigate attached session: {}", raw);
    let (code, d, raw) = run(&["digest", "--session", &attached_id, "--allow-local"]);
    assert_eq!(code, 0, "digest attached session: {}", raw);
    assert!(
        d["digest"]["text"]
            .as_str()
            .unwrap()
            .contains("Main content paragraph"),
        "attached digest must see fixture content"
    );
    assert_eq!(
        d["attached"], true,
        "attached session digest must be annotated"
    );
    assert_eq!(
        d["evidence"]["attached"], true,
        "attached session digest evidence must be annotated"
    );

    // Closing the attached session must NOT kill the underlying Chrome.
    let (code, cl, raw) = run(&["session", "close", &attached_id]);
    assert_eq!(code, 0, "close attached session: {}", raw);
    assert_eq!(cl["closed"][0]["closed"], true);
    assert_eq!(cl["closed"][0]["attached"], true);
    assert_eq!(cl["closed"][0]["managed"], false);

    // The original managed session is still alive and usable.
    let (code, d, raw) = run(&["digest", "--session", &managed_id, "--allow-local"]);
    assert_eq!(code, 0, "managed session survived attached close: {}", raw);
    assert!(
        d.get("digest").is_some(),
        "managed session digest must work after attached close"
    );

    // Cleanup: close the managed session (this one IS allowed to kill its Chrome).
    let (code, cl, raw) = run(&["session", "close", &managed_id]);
    assert_eq!(code, 0, "close managed session: {}", raw);
    assert_eq!(cl["closed"][0]["closed"], true);
    assert_eq!(cl["closed"][0]["managed"], true);
}

// endregion: A multi-step form, and the user's own Chrome

// region: Newline injection, and the honest negative
// ---------------------------------------------------------------------------
// Newline injection, and the honest negative
//
// A newline typed into a single-line input is a form submission by another
// name, so both `type` and `fill` refuse it — and `#tricky` is the case worth
// reading, an `<input type="textarea">` whose unknown type falls back to text
// while looking exempt. A textarea still accepts newlines, which is what stops
// the rule from being a blanket ban.
//
// The last two are the governing rule from the other direction: a submit that
// did not navigate must not claim it succeeded, and a page that answered with
// a challenge is a result rather than a failure.
// ---------------------------------------------------------------------------

#[test]
fn type_rejects_newline_in_single_line_input_and_allows_textarea() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);
    let dir = std::env::temp_dir().join(format!("bm-test-newline-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let allow = allowlist_file(&dir);

    let (code, s, raw) = run(&["session", "open"]);
    assert_eq!(code, 0, "session open: {}", raw);
    let id = s["id"].as_str().unwrap().to_string();

    let r = |args: &[&str]| {
        let mut full: Vec<&str> = args.to_vec();
        full.extend_from_slice(&["--session", &id, "--allow-local", "--allowlist", &allow]);
        run(&full)
    };

    let (code, _, raw) = r(&["navigate", &format!("{}/form.html", base)]);
    assert_eq!(code, 0, "navigate: {}", raw);

    // Single-line input: newline refused before any typing happens.
    let (code, j, raw) = r(&["type", "--selector", "#name", "--text", "a\nb"]);
    assert_eq!(
        code, 2,
        "newline in single-line input must be refused: {}",
        raw
    );
    assert!(
        j["detail"].as_str().unwrap().contains("newline"),
        "detail: {}",
        j
    );
    assert!(
        j["detail"].as_str().unwrap().contains("submission"),
        "detail: {}",
        j
    );

    // Textarea: newline accepted.
    let (code, t, raw) = r(&["type", "--selector", "#bio", "--text", "line1\nline2"]);
    assert_eq!(code, 0, "textarea newline: {}", raw);
    assert_eq!(t["ok"], true);
    let now = t["now_contains"].as_str().unwrap();
    assert!(
        now.contains("line1") && now.contains("line2"),
        "textarea should retain both lines; got: {}",
        t
    );

    let _ = run(&["session", "close", &id]);
}

#[test]
fn fill_rejects_newline_in_single_line_input() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);
    let dir = std::env::temp_dir().join(format!("bm-test-fill-newline-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let allow = allowlist_file(&dir);

    let values = dir.join("values.json");
    std::fs::write(
        &values,
        serde_json::json!({
            "fields": [
                { "selector": "#name", "value": "a\nb" },
                { "selector": "#tricky", "value": "x\ny" },
                { "selector": "#bio", "value": "line1\nline2" }
            ]
        })
        .to_string(),
    )
    .unwrap();

    let (code, s, raw) = run(&["session", "open"]);
    assert_eq!(code, 0, "session open: {}", raw);
    let id = s["id"].as_str().unwrap().to_string();

    let (code, _, raw) = run(&[
        "navigate",
        &format!("{}/form.html", base),
        "--session",
        &id,
        "--allow-local",
    ]);
    assert_eq!(code, 0, "navigate: {}", raw);

    let (code, f, raw) = run(&[
        "fill",
        "--session",
        &id,
        "--values",
        &values.to_string_lossy(),
        "--allow-local",
        "--allowlist",
        &allow,
    ]);
    assert_eq!(code, 0, "fill: {}", raw);
    let results = f["results"].as_array().unwrap();
    let name_result = results.iter().find(|r| r["selector"] == "#name").unwrap();
    assert_eq!(name_result["ok"], false);
    assert!(
        name_result["refused"].as_str().unwrap().contains("newline"),
        "name refused: {}",
        name_result
    );
    // <input type="textarea"> is an UNKNOWN type → text fallback → Enter
    // submits. Must be refused like any single-line input, NOT exempted.
    let tricky_result = results.iter().find(|r| r["selector"] == "#tricky").unwrap();
    assert_eq!(
        tricky_result["ok"], false,
        "input[type=textarea] must be treated single-line: {}",
        tricky_result
    );
    assert!(
        tricky_result["refused"]
            .as_str()
            .unwrap_or("")
            .contains("newline"),
        "tricky refused: {}",
        tricky_result
    );
    let bio_result = results.iter().find(|r| r["selector"] == "#bio").unwrap();
    assert_eq!(
        bio_result["ok"], true,
        "textarea should accept newline: {}",
        bio_result
    );

    let _ = run(&["session", "close", &id]);
}

#[test]
fn submit_auto_reports_not_submitted_when_no_navigation() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);
    let cwd = std::env::temp_dir().join(format!("bm-test-submit-none-{}", std::process::id()));
    let data = cwd.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        data.join("browser-miner-config.json"),
        r#"{ "allow_auto_submit": true, "allow_auto_submit_domains": ["127.0.0.1"] }"#,
    )
    .unwrap();
    std::fs::write(
        data.join("browser-allowlist.json"),
        r#"{ "domains": ["127.0.0.1"] }"#,
    )
    .unwrap();

    let (code, s, raw) = run_in(Some(cwd.as_path()), &["session", "open"]);
    assert_eq!(code, 0, "session open: {}", raw);
    let id = s["id"].as_str().unwrap().to_string();
    let (code, _, _) = run_in(
        Some(cwd.as_path()),
        &[
            "navigate",
            &format!("{}/no-submit.html", base),
            "--session",
            &id,
            "--allow-local",
        ],
    );
    assert_eq!(code, 0);

    // Fill required field so native validation doesn't block the click.
    let (code, _, _) = run_in(
        Some(cwd.as_path()),
        &[
            "type",
            "--session",
            &id,
            "--selector",
            "#nn-name",
            "--text",
            "Ada",
            "--allow-local",
        ],
    );
    assert_eq!(code, 0);

    let (code, j, raw) = run_in(
        Some(cwd.as_path()),
        &[
            "submit",
            "--session",
            &id,
            "--selector",
            "#nn-send",
            "--yes-actually-submit",
            "--allow-local",
        ],
    );
    assert_eq!(code, 0, "auto submit no-navigate: {}", raw);
    assert_eq!(j["mode"], "auto");
    assert_eq!(
        j["submitted"], false,
        "must not claim submitted when URL did not change: {}",
        j
    );
    assert_eq!(j["url_changed"], false);
    assert!(
        j["note"].as_str().unwrap().contains("no navigation"),
        "note: {}",
        j
    );
    // Log still records the attempt.
    let log = std::fs::read_to_string(cwd.join("data/browser-miner-submit-log.jsonl")).unwrap();
    assert!(log.contains("\"mode\":\"auto\""));

    let _ = run_in(Some(cwd.as_path()), &["session", "close", &id]);
}

/// Upstream had `extract_jobs_reports_blocked_not_stale` here. The fork drops
/// `adapters.rs`, so there is no `extract-jobs` verb left to test — but the
/// property that test really guarded is the one Emma depends on hardest, so it
/// is re-asserted against the same fixture through `digest`: a challenged page
/// is a RESULT (exit 0) carrying `looks_blocked: true`, never an error. If this
/// ever exits non-zero, "I could not read the page" and "the page refused to
/// show itself" have become the same message.
#[test]
fn a_blocked_page_is_a_result_not_a_failure() {
    let (port, _h) = serve_fixtures();
    let url = format!("http://127.0.0.1:{}/blocked.html", port);

    let (code, d, raw) = run(&["digest", &url, "--allow-local"]);
    assert_eq!(code, 0, "a blocked page must exit 0: {}", raw);
    assert_eq!(d["looks_blocked"], true, "{}", d);
    assert_eq!(d["outcome"], "blocked", "{}", d);
    assert!(d.get("digest").is_some(), "the payload is still returned");
}

// endregion: Newline injection, and the honest negative

// region: Exactness, cleanup, and caps
// ---------------------------------------------------------------------------
// Exactness, cleanup, and caps
//
// The quiet failures. A substring match reporting `ok: true` for a field that
// actually holds "AdaLovelace"; a temp profile nobody removes; a session id
// used as a path component; a page title with no ceiling. None of these break
// a run outright, which is exactly why each needs a test rather than a reader.
// ---------------------------------------------------------------------------

#[test]
fn type_ok_requires_exact_match() {
    let (port, _h) = serve_fixtures();
    let base = format!("http://127.0.0.1:{}", port);
    let dir = std::env::temp_dir().join(format!("bm-test-exact-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let allow = allowlist_file(&dir);

    let (code, s, raw) = run(&["session", "open"]);
    assert_eq!(code, 0, "session open: {}", raw);
    let id = s["id"].as_str().unwrap().to_string();
    let r = |args: &[&str]| {
        let mut full: Vec<&str> = args.to_vec();
        full.extend_from_slice(&["--session", &id, "--allow-local", "--allowlist", &allow]);
        run(&full)
    };

    let (code, _, _) = r(&["navigate", &format!("{}/form.html", base)]);
    assert_eq!(code, 0);

    // First type sets exact value.
    let (code, t, raw) = r(&["type", "--selector", "#name", "--text", "Ada"]);
    assert_eq!(code, 0, "type Ada: {}", raw);
    assert_eq!(t["ok"], true);

    // Second type appends; substring match would incorrectly report ok:true.
    let (code, t, raw) = r(&["type", "--selector", "#name", "--text", "Lovelace"]);
    assert_eq!(code, 0, "type Lovelace: {}", raw);
    assert_eq!(
        t["ok"], false,
        "substring match must not count as ok: {}",
        t
    );
    assert!(
        t["now_contains"].as_str().unwrap().contains("AdaLovelace"),
        "got: {}",
        t
    );

    let _ = run(&["session", "close", &id]);
}

/// Sessions spawn Chrome with a throwaway profile directory that nothing else
/// will ever clean up — `close` is the only path that removes it. Without this
/// test the leak is invisible: every other session test still passes while the
/// temp directory fills with abandoned profiles.
///
/// **This test was flaky, and it was right.** It failed under a full workspace
/// run and then passed eight times in a row, which reads as a slow machine and
/// is not: `close` killed Chrome and removed the directory in consecutive
/// statements, and `taskkill /T /F` returns before the process is gone. Run in
/// a loop the real rate was 8 leaks in 66 closes — a whole Chrome profile,
/// cookie database included, left in a shared temp folder. `session::close`
/// now waits for the process to actually exit; 130 closes after that, none
/// leaked.
///
/// Which means one run of this test is a **weak** detector of the thing it
/// guards: at one in eight it passes for the wrong reason most of the time, and
/// it cannot tell the fix from a retry that merely narrows the window. The
/// receipt for the mechanism is the unit test on `session::wait_for_exit`; this
/// one asserts the end state a user cares about, which is still worth asserting.
#[test]
fn session_profile_dir_is_removed_on_close() {
    let (code, s, raw) = run(&["session", "open"]);
    assert_eq!(code, 0, "session open: {}", raw);
    let id = s["id"].as_str().unwrap().to_string();
    let profile = std::env::temp_dir().join(format!("browser-miner-session-{}", id));
    assert!(
        profile.exists(),
        "profile dir should exist while session is open: {}",
        profile.display()
    );

    let (code, _, _) = run(&["session", "close", &id]);
    assert_eq!(code, 0);

    assert!(
        !profile.exists(),
        "profile dir must be removed on close: {}",
        profile.display()
    );
}

/// Session ids reach the filesystem as a path component. This is the test that
/// keeps `../../etc` from becoming one: the id is validated to hex before any
/// path is built, and the assertion that no session directory was created is
/// the load-bearing half — an exit code alone would not prove the traversal
/// never happened.
#[test]
fn invalid_session_id_refused_before_path_use() {
    let dir = std::env::temp_dir().join(format!("bm-test-session-id-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bad_id = "../../etc";
    let (code, j, raw) = run_in(Some(&dir), &["digest", "--session", bad_id]);
    assert_eq!(code, 2, "invalid session id must exit 2: {}", raw);
    assert!(
        j["detail"].as_str().unwrap().contains("invalid session id"),
        "detail: {}",
        j
    );
    // No file outside the intended session dir was touched.
    assert!(
        !dir.join(".browser-miner").exists(),
        "must not create session dir for invalid id"
    );
}

/// A page title is attacker-controlled text that lands in two places in the
/// output. Both are capped; this checks both, because capping `page_title` and
/// forgetting `digest.meta.title` leaves the same unbounded string one key
/// deeper.
#[test]
fn page_title_is_capped() {
    let (port, _h) = serve_fixtures();
    let url = format!("http://127.0.0.1:{}/long-title.html", port);
    let (code, d, raw) = run(&["digest", &url, "--allow-local"]);
    assert_eq!(code, 0, "digest long title: {}", raw);
    let title = d["page_title"].as_str().unwrap();
    assert_eq!(
        title.chars().count(),
        300,
        "page_title must be capped at 300 chars"
    );
    let meta_title = d["digest"]["meta"]["title"].as_str().unwrap();
    assert_eq!(
        meta_title.chars().count(),
        300,
        "digest meta title must be capped at 300 chars"
    );
}

// endregion: Exactness, cleanup, and caps
