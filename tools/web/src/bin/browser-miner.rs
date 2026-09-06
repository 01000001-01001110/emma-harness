//! `browser-miner` — the standalone CLI over Emma's fork of chromehand.
//!
//! Upstream had only this: argv in, one JSON object on stdout, an exit code
//! for the verdict. Here the work lives in `emma_tools_web::chromehand` and
//! this file is the thin shell that turns argv into a call and a
//! [`MinerError`] back into an exit code — kept so standalone use still works,
//! and so the vendored integration tests can go on exercising the whole stack
//! through a real process.
//!
//! Exit codes are upstream's, unchanged, because the 22 integration tests —
//! 21 vendored, one written for the fork — assert them directly: `0` = a
//! result *including honest negatives* (a blocked page, a rendered 404, a
//! `wait-for` that timed out) · `2` = bad input, a policy refusal, or an
//! expired session · `3` = browser failure. This is the only place in the
//! crate where those integers still exist. In the library the same taxonomy is
//! [`MinerError`], and [`quit`] below is the single point of translation.
//!
//! Note what this CLI does *not* share with Emma's tools: it calls
//! [`Policy::load`] itself, keeping upstream's fall back to a
//! `data/browser-allowlist.json` under the process's working directory. That
//! is right for a program a user runs from their own project and wrong for a
//! library — see [`emma_tools_web::chromehand::load_policy`] for the other
//! half of that decision.
//!
//! Gone with the fork: `verify --file` (the batch harness and its
//! `browser-miner.stop` kill switch) and `extract-jobs` (the job-board
//! adapters). Both are chromehand's own use cases and both are still in
//! canonical. See `VENDOR.md`.

use emma_tools_web::chromehand::{
    actions, digest, forms, policy::Policy, session, DigestOptions, LaunchedBrowser, MinerError,
    DEFAULT_MAX_TEXT_CHARS, DEFAULT_TIMEOUT_MS,
};

// region: One JSON object, and an exit code
// ---------------------------------------------------------------------------
// One JSON object, and an exit code
//
// Every path out of this program goes through one of these. They all print a
// single JSON object and none of them return, which is what keeps the exit
// contract auditable: there is nowhere else a code can be chosen.
// ---------------------------------------------------------------------------

fn usage() -> ! {
    eprintln!(
        "{}",
        serde_json::json!({
            "error": "usage",
            "usage": [
                "browser-miner digest <url> | digest --session <id> [--delta]",
                "browser-miner verify <url>",
                "browser-miner extract-form (<url> | --session <id>)",
                "browser-miner fill --session <id> --values values.json [--screenshot out.png]",
                "browser-miner submit --session <id> --selector CSS [--yes-actually-submit]",
                "browser-miner screenshot (<url> | --session <id>) --out f.png",
                "browser-miner session (open [--headful] [--attach <ws_url> | --attach-port <N>] | list | close <id|--all>)",
                "browser-miner navigate --session <id> <url>",
                "browser-miner back|forward --session <id>",
                "browser-miner wait-for --session <id> (--selector CSS | --text STR | --url-pattern RE | --network-idle)",
                "browser-miner click --session <id> --selector CSS",
                "browser-miner type --session <id> --selector CSS --text STR",
                "browser-miner select --session <id> --selector CSS --value V"
            ],
            "options": "--timeout-ms 45000 --max-text-chars 8000 --user-agent <ua> --allowlist <path> --allow-local --session-ttl 1800 [--attach <ws_url> | --attach-port <N>]",
            "notes": "One JSON object on stdout. Exit 0=result, 2=bad input/policy refusal, 3=browser failure. Interaction verbs (click/type/select/fill/submit) REQUIRE a domain allowlist. fill never submits. submit default is human-observed (needs --headful session); auto-submit needs --yes-actually-submit AND allow_auto_submit in data/browser-miner-config.json (two-key rule)."
        })
    );
    std::process::exit(2);
}

fn refuse(reason: &str) -> ! {
    println!(
        "{}",
        serde_json::json!({
            "error": "policy_refused",
            "detail": reason,
            "evidence": { "source": "browser-render", "fetch_timestamp": digest::now_iso() }
        })
    );
    std::process::exit(2);
}

fn fail(stage: &str, msg: &str) -> ! {
    println!(
        "{}",
        serde_json::json!({
            "error": stage,
            "detail": msg,
            "evidence": { "source": "browser-render", "fetch_timestamp": digest::now_iso() }
        })
    );
    std::process::exit(3);
}

fn emit(v: &serde_json::Value) {
    match serde_json::to_string_pretty(v) {
        Ok(s) => println!("{}", s),
        Err(e) => fail("serialize", &e.to_string()),
    }
}

/// The typed error, back into an exit code. `Unavailable` rejoins `Browser` at
/// exit 3 on purpose: the CLI's contract is three codes and consumers depend
/// on it, so the extra distinction stays a library-only refinement.
fn quit(e: MinerError) -> ! {
    match e {
        MinerError::Refused(m) => refuse(&m),
        other => fail(other.stage(), other.detail()),
    }
}

/// Upstream's `die`, unchanged in effect: classify a bare error string, then
/// exit on it.
fn die(err: String) -> ! {
    quit(emma_tools_web::chromehand::classify(err))
}

// endregion: One JSON object, and an exit code

// region: Argv
// ---------------------------------------------------------------------------
// Argv
//
// A hand-rolled parser rather than a dependency, and one flat `Opts` covering
// every verb's flags rather than a type per command. Unknown `--flags` fall
// through to `usage`; bare positionals accumulate, and the first one that
// looks like an http(s) URL is taken as the target.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Opts {
    url: Option<String>,
    session: Option<String>,
    values: Option<String>,
    screenshot: Option<String>,
    yes_actually_submit: bool,
    delta: bool,
    selector: Option<String>,
    text: Option<String>,
    url_pattern: Option<String>,
    network_idle: bool,
    value: Option<String>,
    out: Option<String>,
    headful: bool,
    attach: Option<String>,
    attach_port: Option<u16>,
    timeout_ms: u64,
    max_text_chars: usize,
    session_ttl: u64,
    user_agent: Option<String>,
    allowlist: Option<String>,
    allow_local: bool,
    positional: Vec<String>,
}

impl Opts {
    fn digest_options(&self) -> DigestOptions {
        DigestOptions {
            timeout_ms: self.timeout_ms,
            max_text_chars: self.max_text_chars,
            text_offset: 0,
            user_agent: self.user_agent.clone(),
            allowlist: self.allowlist.as_ref().map(Into::into),
            allow_local: self.allow_local,
        }
    }
}

fn parse_opts(args: &[String]) -> Opts {
    let mut o = Opts {
        timeout_ms: DEFAULT_TIMEOUT_MS,
        max_text_chars: DEFAULT_MAX_TEXT_CHARS,
        session_ttl: session::DEFAULT_SESSION_TTL_SECS,
        ..Default::default()
    };
    let mut i = 0;
    while i < args.len() {
        let take = |o: &mut Opts, i: &mut usize| -> String {
            let v = args.get(*i + 1).cloned().unwrap_or_default();
            let _ = o;
            *i += 2;
            v
        };
        match args[i].as_str() {
            "--session" => o.session = Some(take(&mut o, &mut i)),
            "--selector" => o.selector = Some(take(&mut o, &mut i)),
            "--text" => o.text = Some(take(&mut o, &mut i)),
            "--url-pattern" => o.url_pattern = Some(take(&mut o, &mut i)),
            "--network-idle" => {
                o.network_idle = true;
                i += 1;
            }
            "--value" => o.value = Some(take(&mut o, &mut i)),
            "--out" => o.out = Some(take(&mut o, &mut i)),
            "--values" => o.values = Some(take(&mut o, &mut i)),
            "--screenshot" => o.screenshot = Some(take(&mut o, &mut i)),
            "--yes-actually-submit" => {
                o.yes_actually_submit = true;
                i += 1;
            }
            "--delta" => {
                o.delta = true;
                i += 1;
            }
            "--user-agent" => o.user_agent = Some(take(&mut o, &mut i)),
            "--allowlist" => o.allowlist = Some(take(&mut o, &mut i)),
            "--timeout-ms" => {
                o.timeout_ms = take(&mut o, &mut i).parse().unwrap_or(DEFAULT_TIMEOUT_MS)
            }
            "--max-text-chars" => {
                o.max_text_chars = take(&mut o, &mut i)
                    .parse()
                    .unwrap_or(DEFAULT_MAX_TEXT_CHARS)
            }
            "--session-ttl" => {
                o.session_ttl = take(&mut o, &mut i)
                    .parse()
                    .unwrap_or(session::DEFAULT_SESSION_TTL_SECS)
            }
            "--headful" => {
                o.headful = true;
                i += 1;
            }
            "--attach" => o.attach = Some(take(&mut o, &mut i)),
            "--attach-port" => {
                o.attach_port = take(&mut o, &mut i).parse::<u16>().ok().filter(|p| *p != 0);
                if o.attach_port.is_none() {
                    eprintln!(
                        "{}",
                        serde_json::json!({ "error": "bad_input", "detail": "--attach-port must be a non-zero u16" })
                    );
                    std::process::exit(2);
                }
            }
            "--allow-local" => {
                o.allow_local = true;
                i += 1;
            }
            a if a.starts_with("--") => usage(),
            a => {
                o.positional.push(a.to_string());
                if o.url.is_none() && (a.starts_with("http://") || a.starts_with("https://")) {
                    o.url = Some(a.to_string());
                }
                i += 1;
            }
        }
    }
    o
}

// endregion: Argv

// region: Attach mode
// ---------------------------------------------------------------------------
// Attach mode
//
// Driving the Chrome the user already has open, as the user's own logged-in
// identity. Everything here exists to make that a deliberate act: a banner
// they cannot miss, and a connection target that must be loopback.
// ---------------------------------------------------------------------------

/// ADR-2 consent banner: printed to stderr before ANY attach action.
fn print_attach_banner() {
    eprintln!("ATTACH MODE: acting through YOUR running Chrome as your logged-in identity.");
    eprintln!("Many sites' ToS prohibit automation (LinkedIn and Wellfound explicitly litigate/ban). You are responsible for what you automate as yourself.");
    eprintln!("Chrome 136+ refuses debug ports on the default profile; you must have started Chrome yourself on a NON-default --user-data-dir.");
    eprintln!("This tool never stores or exports your cookies or credentials.");
}

/// Resolve --attach / --attach-port to a connection string suitable for
/// Browser::connect. Validates the safety constraints: exactly one attach
/// option, ws:// + localhost for --attach, non-zero port for --attach-port.
fn resolve_attach(o: &Opts) -> Result<Option<String>, String> {
    match (&o.attach, o.attach_port) {
        (Some(_), Some(_)) => Err("cannot use both --attach and --attach-port".to_string()),
        (Some(ws), None) => {
            if !ws.starts_with("ws://") {
                return Err(format!(
                    "--attach must be a ws:// DevTools endpoint (got {})",
                    ws
                ));
            }
            let parsed = url::Url::parse(ws).map_err(|e| format!("invalid --attach URL: {}", e))?;
            let host = parsed.host_str().unwrap_or("").to_lowercase();
            let is_local = match parsed.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                _ => host == "localhost" || host.ends_with(".localhost"),
            };
            if !is_local {
                return Err(format!(
                    "--attach must point to a localhost DevTools endpoint (got {})",
                    host
                ));
            }
            Ok(Some(ws.clone()))
        }
        (None, Some(port)) => Ok(Some(format!("http://127.0.0.1:{}", port))),
        (None, None) => Ok(None),
    }
}

// endregion: Attach mode

// region: The command table
// ---------------------------------------------------------------------------
// The command table
//
// One match over the verb. The shared preamble above it is deliberate
// ordering: session id validated, attach resolved, policy loaded — all before
// any verb runs, so a malformed invocation costs no browser.
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        usage();
    }
    let command = args[1].clone();
    let o = parse_opts(&args[2..]);

    if let Some(id) = &o.session {
        if let Err(e) = session::validate_id(id) {
            die(e);
        }
    }

    let attach = match resolve_attach(&o) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "{}",
                serde_json::json!({ "error": "bad_input", "detail": e })
            );
            std::process::exit(2);
        }
    };
    if o.headful && attach.is_some() {
        eprintln!(
            "{}",
            serde_json::json!({
                "error": "bad_input",
                "detail": "--headful is for managed launch only; attach connects to your already-running Chrome"
            })
        );
        std::process::exit(2);
    }

    let pol = match Policy::load(o.allowlist.as_deref(), o.allow_local) {
        Ok(p) => p,
        Err(e) => refuse(&e),
    };

    match command.as_str() {
        // ── session management ────────────────────────────────────────────
        "session" => {
            let sub = o.positional.first().map(|s| s.as_str()).unwrap_or("");
            match sub {
                "open" => {
                    if let Some(conn) = attach {
                        print_attach_banner();
                        match session::open_attach(&conn).await {
                            Ok(v) => emit(&v),
                            Err(e) => die(e),
                        }
                    } else {
                        match session::open(o.headful).await {
                            Ok(v) => emit(&v),
                            Err(e) => die(e),
                        }
                    }
                }
                "list" => emit(&session::list(o.session_ttl)),
                "close" => {
                    let target = o.positional.get(1).map(|s| s.as_str()).unwrap_or("");
                    if target.is_empty() {
                        usage();
                    }
                    emit(&session::close(target).await);
                }
                _ => usage(),
            }
        }

        // ── digest: stateless or in-session ───────────────────────────────
        "digest" => {
            if o.delta && o.session.is_none() {
                usage(); // --delta needs a session to diff against
            }

            // Attach mode: connect to the user's Chrome, optionally navigate,
            // then digest the current page. Never launch or kill Chrome.
            if o.session.is_none() {
                if let Some(conn) = &attach {
                    print_attach_banner();
                    let c = session::connect_direct(conn)
                        .await
                        .unwrap_or_else(|e| die(e));
                    if let Some(url) = &o.url {
                        if let Err(e) = pol.check(url) {
                            session::disconnect(c);
                            refuse(&e);
                        }
                        if let Err(e) = actions::navigate(&c, url).await {
                            session::disconnect(c);
                            die(e);
                        }
                    }
                    let r = actions::session_digest(&c, digest::TextWindow::head(o.max_text_chars))
                        .await;
                    session::disconnect(c);
                    match r {
                        Ok(mut out) => {
                            actions::annotate_attached(&mut out, true);
                            emit(&out);
                        }
                        Err(e) => die(e),
                    }
                    return;
                }
            }

            match (&o.session, &o.url) {
                (Some(id), _) => {
                    let c = session::connect(id, o.session_ttl)
                        .await
                        .unwrap_or_else(|e| die(e));
                    let r = actions::session_digest(&c, digest::TextWindow::head(o.max_text_chars))
                        .await;
                    session::disconnect(c);
                    match r {
                        Ok(current) => {
                            if o.delta {
                                let prior = session::read_digest_snapshot(id).ok();
                                let (snapshot, out) =
                                    digest::compute_delta(id, prior.as_ref(), &current);
                                if let Err(e) = session::write_digest_snapshot(id, &snapshot) {
                                    fail("snapshot", &e);
                                }
                                emit(&out);
                            } else {
                                emit(&current);
                            }
                        }
                        Err(e) => die(e),
                    }
                }
                (None, Some(url)) => {
                    match emma_tools_web::chromehand::digest_url(url, &o.digest_options()).await {
                        Ok(v) => emit(&v),
                        Err(e) => quit(e),
                    }
                }
                _ => usage(),
            }
        }

        // ── verify: single URL ────────────────────────────────────────────
        "verify" => match &o.url {
            Some(url) => {
                if let Some(conn) = &attach {
                    if let Err(e) = pol.check(url) {
                        refuse(&e);
                    }
                    print_attach_banner();
                    let c = session::connect_direct(conn)
                        .await
                        .unwrap_or_else(|e| die(e));
                    if let Err(e) = actions::navigate(&c, url).await {
                        session::disconnect(c);
                        die(e);
                    }
                    let probed = digest::probe_current(&c.page).await;
                    session::disconnect(c);
                    match probed {
                        Ok(p) => {
                            let mut out = digest::assemble(
                                "verify",
                                url,
                                p,
                                false,
                                digest::TextWindow::head(o.max_text_chars),
                            );
                            actions::annotate_attached(&mut out, true);
                            emit(&out);
                        }
                        Err(e) => die(e),
                    }
                    return;
                }
                match emma_tools_web::chromehand::verify_url(url, &o.digest_options()).await {
                    Ok(v) => emit(&v),
                    Err(e) => quit(e),
                }
            }
            None => usage(),
        },

        // ── extract-form: deepened form inventory (P3) ────────────────────
        "extract-form" => match (&o.session, &o.url) {
            (Some(id), _) => {
                let c = session::connect(id, o.session_ttl)
                    .await
                    .unwrap_or_else(|e| die(e));
                let r = forms::extract_form(&c.page).await;
                let url = c.page.url().await.ok().flatten().unwrap_or_default();
                let sid = c.id.clone();
                session::disconnect(c);
                match r {
                    Ok(form) => emit(&serde_json::json!({
                        "command": "extract-form", "session": sid, "final_url": url,
                        "form": form, "evidence": actions::evidence()
                    })),
                    Err(e) => die(e),
                }
            }
            (None, Some(url)) => {
                if let Err(e) = pol.check(url) {
                    refuse(&e);
                }
                let launched = LaunchedBrowser::launch(o.user_agent.as_deref())
                    .await
                    .unwrap_or_else(|e| quit(e));
                let run = async {
                    let page = launched
                        .browser
                        .new_page(url.as_str())
                        .await
                        .map_err(|e| e.to_string())?;
                    actions::bounded_nav_wait(&page, 10_000).await;
                    tokio::time::sleep(std::time::Duration::from_millis(2_000)).await;
                    let form = forms::extract_form(&page).await?;
                    let final_url = page.url().await.ok().flatten().unwrap_or_default();
                    let _ = page.close().await;
                    Ok::<serde_json::Value, String>(serde_json::json!({
                        "command": "extract-form", "url": url, "final_url": final_url,
                        "form": form, "evidence": actions::evidence()
                    }))
                };
                let r = tokio::time::timeout(std::time::Duration::from_millis(o.timeout_ms), run)
                    .await
                    .unwrap_or_else(|_| {
                        Err(format!("timeout: no result within {} ms", o.timeout_ms))
                    });
                launched.teardown().await;
                match r {
                    Ok(v) => emit(&v),
                    Err(e) => die(e),
                }
            }
            _ => usage(),
        },

        // ── fill (P4): never submits ──────────────────────────────────────
        "fill" => {
            let (id, values_path) = match (&o.session, &o.values) {
                (Some(id), Some(v)) => (id.clone(), v.clone()),
                _ => usage(),
            };
            let spec = match forms::read_fill_spec(&values_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "{}",
                        serde_json::json!({ "error": "bad_input", "detail": e })
                    );
                    std::process::exit(2);
                }
            };
            let c = session::connect(&id, o.session_ttl)
                .await
                .unwrap_or_else(|e| die(e));
            let guard_ms = o.timeout_ms.saturating_add(15_000);
            let r = tokio::time::timeout(
                std::time::Duration::from_millis(guard_ms),
                forms::fill(&c, &pol, &spec, o.screenshot.as_deref()),
            )
            .await
            .unwrap_or_else(|_| Err(format!("timeout: fill exceeded {} ms", guard_ms)));
            session::disconnect(c);
            match r {
                Ok(v) => emit(&v),
                Err(e) => die(e),
            }
        }

        // ── submit (P4): two-key rule ─────────────────────────────────────
        "submit" => {
            let (id, selector) = match (&o.session, &o.selector) {
                (Some(id), Some(sel)) => (id.clone(), sel.clone()),
                _ => usage(),
            };
            let c = session::connect(&id, o.session_ttl)
                .await
                .unwrap_or_else(|e| die(e));
            let headful = c.headful;
            let r = forms::submit(
                &c,
                &pol,
                &selector,
                o.yes_actually_submit,
                headful,
                o.timeout_ms,
            )
            .await;
            session::disconnect(c);
            match r {
                Ok(v) => emit(&v),
                Err(e) => die(e),
            }
        }

        // ── screenshot: stateless or in-session ───────────────────────────
        "screenshot" => {
            let out_path = match &o.out {
                Some(p) => p.clone(),
                None => usage(),
            };
            match (&o.session, &o.url) {
                (Some(id), _) => {
                    let c = session::connect(id, o.session_ttl)
                        .await
                        .unwrap_or_else(|e| die(e));
                    let r = actions::screenshot(&c.page, &out_path).await;
                    session::disconnect(c);
                    match r {
                        Ok(v) => emit(&v),
                        Err(e) => die(e),
                    }
                }
                (None, Some(url)) => {
                    if let Err(e) = pol.check(url) {
                        refuse(&e);
                    }
                    let launched = LaunchedBrowser::launch(o.user_agent.as_deref())
                        .await
                        .unwrap_or_else(|e| quit(e));
                    let page = match launched.browser.new_page(url.as_str()).await {
                        Ok(p) => p,
                        Err(e) => {
                            let msg = format!("navigate: {}", e);
                            launched.teardown().await;
                            fail("browser", &msg)
                        }
                    };
                    let _ = page.wait_for_navigation().await;
                    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
                    let r = actions::screenshot(&page, &out_path).await;
                    let _ = page.close().await;
                    launched.teardown().await;
                    match r {
                        Ok(v) => emit(&v),
                        Err(e) => die(e),
                    }
                }
                _ => usage(),
            }
        }

        // ── session action verbs ──────────────────────────────────────────
        "navigate" | "back" | "forward" | "wait-for" | "click" | "type" | "select" => {
            let id = match &o.session {
                Some(id) => id.clone(),
                None => usage(),
            };
            // navigate is a read verb: its target URL passes normal policy.
            if command == "navigate" {
                match &o.url {
                    Some(u) => {
                        if let Err(e) = pol.check(u) {
                            refuse(&e);
                        }
                    }
                    None => usage(),
                }
            }
            let c = session::connect(&id, o.session_ttl)
                .await
                .unwrap_or_else(|e| die(e));
            // Outer guard so no session verb can hang past --timeout-ms.
            let action = async {
                match command.as_str() {
                    "navigate" => actions::navigate(&c, o.url.as_deref().unwrap()).await,
                    "back" | "forward" => actions::history(&c, &command).await,
                    "wait-for" => {
                        actions::wait_for(
                            &c,
                            o.selector.as_deref(),
                            o.text.as_deref(),
                            o.url_pattern.as_deref(),
                            o.network_idle,
                            o.timeout_ms,
                        )
                        .await
                    }
                    "click" => match &o.selector {
                        Some(sel) => actions::click(&c, &pol, sel).await,
                        None => Err("refused: click needs --selector".to_string()),
                    },
                    "type" => match (&o.selector, &o.text) {
                        (Some(sel), Some(text)) => actions::type_text(&c, &pol, sel, text).await,
                        _ => Err("refused: type needs --selector and --text".to_string()),
                    },
                    "select" => match (&o.selector, &o.value) {
                        (Some(sel), Some(val)) => actions::select(&c, &pol, sel, val).await,
                        _ => Err("refused: select needs --selector and --value".to_string()),
                    },
                    _ => unreachable!(),
                }
            };
            // wait-for manages its own deadline; give it headroom on top.
            let guard_ms = o.timeout_ms.saturating_add(15_000);
            let r = tokio::time::timeout(std::time::Duration::from_millis(guard_ms), action)
                .await
                .unwrap_or_else(|_| Err(format!("timeout: session verb exceeded {} ms", guard_ms)));
            session::disconnect(c);
            match r {
                Ok(v) => emit(&v),
                Err(e) => die(e),
            }
        }

        _ => usage(),
    }
}

// endregion: The command table
