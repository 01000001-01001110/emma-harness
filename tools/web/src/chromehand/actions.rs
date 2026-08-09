//! Session action verbs (ADR-4.3): navigate, back/forward, wait-for, click,
//! type, select, screenshot, and in-session digest. Policy enforcement per
//! ADR-4: read verbs (navigate/wait-for/digest/screenshot) follow the normal
//! URL policy; INTERACTION verbs (click/type/select) additionally REQUIRE a
//! user-owned allowlist — reading the web is ordinary, acting on it is opt-in.
//!
//! Structural refusals: `click` refuses submit-type controls (submission is
//! P4's two-key `submit`, not built); `type` refuses password fields — the
//! action vocabulary has no code path into credentials.

use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::network::{EventResponseReceived, ResourceType};
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::Page;
use futures::StreamExt;

use crate::chromehand::digest::{self, now_iso};
use crate::chromehand::policy::Policy;
use crate::chromehand::session::Connected;

/// In-page helper for resolving selectors that pierce open shadow roots
/// (` >>> `) and same-origin iframe documents (` ||| `). The two delimiters
/// compose left-to-right in a single resolver. Cross-origin frames and closed
/// shadow roots are unreachable and reported as not found.
pub fn selector_needs_in_page(selector: &str) -> bool {
    selector.contains(" >>> ") || selector.contains(" ||| ")
}

pub const RESOLVE_JS: &str = r##"
function __bmTokenizeSelector(selector) {
  const tokens = [];
  let remaining = selector;
  while (remaining.length > 0) {
    const shadowIdx = remaining.indexOf(' >>> ');
    const frameIdx = remaining.indexOf(' ||| ');
    let delim = '';
    let idx = -1;
    if (shadowIdx !== -1 && (frameIdx === -1 || shadowIdx < frameIdx)) {
      idx = shadowIdx;
      delim = ' >>> ';
    } else if (frameIdx !== -1) {
      idx = frameIdx;
      delim = ' ||| ';
    }
    if (idx === -1) {
      tokens.push({ type: 'sel', value: remaining });
      break;
    }
    if (idx > 0) {
      tokens.push({ type: 'sel', value: remaining.slice(0, idx) });
    }
    tokens.push({ type: 'delim', value: delim });
    remaining = remaining.slice(idx + delim.length);
  }
  return tokens;
}
function __bmResolve(selector) {
  const tokens = __bmTokenizeSelector(selector);
  let root = document;
  let el = null;
  for (let i = 0; i < tokens.length; i++) {
    const tok = tokens[i];
    if (tok.type === 'delim') continue;
    el = root.querySelector(tok.value);
    if (!el) return { el: null };
    if (i + 1 < tokens.length && tokens[i + 1].type === 'delim') {
      const delim = tokens[i + 1].value;
      if (delim === ' >>> ') {
        root = el.shadowRoot;
        if (!root) return { el: null, error: 'shadow root not found' };
      } else if (delim === ' ||| ') {
        try { root = el.contentDocument; } catch (e) { root = null; }
        if (!root) return { el: null, error: 'cross-origin frame — unreachable without frame attachment (future work)' };
      }
      i++;
    }
  }
  return { el };
}
function __bmResolveEl(selector) {
  return __bmResolve(selector).el;
}
function __bmResolveMeta(selector) {
  const res = __bmResolve(selector);
  if (!res.el) {
    const out = { found: false };
    if (res.error) out.error = res.error;
    return out;
  }
  const el = res.el;
  const tag = el.tagName.toLowerCase();
  const type = (el.getAttribute('type') || '').toLowerCase();
  const inForm = !!el.closest('form');
  let isSubmit = false;
  if (tag === 'input' && type === 'submit') isSubmit = true;
  if (tag === 'button' && (type === 'submit' || (!type && inForm))) isSubmit = true;
  return { found: true, tag, type, inForm, isSubmit, editable: el.isContentEditable === true, disabled: !!el.disabled };
}
"##;

/// wait_for_navigation can block forever when no observable navigation
/// happens (bfcache restores, same-document changes) — always bound it.
pub async fn bounded_nav_wait(page: &Page, ms: u64) {
    let _ = tokio::time::timeout(Duration::from_millis(ms), page.wait_for_navigation()).await;
}

pub fn evidence() -> serde_json::Value {
    serde_json::json!({
        "source": "browser-render",
        "engine": format!("browser-miner/{} chromiumoxide", env!("CARGO_PKG_VERSION")),
        "fetch_timestamp": now_iso()
    })
}

/// Annotate an output value and its evidence block when it came from an
/// attached session (ADR-2 auditability).
pub fn annotate_attached(v: &mut serde_json::Value, attached: bool) {
    if attached {
        v["attached"] = serde_json::json!(true);
        if let Some(ev) = v.get_mut("evidence") {
            ev["attached"] = serde_json::json!(true);
        }
    }
}

/// Small deterministic page snapshot for action results: where are we, what
/// does it look like, is it a block page. No payload — agents re-`digest`.
pub async fn mini_digest(page: &Page) -> serde_json::Value {
    let js = r#"(() => JSON.stringify({
        href: location.href,
        title: document.title,
        text_head: (document.body ? document.body.innerText : '').replace(/\s+/g,' ').slice(0, 600)
    }))()"#;
    let snap: serde_json::Value = page
        .evaluate(js)
        .await
        .ok()
        .and_then(|v| v.into_value::<String>().ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));
    let title = snap.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let text = snap.get("text_head").and_then(|v| v.as_str()).unwrap_or("");
    let lower = text.to_lowercase();
    let href = snap
        .get("href")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let looks_blocked = digest::looks_blocked(title, &lower, &href);
    let title = title.chars().take(300).collect::<String>();
    serde_json::json!({
        "final_url": href,
        "page_title": title,
        "looks_blocked": looks_blocked
    })
}

/// `navigate --session S <url>` — full navigation with observed main-document
/// status (listener attached BEFORE goto, evidence not guesswork).
pub async fn navigate(c: &Connected, url: &str) -> Result<serde_json::Value, String> {
    // Network events are enabled per CDP session; a reconnected session needs
    // its own enable or responseReceived never fires (status would be null).
    let _ = c
        .page
        .execute(chromiumoxide::cdp::browser_protocol::network::EnableParams::default())
        .await;
    let mut responses = c
        .page
        .event_listener::<EventResponseReceived>()
        .await
        .map_err(|e| format!("event listener: {}", e))?;
    let status_task = tokio::task::spawn(async move {
        let mut first_doc: Option<i64> = None;
        while let Some(ev) = responses.next().await {
            if ev.r#type == ResourceType::Document && first_doc.is_none() {
                first_doc = Some(ev.response.status);
            }
        }
        first_doc
    });

    let nav_err = c.page.goto(url).await.err().map(|e| e.to_string());
    bounded_nav_wait(&c.page, 10_000).await;
    let _ = digest::settle_network_quiet(&c.page, 500, 2_500).await;

    let mut out = mini_digest(&c.page).await;
    status_task.abort();
    let mut http_status = status_task.await.ok().flatten();
    if http_status.is_none() {
        // Reconnected CDP sessions can miss Network events; the browser's own
        // navigation record still carries the OBSERVED status (Chrome 109+).
        // 0 means unavailable — stays null, never invented.
        http_status = c
            .page
            .evaluate(
                "(() => { const e = performance.getEntriesByType('navigation')[0]; return e && e.responseStatus ? e.responseStatus : 0; })()",
            )
            .await
            .ok()
            .and_then(|v| v.into_value::<i64>().ok())
            .filter(|s| *s > 0);
    }

    if let Some(e) = &nav_err {
        let empty = out
            .get("page_title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty();
        if empty {
            return Err(format!("navigation failed: {}", e));
        }
    }

    let blocked = out["looks_blocked"].as_bool().unwrap_or(false);
    out["command"] = serde_json::json!("navigate");
    out["session"] = serde_json::json!(c.id);
    out["url"] = serde_json::json!(url);
    out["http_status"] = serde_json::json!(http_status);
    out["outcome"] = serde_json::json!(digest::outcome(blocked, http_status));
    out["evidence"] = evidence();
    Ok(out)
}

/// `back` / `forward` — history navigation. Status is often served from
/// cache/bfcache and unobservable; reported null, never invented, no outcome.
pub async fn history(c: &Connected, direction: &str) -> Result<serde_json::Value, String> {
    // CDP history navigation — deterministic, no JS. (Evaluating
    // history.back() races the context teardown and can hang the session.)
    use chromiumoxide::cdp::browser_protocol::page::{
        GetNavigationHistoryParams, NavigateToHistoryEntryParams,
    };
    let hist = c
        .page
        .execute(GetNavigationHistoryParams::default())
        .await
        .map_err(|e| format!("navigation history: {}", e))?;
    let delta: i64 = if direction == "back" { -1 } else { 1 };
    let target_idx = hist.result.current_index + delta;
    let entry = if target_idx >= 0 {
        hist.result.entries.get(target_idx as usize)
    } else {
        None
    };
    let Some(entry) = entry else {
        return Ok(serde_json::json!({
            "command": direction,
            "session": c.id,
            "navigated": false,
            "reason": "no history entry in that direction",
            "evidence": evidence()
        }));
    };
    c.page
        .execute(NavigateToHistoryEntryParams::new(entry.id))
        .await
        .map_err(|e| format!("history navigate: {}", e))?;
    bounded_nav_wait(&c.page, 5_000).await;
    let _ = digest::settle_network_quiet(&c.page, 500, 2_500).await;
    let mut out = mini_digest(&c.page).await;
    out["command"] = serde_json::json!(direction);
    out["session"] = serde_json::json!(c.id);
    out["http_status"] = serde_json::Value::Null;
    out["evidence"] = evidence();
    Ok(out)
}

/// `wait-for --session S (--selector CSS | --text STR | --url-pattern RE | --network-idle)
/// [--timeout-ms N]` — deterministic condition wait. Timeout is a RESULT
/// ({met:false}, exit 0), not a crash.
pub async fn wait_for(
    c: &Connected,
    selector: Option<&str>,
    text: Option<&str>,
    url_pattern: Option<&str>,
    network_idle: bool,
    timeout_ms: u64,
) -> Result<serde_json::Value, String> {
    let start = std::time::Instant::now();
    let deadline = start + Duration::from_millis(timeout_ms);
    let mut met = false;

    if let Some(pattern) = url_pattern {
        let re = regex::Regex::new(pattern)
            .map_err(|e| format!("refused: invalid --url-pattern regex: {}", e))?;
        while std::time::Instant::now() < deadline {
            if let Ok(Some(url)) = c.page.url().await {
                if re.is_match(&url) {
                    met = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    } else if network_idle {
        let remaining = deadline
            .saturating_duration_since(std::time::Instant::now())
            .as_millis() as u64;
        met = digest::settle_network_quiet(&c.page, 500, remaining.max(300)).await;
    } else {
        let cond_js = match (selector, text) {
            (Some(sel), _) => format!(
                "(() => {{ {resolver} return !!__bmResolveEl({sel}); }})()",
                resolver = RESOLVE_JS,
                sel = serde_json::to_string(sel).unwrap()
            ),
            (None, Some(t)) => format!(
                "(() => (document.body ? document.body.innerText : '').includes({}))()",
                serde_json::to_string(t).unwrap()
            ),
            (None, None) => {
                return Err(
                    "wait-for needs --selector or --text or --url-pattern or --network-idle"
                        .to_string(),
                )
            }
        };
        while std::time::Instant::now() < deadline {
            if let Ok(v) = c.page.evaluate(cond_js.as_str()).await {
                if v.into_value::<bool>().unwrap_or(false) {
                    met = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    Ok(serde_json::json!({
        "command": "wait-for",
        "session": c.id,
        "met": met,
        "waited_ms": start.elapsed().as_millis() as u64,
        "condition": { "selector": selector, "text": text, "url_pattern": url_pattern, "network_idle": network_idle },
        "evidence": evidence()
    }))
}

/// Interaction-verb gate: allowlist REQUIRED, current page URL must pass.
async fn interaction_gate(c: &Connected, pol: &Policy) -> Result<(), String> {
    let url = c.page.url().await.ok().flatten().unwrap_or_default();
    pol.check_interaction(&url)
}

/// `click --session S --selector CSS` — bounded interaction for non-submitting
/// controls. Submit-typed controls are refused (exit 2): submission is P4's
/// two-key `submit` verb, which does not exist yet.
pub async fn click(
    c: &Connected,
    pol: &Policy,
    selector: &str,
) -> Result<serde_json::Value, String> {
    interaction_gate(c, pol).await?;

    let sel_json = serde_json::to_string(selector).unwrap();
    let precheck_js = format!(
        r#"(() => {{ {resolver}
            return JSON.stringify(__bmResolveMeta({sel}));
        }})()"#,
        resolver = RESOLVE_JS,
        sel = sel_json
    );
    let pre: serde_json::Value = c
        .page
        .evaluate(precheck_js.as_str())
        .await
        .map_err(|e| e.to_string())?
        .into_value::<String>()
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));

    if !pre.get("found").and_then(|v| v.as_bool()).unwrap_or(false) {
        if let Some(err) = pre.get("error").and_then(|v| v.as_str()) {
            return Err(err.to_string());
        }
        return Ok(serde_json::json!({
            "command": "click", "session": c.id, "selector": selector,
            "clicked": false, "error": "selector matched no element",
            "evidence": evidence()
        }));
    }
    if pre
        .get("isSubmit")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Err(format!(
            "refused: {} is a submit-type control — submission requires the `submit` verb (P4, two-key rule), which is not built. click handles non-submitting controls only",
            selector
        ));
    }

    if selector_needs_in_page(selector) {
        let click_js = format!(
            r#"(() => {{ {resolver}
                const el = __bmResolveEl({sel});
                if (!el) return 'selector matched no element';
                el.click();
                return '';
            }})()"#,
            resolver = RESOLVE_JS,
            sel = sel_json
        );
        let err: String = c
            .page
            .evaluate(click_js.as_str())
            .await
            .map_err(|e| format!("click: {}", e))?
            .into_value::<String>()
            .unwrap_or_default();
        if !err.is_empty() {
            return Err(format!("click: {}", err));
        }
    } else {
        let el = c
            .page
            .find_element(selector)
            .await
            .map_err(|e| format!("find element: {}", e))?;
        el.click().await.map_err(|e| format!("click: {}", e))?;
    }
    bounded_nav_wait(&c.page, 5_000).await;
    let _ = digest::settle_network_quiet(&c.page, 500, 2_500).await;

    let mut out = mini_digest(&c.page).await;
    out["command"] = serde_json::json!("click");
    out["session"] = serde_json::json!(c.id);
    out["selector"] = serde_json::json!(selector);
    out["clicked"] = serde_json::json!(true);
    out["control"] = pre;
    out["evidence"] = evidence();
    Ok(out)
}

/// `type --session S --selector CSS --text STR` — single-field typing with
/// real key events (JS validation fires). Password fields refused
/// structurally. Returns the field's live value re-read from the DOM.
pub async fn type_text(
    c: &Connected,
    pol: &Policy,
    selector: &str,
    text: &str,
) -> Result<serde_json::Value, String> {
    interaction_gate(c, pol).await?;

    let sel_json = serde_json::to_string(selector).unwrap();
    let precheck_js = format!(
        r#"(() => {{ {resolver}
            return JSON.stringify(__bmResolveMeta({sel}));
        }})()"#,
        resolver = RESOLVE_JS,
        sel = sel_json
    );
    let pre: serde_json::Value = c
        .page
        .evaluate(precheck_js.as_str())
        .await
        .map_err(|e| e.to_string())?
        .into_value::<String>()
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));

    if !pre.get("found").and_then(|v| v.as_bool()).unwrap_or(false) {
        if let Some(err) = pre.get("error").and_then(|v| v.as_str()) {
            return Err(err.to_string());
        }
        return Ok(serde_json::json!({
            "command": "type", "session": c.id, "selector": selector,
            "ok": false, "error": "selector matched no element",
            "evidence": evidence()
        }));
    }
    if pre.get("type").and_then(|v| v.as_str()) == Some("password") {
        return Err(
            "refused: password fields are structurally unreachable — the binary never handles credentials"
                .to_string(),
        );
    }
    let tag = pre.get("tag").and_then(|v| v.as_str()).unwrap_or("");
    let editable = pre
        .get("editable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let is_single_line = tag == "input" && !editable;
    if is_single_line && (text.contains('\n') || text.contains('\r')) {
        return Err(format!(
            "refused: newline in --text would trigger implicit form submission on single-line input {} — multi-line values are only allowed for textarea",
            selector
        ));
    }

    // Textarea and contenteditable need direct value/innerText assignment:
    // chromiumoxide's type_str cannot represent literal newlines, and
    // contenteditable has no .value.
    let use_js_path = selector_needs_in_page(selector) || tag == "textarea" || editable;
    if use_js_path {
        let type_js = format!(
            r#"(() => {{ {resolver}
                const el = __bmResolveEl({sel});
                if (!el) return 'selector matched no element';
                el.focus();
                if (el.isContentEditable) el.innerText = {text};
                else el.value = {text};
                el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                return '';
            }})()"#,
            resolver = RESOLVE_JS,
            sel = sel_json,
            text = serde_json::to_string(text).unwrap()
        );
        let err: String = c
            .page
            .evaluate(type_js.as_str())
            .await
            .map_err(|e| format!("type: {}", e))?
            .into_value::<String>()
            .unwrap_or_default();
        if !err.is_empty() {
            return Err(format!("type: {}", err));
        }
    } else {
        let el = c
            .page
            .find_element(selector)
            .await
            .map_err(|e| format!("find element: {}", e))?;
        el.click()
            .await
            .map_err(|e| format!("focus click: {}", e))?;
        el.type_str(text)
            .await
            .map_err(|e| format!("type: {}", e))?;
    }

    // Re-read the live value — report what the DOM now holds, not what we sent.
    let read_js = format!(
        "(() => {{ {resolver} const el = __bmResolveEl({sel}); return el ? String(el.value ?? el.innerText ?? '') : ''; }})()",
        resolver = RESOLVE_JS,
        sel = sel_json
    );
    let now_contains: String = c
        .page
        .evaluate(read_js.as_str())
        .await
        .ok()
        .and_then(|v| v.into_value::<String>().ok())
        .unwrap_or_default();

    Ok(serde_json::json!({
        "command": "type",
        "session": c.id,
        "selector": selector,
        "requested": text,
        "now_contains": now_contains,
        "ok": now_contains == text,
        "evidence": evidence()
    }))
}

/// `select --session S --selector CSS --value V` — option by value or visible
/// text; fires input+change so framework listeners see it.
pub async fn select(
    c: &Connected,
    pol: &Policy,
    selector: &str,
    value: &str,
) -> Result<serde_json::Value, String> {
    interaction_gate(c, pol).await?;

    let js = format!(
        r#"(() => {{ {resolver}
            const el = __bmResolveEl({sel});
            if (!el || el.tagName !== 'SELECT') return JSON.stringify({{ ok: false, error: 'no select element at selector' }});
            let match_ = null;
            for (const o of el.options) if (o.value === {val} || o.text.trim() === {val}) {{ match_ = o; break; }}
            if (!match_) return JSON.stringify({{ ok: false, error: 'no option matching value or visible text', options: Array.from(el.options).slice(0, 30).map(o => o.text.trim()) }});
            el.value = match_.value;
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return JSON.stringify({{ ok: true, value: el.value, text: match_.text.trim() }});
        }})()"#,
        resolver = RESOLVE_JS,
        sel = serde_json::to_string(selector).unwrap(),
        val = serde_json::to_string(value).unwrap()
    );
    let mut out: serde_json::Value = c
        .page
        .evaluate(js.as_str())
        .await
        .map_err(|e| e.to_string())?
        .into_value::<String>()
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({ "ok": false, "error": "evaluation failed" }));
    out["command"] = serde_json::json!("select");
    out["session"] = serde_json::json!(c.id);
    out["selector"] = serde_json::json!(selector);
    out["requested"] = serde_json::json!(value);
    out["evidence"] = evidence();
    Ok(out)
}

/// `screenshot (--session S | <url>) --out f.png` — evidence capture.
pub async fn screenshot(page: &Page, out_path: &str) -> Result<serde_json::Value, String> {
    let params = ScreenshotParams::builder().full_page(true).build();
    page.save_screenshot(params, out_path)
        .await
        .map_err(|e| format!("screenshot: {}", e))?;
    let bytes = std::fs::metadata(out_path).map(|m| m.len()).unwrap_or(0);
    Ok(serde_json::json!({
        "command": "screenshot",
        "out": out_path,
        "bytes": bytes,
        "final_url": page.url().await.ok().flatten(),
        "evidence": evidence()
    }))
}

/// `digest --session S` — the full digest of the CURRENT page state. A
/// content read, not a navigation: no main-document status is observable, so
/// `http_status` is null and there is NO liveness `outcome` (use `navigate`'s
/// result for that evidence). Never fabricated.
pub async fn session_digest(
    c: &Connected,
    max_text_chars: usize,
) -> Result<serde_json::Value, String> {
    let probe = digest::probe_current(&c.page).await?;
    let mut out = digest::assemble(
        "digest",
        &probe.final_url.clone(),
        probe,
        true,
        max_text_chars,
    );
    if let Some(obj) = out.as_object_mut() {
        obj.remove("outcome");
        obj.insert("session".into(), serde_json::json!(c.id));
        obj.insert(
            "note".into(),
            serde_json::json!("in-session content read: http_status unobservable (see the prior navigate result for status evidence)"),
        );
    }
    annotate_attached(&mut out, c.attached);
    Ok(out)
}
