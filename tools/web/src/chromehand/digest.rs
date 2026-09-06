//! The digest primitive: render one page in the shared Chrome, run the
//! deterministic in-page extraction, and assemble the output contract.
//! No model anywhere; every extraction is a DOM walk.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::network::{
    EventLoadingFinished, EventRequestWillBeSent, EventResponseReceived, ResourceType,
};
use futures::StreamExt;
use tokio::task::JoinHandle;

// region: Readable extraction
// ---------------------------------------------------------------------------
// Readable extraction
//
// One long string of JavaScript, and the largest single thing in the crate.
// Every budget in it is deliberate: a digest that does not fit a context
// window is not a digest. The comments inside are scars — a truncated selector
// path that clicked the wrong element, hidden elements whose selectors could
// not be acted on, page chrome that ate the whole link budget before the
// article appeared.
// ---------------------------------------------------------------------------

pub const DEFAULT_TIMEOUT_MS: u64 = 45_000;
/// The `browser-miner` CLI's default. Emma's `WebFetch` sets its own, larger
/// one — see `crate::fetch::DEFAULT_MAX_CHARS` — because a CLI default is read
/// by a human with a scrollback and the tool default is spent from a context
/// window.
pub const DEFAULT_MAX_TEXT_CHARS: usize = 8_000;

/// Which slice of a page's text one read returns.
///
/// **Why characters and not blocks or paragraphs.** A window has to be named
/// by an index both sides can compute from what the digest already reports,
/// and the only such number is `text_chars_total`. Block indices would need a
/// segmentation that survives being computed twice, on two separate renders of
/// a page that may have changed between them — a guarantee nothing here can
/// make. Characters also compose with `max_chars` the way the filesystem
/// `Read` tool's `offset` composes with its `limit`, which is the pattern the
/// model has already learned.
///
/// The full text is in hand when this is applied (`Probe::text` arrives
/// uncapped from the in-page walk), so a window costs nothing here. Holding
/// that text so the *next* window is free would be a cache with a lifetime, an
/// eviction policy and an owner, and `WebFetch` is deliberately stateless. The
/// price of continuing is therefore one more page load, and the notice says so
/// rather than implying a free seek.
#[derive(Debug, Clone, Copy)]
pub struct TextWindow {
    /// Characters to skip. Zero is the head of the page.
    pub offset: usize,
    /// Characters to return from `offset`.
    pub max_chars: usize,
}

impl TextWindow {
    /// The window every caller that has no offset to continue from wants.
    pub fn head(max_chars: usize) -> Self {
        Self {
            offset: 0,
            max_chars,
        }
    }
}

/// In-page extraction script. Deterministic DOM walk — no model anywhere.
/// Returns a JSON string with meta, boilerplate-stripped main text, structured
/// data (JSON-LD + tables), and an interactive inventory (forms, fields,
/// buttons, links) each carrying a stable selector for later `interact` use.
/// All sections are bounded so the digest stays context-window sized.
const DIGEST_JS: &str = r##"
(() => {
  const clip = (s, n) => { s = (s || '').replace(/\s+/g, ' ').trim(); return s.length > n ? s.slice(0, n) : s; };
  const localSel = (el, root) => {
    if (el.id) return '#' + CSS.escape(el.id);
    if (el.name && el.tagName !== 'A') {
      const q = el.tagName.toLowerCase() + '[name=' + JSON.stringify(el.name) + ']';
      try { if (root.querySelectorAll(q).length === 1) return q; } catch (e) {}
    }
    // Walk all the way to an id ancestor or the root boundary — a truncated
    // path is not a selector, it's a lottery ticket (a depth-5 cap once
    // emitted the non-unique 'a:nth-of-type(1)' and click hit the wrong element).
    const parts = [];
    let n = el, anchored = false;
    for (let d = 0; n && n.nodeType === 1 && n.tagName !== 'BODY' && d < 32; d++) {
      let i = 1, sib = n;
      while ((sib = sib.previousElementSibling)) if (sib.tagName === n.tagName) i++;
      parts.unshift(n.tagName.toLowerCase() + ':nth-of-type(' + i + ')');
      if (n.id) { parts[0] = '#' + CSS.escape(n.id); anchored = true; break; }
      n = n.parentElement;
    }
    if (!anchored && root === document) parts.unshift('body');
    return parts.join(' > ');
  };
  const sel = (el) => {
    const root = el.getRootNode();
    if (root instanceof ShadowRoot) {
      const host = root.host;
      return sel(host) + ' >>> ' + localSel(el, root);
    }
    return localSel(el, root);
  };
  const labelFor = (el) => {
    if (el.labels && el.labels.length) return clip(el.labels[0].innerText, 120);
    return clip(el.getAttribute('aria-label') || el.placeholder || el.innerText || el.value || el.title || '', 120);
  };

  const jsonld = [];
  for (const s of document.querySelectorAll('script[type="application/ld+json"]')) {
    try { jsonld.push(JSON.parse(s.textContent)); } catch (e) {}
    if (jsonld.length >= 5) break;
  }
  const tables = [];
  for (const t of document.querySelectorAll('table')) {
    const rows = [];
    for (const tr of t.querySelectorAll('tr')) {
      rows.push(Array.from(tr.querySelectorAll('th,td')).map((c) => clip(c.innerText, 80)).slice(0, 12));
      if (rows.length >= 20) break;
    }
    if (rows.length) tables.push(rows);
    if (tables.length >= 3) break;
  }

  // Only inventory elements an agent can actually ACT on: hidden elements
  // (collapsed menus, offscreen drawers) produced selectors whose click then
  // failed "not visible" — accuracy means visible-only.
  const visible = (el) => {
    if (el.checkVisibility) return el.checkVisibility();
    return !!(el.offsetParent || el.getClientRects().length);
  };

  // Recurse into open shadow roots. Closed roots are unobservable by design.
  const collectAll = (root, selector, depth, state) => {
    const out = Array.from(root.querySelectorAll(selector));
    if (depth >= 6 || state.shadows >= 50) return out;
    for (const el of root.querySelectorAll('*')) {
      if (el.shadowRoot) {
        state.shadows++;
        if (state.shadows > 50) break;
        out.push(...collectAll(el.shadowRoot, selector, depth + 1, state));
        if (out.length >= state.budget) break;
      }
    }
    return out;
  };

  // Content links FIRST (main/article), then page chrome fills what's left of
  // the budget — on link-heavy sites (wikis, docs) the old whole-document
  // order spent all 120 slots on header/sidebar before content appeared.
  const contentRoot = document.querySelector('main,[role="main"],article,#content,#bodyContent') || document.body;
  const links = [];
  const buttons = [];
  const fields = [];
  const forms = [];
  const seenHref = new Set();
  const state = {
    linksBudget: 120,
    buttonsBudget: 40,
    fieldsBudget: 60,
    formsBudget: 10
  };

  const collectLinks = (scope, inContent, framePrefix) => {
    for (const a of collectAll(scope, 'a[href]', 0, { shadows: 0, budget: state.linksBudget })) {
      const text = clip(a.innerText || a.getAttribute('aria-label') || '', 100);
      if (!text || seenHref.has(a.href) || !visible(a)) continue;
      seenHref.add(a.href);
      const selector = framePrefix ? framePrefix + ' ||| ' + sel(a) : sel(a);
      links.push({ text, href: a.href, selector, in_content: inContent, in_shadow: a.getRootNode() instanceof ShadowRoot, in_frame: !!framePrefix });
      if (links.length >= state.linksBudget) return;
    }
  };
  const collectButtons = (scope, framePrefix) => {
    for (const b of collectAll(scope, 'button, [role="button"], input[type="submit"], input[type="button"]', 0, { shadows: 0, budget: state.buttonsBudget })) {
      const label = labelFor(b);
      if (!label || !visible(b)) continue;
      const selector = framePrefix ? framePrefix + ' ||| ' + sel(b) : sel(b);
      buttons.push({ label, type: b.type || null, selector, in_shadow: b.getRootNode() instanceof ShadowRoot, in_frame: !!framePrefix });
      if (buttons.length >= state.buttonsBudget) break;
    }
  };
  const collectFields = (scope, framePrefix) => {
    for (const f of collectAll(scope, 'input, select, textarea', 0, { shadows: 0, budget: state.fieldsBudget })) {
      if (['hidden', 'submit', 'button', 'image'].includes(f.type)) continue;
      if (!visible(f)) continue;
      const selector = framePrefix ? framePrefix + ' ||| ' + sel(f) : sel(f);
      const entry = {
        tag: f.tagName.toLowerCase(), type: f.type || null, name: f.name || null,
        id: f.id || null, label: labelFor(f), required: !!f.required, selector,
        in_shadow: f.getRootNode() instanceof ShadowRoot, in_frame: !!framePrefix
      };
      if (f.tagName === 'SELECT') entry.options = Array.from(f.options).slice(0, 30).map((o) => clip(o.text, 60));
      fields.push(entry);
      if (fields.length >= state.fieldsBudget) break;
    }
  };
  const collectForms = (scope, framePrefix) => {
    for (const fm of scope.querySelectorAll('form')) {
      const selector = framePrefix ? framePrefix + ' ||| ' + sel(fm) : sel(fm);
      forms.push({
        action: fm.getAttribute('action') || null, method: fm.method || null,
        selector, field_count: fm.elements.length, in_frame: !!framePrefix
      });
      if (forms.length >= state.formsBudget) break;
    }
  };
  const collectInventory = (doc, framePrefix) => {
    collectLinks(doc, false, framePrefix);
    collectButtons(doc, framePrefix);
    collectFields(doc, framePrefix);
    collectForms(doc, framePrefix);
  };

  // Main document first (content links get priority), then same-origin frames.
  collectLinks(contentRoot, true, null);
  if (links.length < 120) collectLinks(document, false, null);
  collectButtons(document, null);
  collectFields(document, null);
  collectForms(document, null);

  const framesUnreadable = [];
  const collectFrames = (doc, prefix, level) => {
    if (level >= 2) return;
    for (const iframe of doc.querySelectorAll('iframe')) {
      if (framesUnreadable.length >= 10) break;
      let frameDoc = null;
      try { frameDoc = iframe.contentDocument; } catch (e) {}
      const iframeSel = prefix ? prefix + ' ||| ' + sel(iframe) : sel(iframe);
      if (!frameDoc) {
        framesUnreadable.push({ selector: iframeSel, src: iframe.getAttribute('src') || iframe.src || null });
        continue;
      }
      collectInventory(frameDoc, iframeSel);
      collectFrames(frameDoc, iframeSel, level + 1);
    }
  };
  collectFrames(document, null, 0);

  const rawHtmlChars = document.documentElement ? document.documentElement.outerHTML.length : 0;

  // Shadow-root discovery for the text fallback below.
  let hasShadow = false;
  const scanShadow = (root, depth, state) => {
    if (depth >= 6 || state.count >= 50) return;
    for (const el of root.querySelectorAll('*')) {
      if (el.shadowRoot) {
        hasShadow = true;
        state.count++;
        if (state.count >= 50) return;
        scanShadow(el.shadowRoot, depth + 1, state);
      }
    }
  };
  scanShadow(document, 0, { count: 0 });

  // Main-content text: HIDE boilerplate (innerText skips display:none), read,
  // then RESTORE — non-destructive, because in-session digests must leave the
  // live DOM intact for subsequent click/type verbs.
  const kills = Array.from(document.querySelectorAll('script,style,noscript,svg,iframe,nav,header,footer,aside,[role="navigation"],[role="banner"],[role="contentinfo"],[aria-hidden="true"]'));
  const savedDisplay = kills.map((el) => el.style.display);
  for (const el of kills) el.style.display = 'none';
  let text = '';
  try {
    const root = document.querySelector('main,[role="main"],article') || document.body;
    text = root ? root.innerText.replace(/[ \t]+/g, ' ').replace(/\n{3,}/g, '\n\n').trim() : '';
  } finally {
    kills.forEach((el, i) => { el.style.display = savedDisplay[i]; });
  }

  // If the light-DOM content root has no visible text but the page hosts open
  // shadow roots, gather shadow text too (truncated later by the caller's
  // max-text-chars budget). innerText does not cross shadow boundaries, so
  // there is no double-collection.
  if (!text && hasShadow) {
    const pieces = [];
    const gatherText = (root, depth, state) => {
      if (depth >= 6 || state.count >= 50) return;
      const t = root.innerText.replace(/[ \t]+/g, ' ').replace(/\n{3,}/g, '\n\n').trim();
      if (t) pieces.push(t);
      for (const el of root.querySelectorAll('*')) {
        if (el.shadowRoot) {
          state.count++;
          if (state.count > 50) return;
          gatherText(el.shadowRoot, depth + 1, state);
        }
      }
    };
    gatherText(document.body, 0, { count: 0 });
    text = pieces.join('\n\n');
  }

  const canonical = document.querySelector('link[rel="canonical"]');
  const desc = document.querySelector('meta[name="description"]');
  return JSON.stringify({
    meta: {
      title: document.title,
      canonical: canonical ? canonical.href : null,
      description: desc ? clip(desc.content, 300) : null,
      lang: document.documentElement.lang || null
    },
    raw_html_chars: rawHtmlChars,
    text: text,
    structured: { json_ld: jsonld, tables: tables },
    interactive: { forms: forms, fields: fields, buttons: buttons, links: links },
    frames_unreadable: framesUnreadable
  });
})()
"##;

// endregion: Readable extraction

// region: Timestamps without a date crate
// ---------------------------------------------------------------------------
// Timestamps without a date crate
//
// Evidence needs an RFC3339 instant and nothing else, so the civil-date
// arithmetic is inlined rather than pulling in a dependency. Seconds
// precision, UTC, no parsing, no formatting options.
// ---------------------------------------------------------------------------

pub fn now_iso() -> String {
    // RFC3339 UTC without pulling in chrono: seconds precision is enough
    // for evidence timestamps (mine-boards uses toISOString()).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil date from days since epoch (Howard Hinnant's algorithm)
    let z = days as i64 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mth, d, h, m, s)
}

// endregion: Timestamps without a date crate

// region: Rendering one page
// ---------------------------------------------------------------------------
// Rendering one page
//
// Navigate, settle, extract — and the care here is all about honesty of
// evidence. The response listener is attached BEFORE the navigation so the
// HTTP status is observed rather than inferred, a soft block that renders is
// not treated as a failed navigation, and the page is closed on every path
// including the timeout.
// ---------------------------------------------------------------------------

/// Everything one rendered page yields, before output assembly.
pub struct Probe {
    pub final_url: String,
    pub title: String,
    /// (status, url) of the main-document response — observed via CDP
    /// Network events, never guessed. None = unobserved.
    pub http_status: Option<(i64, String)>,
    pub digest: serde_json::Value,
    pub text: String,
    pub raw_html_chars: u64,
    pub fetch_timestamp: String,
}

/// Render `url` in a fresh tab of the shared browser and extract. The page is
/// always closed, even when navigation/extraction times out.
pub async fn probe_page(browser: &Browser, url: &str, timeout_ms: u64) -> Result<Probe, String> {
    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|e| format!("new page: {}", e))?;

    let inner = navigate_and_extract(&page, url);
    let result = tokio::time::timeout(Duration::from_millis(timeout_ms), inner).await;

    // Close the page regardless — this also ends the per-page event stream so
    // the status listener below can finish.
    let (probe_core, status_task) = match result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            let _ = page.close().await;
            return Err(e);
        }
        Err(_) => {
            let _ = page.close().await;
            return Err(format!("timeout: no result within {} ms", timeout_ms));
        }
    };
    let _ = page.close().await;
    let http_status = tokio::time::timeout(Duration::from_millis(2_000), status_task)
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten();

    let (final_url, title, digest_raw, fetch_timestamp, nav_err) = probe_core;
    finish_probe(
        final_url,
        title,
        http_status,
        digest_raw,
        fetch_timestamp,
        nav_err,
    )
}

/// Digest of a page's CURRENT state without navigating — the in-session
/// content read. No main-document status is observable here.
pub async fn probe_current(page: &chromiumoxide::Page) -> Result<Probe, String> {
    let fetch_timestamp = now_iso();
    let final_url = page.url().await.ok().flatten().unwrap_or_default();
    let title = page.get_title().await.ok().flatten().unwrap_or_default();
    let digest_raw: Option<String> = page
        .evaluate(DIGEST_JS)
        .await
        .map_err(|e| format!("extraction: {}", e))?
        .into_value::<String>()
        .ok();
    finish_probe(final_url, title, None, digest_raw, fetch_timestamp, None)
}

fn finish_probe(
    final_url: String,
    title: String,
    http_status: Option<(i64, String)>,
    digest_raw: Option<String>,
    fetch_timestamp: String,
    nav_err: Option<String>,
) -> Result<Probe, String> {
    let mut digest: serde_json::Value = digest_raw
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::json!({}));

    let text = digest
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    if nav_err.is_some() && text.is_empty() && title.is_empty() {
        return Err(format!(
            "navigation failed: {}",
            nav_err.unwrap_or_default()
        ));
    }

    let raw_html_chars = digest
        .get("raw_html_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if let Some(obj) = digest.as_object_mut() {
        obj.remove("raw_html_chars"); // reported under economy instead
    }

    Ok(Probe {
        final_url,
        title,
        http_status,
        digest,
        text,
        raw_html_chars,
        fetch_timestamp,
    })
}

type ProbeCore = (String, String, Option<String>, String, Option<String>);

async fn navigate_and_extract(
    page: &chromiumoxide::Page,
    url: &str,
) -> Result<(ProbeCore, tokio::task::JoinHandle<Option<(i64, String)>>), String> {
    // Capture the main-document network response for the real HTTP status.
    let mut responses = page
        .event_listener::<EventResponseReceived>()
        .await
        .map_err(|e| format!("event listener: {}", e))?;
    let target_url = url.to_string();
    let status_task = tokio::task::spawn(async move {
        let mut first_doc: Option<(i64, String)> = None;
        while let Some(ev) = responses.next().await {
            if ev.r#type == ResourceType::Document {
                // First Document response = the navigation itself (or its redirect target).
                let s = ev.response.status;
                let u = ev.response.url.clone();
                if first_doc.is_none() || u == target_url {
                    first_doc = Some((s, u));
                }
            }
        }
        first_doc
    });

    let fetch_timestamp = now_iso();
    let nav = page.goto(url).await;
    // goto errs on hard navigation failures (DNS, refused); soft blocks (403/406
    // pages) still render — both are evidence, so don't bail on Err unless the
    // page never materializes.
    let nav_err = nav.err().map(|e| e.to_string());

    let _ = page.wait_for_navigation().await;
    settle_network_quiet(page, 500, 2_500).await;

    let final_url = page
        .url()
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| url.to_string());
    let title = page.get_title().await.ok().flatten().unwrap_or_default();

    // One deterministic in-page extraction pass (also used by verify for the
    // liveness signals; verify just omits the digest payload from its output).
    let digest_raw: Option<String> = page
        .evaluate(DIGEST_JS)
        .await
        .ok()
        .and_then(|v| v.into_value::<String>().ok());

    Ok((
        (final_url, title, digest_raw, fetch_timestamp, nav_err),
        status_task,
    ))
}

// endregion: Rendering one page

// region: Blocked pages, and the liveness verdict
// ---------------------------------------------------------------------------
// Blocked pages, and the liveness verdict
//
// Two small functions carrying the project's governing rule. A challenge page
// is detected and reported, never evaded and never dressed up as a failure.
// `verified` requires an OBSERVED 2xx/3xx — a page that rendered without one
// is `failed`, because upgrading it would be a guess wearing the word
// "verified".
// ---------------------------------------------------------------------------

/// Shared block-page phrasebook. HEURISTIC phrases are not yet live-certified;
/// they are logged honestly as `looks_blocked: true` rather than evaded.
pub fn looks_blocked(title: &str, text_lower: &str, final_url: &str) -> bool {
    let t = title.to_lowercase();
    t.contains("just a moment")
        // Two certified misses from 2026-09-05, both found the same afternoon
        // by pointing a fresh Chrome at search engines. DuckDuckGo's challenge
        // ("Unfortunately, bots use DuckDuckGo too. Please complete the
        // following challenge...") carried none of the phrases below and came
        // back HTTP 202 with `looks_blocked: false`. Startpage's block page
        // titled itself "Access Denied - Startpage" while its text said
        // "Access Temporarily Suspended"; the phrase was in the title, and
        // only the text was checked. Both were then handed to a tool as an
        // ordinary page with one link on it.
        || t.contains("access denied")
        || text_lower.contains("bots use duckduckgo too")
        || text_lower.contains("confirm this search was made by a human")
        || text_lower.contains("access temporarily suspended")
        || t.contains("pardon our interruption") // HEURISTIC: Akamai
        || t.contains("attention required! | cloudflare") // HEURISTIC: Cloudflare legacy
        || text_lower.contains("verify you are human")
        || text_lower.contains("access denied")
        || text_lower.contains("captcha")
        || text_lower.contains("unusual traffic")
        || text_lower.contains("request unsuccessful. incapsula") // HEURISTIC: Imperva/Incapsula
        || text_lower.contains("incapsula incident") // HEURISTIC: Imperva/Incapsula
        || text_lower.contains("datadome") // HEURISTIC: DataDome
        || text_lower.contains("px-captcha") // HEURISTIC: PerimeterX/HUMAN
        || text_lower.contains("checking your browser before accessing") // HEURISTIC: Cloudflare legacy
        || final_url.contains("google.com/sorry")
}

/// Liveness verdict per gate semantics: blocked stays blocked; verified only
/// on an OBSERVED 2xx/3xx main-document status — a rendered page with no
/// observed status is `failed`, never upgraded without evidence.
pub fn outcome(looks_blocked: bool, http_status: Option<i64>) -> &'static str {
    if looks_blocked {
        return "blocked";
    }
    match http_status {
        Some(s) if (200..400).contains(&s) => "verified",
        _ => "failed",
    }
}

// endregion: Blocked pages, and the liveness verdict

// region: The output contract
// ---------------------------------------------------------------------------
// The output contract
//
// The one function that builds the JSON object everything downstream reads,
// matching `docs/schema/chromehand-output.schema.json`. Text is the last thing
// truncated because every other section was already bounded in-page, and both
// copies of the page title are capped — a title is text a hostile page chose.
// ---------------------------------------------------------------------------

/// Assemble the output contract from a probe. `include_digest` = digest
/// command (verify omits the payload, keeps the liveness signals).
pub fn assemble(
    command: &str,
    url: &str,
    mut probe: Probe,
    include_digest: bool,
    window: TextWindow,
) -> serde_json::Value {
    let lower = probe.text.to_lowercase();
    let contains_apply = lower.contains("apply");
    // Block heuristics. Certified misses fixed here: Google's /sorry
    // interstitial ("unusual traffic") returned HTTP 200 and slipped past the
    // P1 phrases — a false `verified` on a block page (caught live 2026-07-19).
    let looks_blocked = looks_blocked(&probe.title, &lower, &probe.final_url);

    // Cap hostile-page-controlled title length (page_title and digest meta).
    let page_title = probe.title.chars().take(300).collect::<String>();
    if let Some(meta) = probe.digest.get_mut("meta") {
        if let Some(t) = meta.get_mut("title") {
            if let Some(s) = t.as_str() {
                *t = serde_json::json!(s.chars().take(300).collect::<String>());
            }
        }
    }

    // Context-window budget: text is the biggest section, cut last-priority
    // (metadata/interactive/structured are already bounded in-page). What is
    // returned is a *window* into the text and not always its head, so the
    // three numbers below are what a caller needs to ask for the next one.
    let text_chars = probe.text.chars().count();
    let shown: String = probe
        .text
        .chars()
        .skip(window.offset)
        .take(window.max_chars)
        .collect();
    let end = window.offset.saturating_add(shown.chars().count());
    probe.digest["text"] = serde_json::json!(shown);
    probe.digest["text_offset"] = serde_json::json!(window.offset);
    probe.digest["text_chars_total"] = serde_json::json!(text_chars);
    // "There is more after this window", which is the question a reader has.
    // Not "the page is longer than one window": at the last window those two
    // disagree, and answering the wrong one costs a page load for nothing.
    probe.digest["text_truncated"] = serde_json::json!(end < text_chars);

    let (status, status_url) = match &probe.http_status {
        Some((s, u)) => (serde_json::json!(s), Some(u.clone())),
        None => (serde_json::Value::Null, None),
    };
    let status_num = probe.http_status.as_ref().map(|(s, _)| *s);

    let mut out = serde_json::json!({
        "command": command,
        "url": url,
        "final_url": probe.final_url,
        "http_status": status,
        "page_title": page_title,
        "contains_apply_button": contains_apply,
        "looks_blocked": looks_blocked,
        "outcome": outcome(looks_blocked, status_num),
        "evidence": {
            "source": "browser-render",
            "engine": format!("browser-miner/{} chromiumoxide", env!("CARGO_PKG_VERSION")),
            "api_endpoint": status_url.unwrap_or_else(|| url.to_string()),
            "http_status": status_num,
            "fetch_timestamp": probe.fetch_timestamp
        }
    });

    if include_digest {
        let digest_chars = serde_json::to_string(&probe.digest)
            .map(|s| s.chars().count())
            .unwrap_or(0);
        out["digest"] = probe.digest;
        out["economy"] = serde_json::json!({
            "raw_html_chars": probe.raw_html_chars,
            "digest_chars": digest_chars
        });
    }
    out
}

// endregion: The output contract

// region: Session deltas
// ---------------------------------------------------------------------------
// Session deltas
//
// What changed on the page since the last look, so a long session does not
// re-send the whole digest every turn. Elements are keyed by their stable
// selector, which is what makes a click that reveals content show up as an
// addition rather than as everything having moved. Each list is capped with an
// honest `*_truncated` flag beside it.
// ---------------------------------------------------------------------------

/// Per-category item budget for `digest --delta`.
const DELTA_BUDGET: usize = 60;

/// Compute the session delta for `digest --session S --delta`.
///
/// `prior` is the snapshot from the previous `--delta` call (or `None` on the
/// first call). `current` is the full in-session digest produced by
/// `actions::session_digest`. Returns `(snapshot, output)` where `snapshot`
/// should be written to `.browser-miner/session-<id>.digest.json` and `output`
/// is emitted to stdout.
///
/// Deterministic: elements are keyed by their stable `selector`; diff fields
/// are limited to the agent-meaningful subset (text/href for links,
/// label/type for buttons, label for fields). Each list is capped at 60 items
/// with an honesty `*_truncated` flag.
pub fn compute_delta(
    session_id: &str,
    prior: Option<&serde_json::Value>,
    current: &serde_json::Value,
) -> (serde_json::Value, serde_json::Value) {
    use std::collections::hash_map::DefaultHasher;
    use std::collections::BTreeMap;
    use std::hash::{Hash, Hasher};

    let digest = &current["digest"];
    let text = digest["text"].as_str().unwrap_or("");
    let text_chars_total = digest["text_chars_total"].as_u64().unwrap_or(0);

    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let text_hash = hasher.finish();

    let interactive = &digest["interactive"];
    let links = interactive["links"].as_array().cloned().unwrap_or_default();
    let buttons = interactive["buttons"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let fields = interactive["fields"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let snapshot = serde_json::json!({
        "text_hash": text_hash,
        "links": links,
        "buttons": buttons,
        "fields": fields,
    });

    // First `--delta` call: store baseline, return the full digest annotated.
    let Some(prior) = prior else {
        let mut out = current.clone();
        out["delta"] = serde_json::json!({
            "baseline": true,
            "note": "no prior snapshot — full digest returned; subsequent --delta calls diff against this"
        });
        return (snapshot, out);
    };

    fn array_cloned(v: &serde_json::Value) -> Vec<serde_json::Value> {
        v.as_array().cloned().unwrap_or_default()
    }

    fn by_selector(arr: &[serde_json::Value]) -> BTreeMap<String, serde_json::Value> {
        let mut map = BTreeMap::new();
        for v in arr {
            if let Some(sel) = v["selector"].as_str() {
                map.insert(sel.to_string(), v.clone());
            }
        }
        map
    }

    fn truncate_list(arr: &mut Vec<serde_json::Value>, budget: usize) -> bool {
        if arr.len() > budget {
            arr.truncate(budget);
            true
        } else {
            false
        }
    }

    fn diff_category(
        prior: BTreeMap<String, serde_json::Value>,
        cur: BTreeMap<String, serde_json::Value>,
        kind: &str,
        relevant: &[&str],
        added: &mut Vec<serde_json::Value>,
        removed: &mut Vec<serde_json::Value>,
        changed: &mut Vec<serde_json::Value>,
    ) {
        for (sel, v) in &cur {
            if !prior.contains_key(sel) {
                added.push(v.clone());
            }
        }
        for (sel, v) in &prior {
            match cur.get(sel) {
                None => removed.push(serde_json::json!({ "selector": sel, "kind": kind })),
                Some(cur_v) => {
                    let mut was = serde_json::Map::new();
                    for key in relevant {
                        if v.get(key) != cur_v.get(key) {
                            was.insert(
                                key.to_string(),
                                v.get(key).cloned().unwrap_or(serde_json::Value::Null),
                            );
                        }
                    }
                    if !was.is_empty() {
                        let mut cur_clone = cur_v.clone();
                        cur_clone["was"] = serde_json::Value::Object(was);
                        changed.push(cur_clone);
                    }
                }
            }
        }
    }

    let prior_links = by_selector(&array_cloned(&prior["links"]));
    let prior_buttons = by_selector(&array_cloned(&prior["buttons"]));
    let prior_fields = by_selector(&array_cloned(&prior["fields"]));
    let cur_links = by_selector(&links);
    let cur_buttons = by_selector(&buttons);
    let cur_fields = by_selector(&fields);

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    diff_category(
        prior_links,
        cur_links,
        "link",
        &["text", "href"],
        &mut added,
        &mut removed,
        &mut changed,
    );
    diff_category(
        prior_buttons,
        cur_buttons,
        "button",
        &["label", "type"],
        &mut added,
        &mut removed,
        &mut changed,
    );
    diff_category(
        prior_fields,
        cur_fields,
        "field",
        &["label"],
        &mut added,
        &mut removed,
        &mut changed,
    );

    let added_truncated = truncate_list(&mut added, DELTA_BUDGET);
    let removed_truncated = truncate_list(&mut removed, DELTA_BUDGET);
    let changed_truncated = truncate_list(&mut changed, DELTA_BUDGET);

    let prior_text_hash = prior["text_hash"].as_u64();
    let text_changed = prior_text_hash != Some(text_hash);

    let mut delta = serde_json::json!({
        "baseline": false,
        "added": added,
        "added_truncated": added_truncated,
        "removed": removed,
        "removed_truncated": removed_truncated,
        "changed": changed,
        "changed_truncated": changed_truncated,
        "text_changed": text_changed,
        "text_chars_total": text_chars_total,
    });
    if text_changed {
        delta["text"] = serde_json::json!(text);
    }

    let out = serde_json::json!({
        "command": "digest",
        "session": session_id,
        "delta": delta,
        "final_url": current["final_url"].clone(),
        "page_title": current["page_title"].clone(),
        "evidence": current.get("evidence").cloned().unwrap_or(serde_json::json!({
            "source": "browser-render",
            "fetch_timestamp": now_iso()
        })),
    });

    (snapshot, out)
}

// endregion: Session deltas

// region: Waiting for the network to go quiet
// ---------------------------------------------------------------------------
// Waiting for the network to go quiet
//
// Timestamp-of-last-event rather than a count of requests in flight, and the
// distinction is the whole function: counters drift permanently the first time
// an event pair is missed, whereas a timestamp cannot.
// ---------------------------------------------------------------------------

/// Wait until the network has been quiet for `quiet_ms`, or until `cap_ms`
/// has elapsed. Robust implementation: track the timestamp of the LAST network
/// event seen (RequestWillBeSent / LoadingFinished). Never counts in-flight
/// requests — counters drift when event pairs are missed. Listeners are aborted
/// and dropped before returning.
pub async fn settle_network_quiet(page: &chromiumoxide::Page, quiet_ms: u64, cap_ms: u64) -> bool {
    // Network domain must be enabled for request/loading events to flow.
    let _ = page
        .execute(chromiumoxide::cdp::browser_protocol::network::EnableParams::default())
        .await;

    let start = std::time::Instant::now();
    let last_event = Arc::new(AtomicU64::new(0));
    let mut tasks: Vec<JoinHandle<()>> = Vec::with_capacity(2);

    if let Ok(stream) = page.event_listener::<EventRequestWillBeSent>().await {
        let last = last_event.clone();
        tasks.push(tokio::task::spawn(async move {
            let mut stream = stream;
            while stream.next().await.is_some() {
                last.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
            }
        }));
    }
    if let Ok(stream) = page.event_listener::<EventLoadingFinished>().await {
        let last = last_event.clone();
        tasks.push(tokio::task::spawn(async move {
            let mut stream = stream;
            while stream.next().await.is_some() {
                last.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
            }
        }));
    }

    const FLOOR_MS: u64 = 300;
    let mut quiet_met = false;
    loop {
        let elapsed = start.elapsed().as_millis() as u64;
        if elapsed >= cap_ms {
            break;
        }
        let since_last = elapsed.saturating_sub(last_event.load(Ordering::Relaxed));
        if since_last >= quiet_ms && elapsed >= FLOOR_MS {
            quiet_met = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    for t in tasks {
        t.abort();
    }
    quiet_met
}

// endregion: Waiting for the network to go quiet

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The block phrasebook, entry by entry, and one structural assertion about the
// extraction script itself: the display-restore must sit in a `finally`. That
// last one is checked by reading the source string because the alternative is
// a live browser, and the property — an in-session digest leaves the DOM as it
// found it — is what every later click depends on.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{assemble, looks_blocked, Probe, TextWindow, DIGEST_JS};

    fn probe(text: &str) -> Probe {
        Probe {
            final_url: "https://example.com/".into(),
            title: "Example".into(),
            http_status: Some((200, "https://example.com/".into())),
            digest: serde_json::json!({ "text": text }),
            text: text.into(),
            raw_html_chars: 0,
            fetch_timestamp: "2026-08-26T00:00:00Z".into(),
        }
    }

    /// The full page text is in hand here — `probe.text` arrives uncapped from
    /// the in-page walk — so a window is a slice, not a second read of
    /// anything. What costs a second page load is the *next* window, because
    /// nothing holds this text between tool calls.
    #[test]
    fn a_text_window_returns_the_slice_the_caller_asked_for() {
        let text: String = (0..100)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let out = assemble(
            "digest",
            "https://example.com/",
            probe(&text),
            true,
            TextWindow {
                offset: 40,
                max_chars: 20,
            },
        );
        let d = &out["digest"];
        assert_eq!(d["text"].as_str().unwrap(), &text[40..60]);
        assert_eq!(d["text_offset"].as_u64(), Some(40));
        assert_eq!(d["text_chars_total"].as_u64(), Some(100));
        // 60 < 100: there is more after this window.
        assert_eq!(d["text_truncated"].as_bool(), Some(true));
    }

    /// The last window ends exactly at the end and is not "truncated" — and a
    /// window starting past the end is empty without lying about the total.
    #[test]
    fn the_final_window_is_whole_and_a_window_past_the_end_is_empty() {
        let text = "abcdefghij".to_string();
        let whole = assemble(
            "digest",
            "https://example.com/",
            probe(&text),
            true,
            TextWindow {
                offset: 0,
                max_chars: 10,
            },
        );
        assert_eq!(whole["digest"]["text_truncated"].as_bool(), Some(false));

        let past = assemble(
            "digest",
            "https://example.com/",
            probe(&text),
            true,
            TextWindow {
                offset: 900,
                max_chars: 10,
            },
        );
        assert_eq!(past["digest"]["text_offset"].as_u64(), Some(900));
        assert_eq!(past["digest"]["text"].as_str().unwrap(), "");
        assert_eq!(past["digest"]["text_truncated"].as_bool(), Some(false));
    }

    #[test]
    fn detects_pardon_our_interruption() {
        assert!(looks_blocked("Pardon Our Interruption", "", ""));
    }

    #[test]
    fn detects_checking_your_browser() {
        assert!(looks_blocked(
            "",
            "checking your browser before accessing example.com",
            ""
        ));
    }

    #[test]
    fn plain_text_not_blocked() {
        assert!(!looks_blocked(
            "Example Domain",
            "hello world",
            "https://example.com/"
        ));
    }

    #[test]
    fn digest_js_restores_display_in_finally() {
        // Non-destructive in-session extraction: even if innerText throws,
        // the boilerplate display values must be restored. The guard is a
        // try/finally around the read/restore block.
        assert!(
            DIGEST_JS.contains("try {"),
            "DIGEST_JS must wrap the innerText read in try"
        );
        assert!(
            DIGEST_JS.contains("} finally {"),
            "DIGEST_JS must use finally for display restore"
        );
    }

    #[test]
    fn detects_existing_just_a_moment() {
        assert!(looks_blocked("Just a moment...", "", ""));
    }

    #[test]
    fn detects_google_sorry_url() {
        assert!(looks_blocked("", "", "https://www.google.com/sorry/index"));
    }
}

// endregion: Tests
