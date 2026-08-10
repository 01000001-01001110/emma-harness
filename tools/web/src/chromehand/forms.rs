//! P3 `extract-form` + P4 `fill` / `submit`.
//!
//! extract-form: the digest's field inventory, deepened — real label
//! resolution (label[for] → wrapping label → aria-label/labelledby →
//! placeholder), fieldset/legend grouping, radio/checkbox groups collapsed to
//! one logical field with options, ARIA custom widgets (combobox/listbox/
//! radiogroup), wizard-step signals (step indicators, disabled next/prev,
//! hidden panels). Deterministic DOM walk — nothing inferred by a model.
//!
//! fill: agent-provided values keyed by STABLE SELECTOR (from extract-form/
//! digest). Types like a user (focus + key events so JS validation fires),
//! sets selects/checks/radios with real input+change events, uploads files
//! via DOM.setFileInputFiles (only paths the caller passed). Re-reads every
//! targeted field from the live DOM and reports {requested, now_contains,
//! ok}. Contains NO submit path; password fields refused structurally.
//!
//! submit: its own verb behind the TWO-KEY RULE — (1) the calling agent
//! passes --yes-actually-submit AND (2) the user set "allow_auto_submit":
//! true (optionally scoped by allow_auto_submit_domains) in
//! data/browser-miner-config.json, a file no agent may edit on the user's
//! behalf. Either key missing → exit 2 naming the missing key. Without the
//! flag, submit is OBSERVE mode: requires a --headful session, focuses the
//! window, and waits for the HUMAN to click — the binary observes the
//! outcome and records evidence. Every auto-submit is logged to
//! data/browser-miner-submit-log.jsonl.

use std::time::Duration;

use crate::chromehand::actions::{
    bounded_nav_wait, evidence, mini_digest, selector_needs_in_page, RESOLVE_JS,
};
use crate::chromehand::digest::now_iso;
use crate::chromehand::policy::Policy;
use crate::chromehand::session::Connected;

// region: The user's own files
// ---------------------------------------------------------------------------
// The user's own files
//
// Two paths, both relative to the process's working directory, and both
// meaningful only for the CLI: the config holding the second key, and the log
// every auto-submit is appended to. The config's whole point is that it
// belongs to the user, so no agent may write it on their behalf.
// ---------------------------------------------------------------------------

pub const USER_CONFIG_PATH: &str = "data/browser-miner-config.json";
pub const SUBMIT_LOG_PATH: &str = "data/browser-miner-submit-log.jsonl";

// endregion: The user's own files

// region: extract-form
// ---------------------------------------------------------------------------
// extract-form
//
// The digest's field inventory, deepened: real label resolution, fieldset
// grouping, radio and checkbox groups collapsed to one logical field, ARIA
// widgets, and wizard-step signals. A deterministic DOM walk — nothing here is
// inferred by a model.
// ---------------------------------------------------------------------------

const EXTRACT_FORM_JS: &str = r##"
(() => {
  const clip = (s, n) => { s = (s || '').replace(/\s+/g, ' ').trim(); return s.length > n ? s.slice(0, n) : s; };
  const localSel = (el, root) => {
    if (el.id) return '#' + CSS.escape(el.id);
    if (el.name && el.tagName !== 'A') {
      const q = el.tagName.toLowerCase() + '[name=' + JSON.stringify(el.name) + ']';
      try { if (root.querySelectorAll(q).length === 1) return q; } catch (e) {}
    }
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
  const prefixedSel = (el, framePrefix) => framePrefix ? framePrefix + ' ||| ' + sel(el) : sel(el);
  const visible = (el) => {
    if (el.checkVisibility) return el.checkVisibility();
    return !!(el.offsetParent || el.getClientRects().length);
  };
  const labelOf = (el, root) => {
    if (el.labels && el.labels.length) return clip(el.labels[0].innerText, 160);
    const wrap = el.closest('label');
    if (wrap) return clip(wrap.innerText, 160);
    const lb = el.getAttribute('aria-labelledby');
    if (lb) {
      const t = lb.split(/\s+/).map((id) => { const r = root.getElementById(id); return r ? r.innerText : ''; }).join(' ');
      if (t.trim()) return clip(t, 160);
    }
    return clip(el.getAttribute('aria-label') || el.placeholder || el.title || '', 160);
  };
  const groupOf = (el) => {
    const fs = el.closest('fieldset');
    if (fs) {
      const lg = fs.querySelector('legend');
      if (lg) return clip(lg.innerText, 120);
    }
    const rg = el.closest('[role="group"],[role="radiogroup"]');
    if (rg) return clip(rg.getAttribute('aria-label') || (rg.querySelector('legend,h1,h2,h3,h4') || {}).innerText || '', 120) || null;
    return null;
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

  const fields = [];
  const widgets = [];
  const forms = [];

  const collectFields = (root, framePrefix, doneRadioGroups) => {
    const isFrame = !!framePrefix;
    for (const f of collectAll(root, 'input, select, textarea', 0, { shadows: 0, budget: 120 })) {
      if (['hidden', 'submit', 'button', 'image'].includes(f.type)) continue;
      const fRoot = f.getRootNode();
      const isShadow = fRoot instanceof ShadowRoot;
      const isVis = visible(f);
      // Radio/checkbox groups collapse to ONE logical field with options.
      // Scope membership to the same root so same-named groups in different
      // shadow trees or different frames do not collide.
      if ((f.type === 'radio' || f.type === 'checkbox') && f.name) {
        const key = (isFrame ? 'frame:' : '') + (isShadow ? 'shadow:' : '') + f.type + ':' + f.name;
        if (doneRadioGroups.has(key)) continue;
        doneRadioGroups.add(key);
        const members = Array.from(fRoot.querySelectorAll('input[type=' + JSON.stringify(f.type) + '][name=' + JSON.stringify(f.name) + ']'));
        fields.push({
          kind: f.type === 'radio' ? 'radio_group' : 'checkbox_group',
          name: f.name,
          label: groupOf(f) || labelOf(f, fRoot),
          group: groupOf(f),
          required: members.some((m) => m.required),
          visible: members.some(visible),
          options: members.slice(0, 30).map((m) => ({ value: m.value, label: labelOf(m, fRoot), selector: prefixedSel(m, framePrefix), checked: m.checked })),
          selector: prefixedSel(members[0], framePrefix),
          in_shadow: isShadow,
          in_frame: isFrame
        });
        continue;
      }
      const entry = {
        kind: f.tagName === 'SELECT' ? 'select' : (f.tagName === 'TEXTAREA' ? 'textarea' : (f.type || 'text')),
        name: f.name || null, id: f.id || null,
        label: labelOf(f, fRoot), group: groupOf(f),
        required: !!f.required, visible: isVis,
        autocomplete: f.getAttribute('autocomplete') || null,
        selector: prefixedSel(f, framePrefix),
        in_shadow: isShadow,
        in_frame: isFrame
      };
      if (f.tagName === 'SELECT') {
        entry.value = f.value || null;
      } else if (f.type === 'file') {
        entry.value = (f.files && f.files.length) ? f.files[0].name : null;
      } else if (f.type === 'checkbox' || f.type === 'radio') {
        entry.checked = f.checked;
      } else {
        entry.value = f.value || null;
      }
      if (f.tagName === 'SELECT') {
        entry.options = Array.from(f.options).slice(0, 50).map((o) => ({ value: o.value, label: clip(o.text, 80) }));
        entry.multiple = !!f.multiple;
      }
      if (f.type === 'file') { entry.accept = f.getAttribute('accept') || null; entry.multiple = !!f.multiple; }
      if (f.type === 'password') entry.fill_refused = 'password fields are structurally unreachable';
      fields.push(entry);
      if (fields.length >= 120) break;
    }
  };

  const collectWidgets = (root, framePrefix) => {
    const isFrame = !!framePrefix;
    for (const w of collectAll(root, '[role="combobox"],[role="listbox"],[role="slider"],[role="switch"],[role="spinbutton"]', 0, { shadows: 0, budget: 20 })) {
      if (w.tagName === 'INPUT' || w.tagName === 'SELECT') continue;
      if (!visible(w)) continue;
      const wRoot = w.getRootNode();
      widgets.push({ role: w.getAttribute('role'), label: labelOf(w, wRoot), selector: prefixedSel(w, framePrefix), expanded: w.getAttribute('aria-expanded'), in_shadow: wRoot instanceof ShadowRoot, in_frame: isFrame });
      if (widgets.length >= 20) break;
    }
  };

  const collectForms = (root, framePrefix) => {
    const isFrame = !!framePrefix;
    for (const fm of root.querySelectorAll('form')) {
      forms.push({
        action: fm.getAttribute('action') || null, method: fm.method || null,
        selector: prefixedSel(fm, framePrefix), field_count: fm.elements.length,
        submit_controls: Array.from(fm.querySelectorAll('button[type="submit"],input[type="submit"],button:not([type])')).slice(0, 5)
          .map((b) => ({ label: clip(b.innerText || b.value || '', 60), selector: prefixedSel(b, framePrefix) })),
        in_frame: isFrame
      });
      if (forms.length >= 10) break;
    }
  };

  const collectInventory = (doc, framePrefix) => {
    const doneRadioGroups = new Set();
    collectFields(doc, framePrefix, doneRadioGroups);
    collectWidgets(doc, framePrefix);
    collectForms(doc, framePrefix);
  };

  // Main document first, then same-origin frames (max 2 levels).
  collectInventory(document, null);

  const collectFrames = (doc, prefix, level) => {
    if (level >= 2) return;
    for (const iframe of doc.querySelectorAll('iframe')) {
      let frameDoc = null;
      try { frameDoc = iframe.contentDocument; } catch (e) {}
      if (!frameDoc) continue;
      const iframeSel = prefix ? prefix + ' ||| ' + sel(iframe) : sel(iframe);
      collectInventory(frameDoc, iframeSel);
      collectFrames(frameDoc, iframeSel, level + 1);
    }
  };
  collectFrames(document, null, 0);

  // Wizard signals: step indicators + next/prev/continue controls (main document only).
  const wizard = { likely: false, indicators: [], nav_buttons: [] };
  for (const s of document.querySelectorAll('[class*="step" i],[aria-label*="step" i],[data-step]')) {
    const t = clip(s.innerText, 60);
    if (t && /step\s*\d|^\d+\s*(of|\/)\s*\d+$/i.test(t)) { wizard.indicators.push(t); if (wizard.indicators.length >= 5) break; }
  }
  for (const b of document.querySelectorAll('button, [role="button"], input[type="button"]')) {
    const t = (b.innerText || b.value || '').trim();
    if (/^(next|continue|back|previous|prev)\b/i.test(t) && visible(b)) {
      wizard.nav_buttons.push({ label: clip(t, 40), selector: sel(b), disabled: !!b.disabled });
      if (wizard.nav_buttons.length >= 6) break;
    }
  }
  wizard.likely = wizard.indicators.length > 0 || wizard.nav_buttons.length > 0;

  return JSON.stringify({ forms, fields, widgets, wizard });
})()
"##;

pub async fn extract_form(page: &chromiumoxide::Page) -> Result<serde_json::Value, String> {
    let raw: String = page
        .evaluate(EXTRACT_FORM_JS)
        .await
        .map_err(|e| format!("extract-form script: {}", e))?
        .into_value::<String>()
        .map_err(|e| format!("extract-form result: {}", e))?;
    serde_json::from_str(&raw).map_err(|e| format!("extract-form parse: {}", e))
}

// endregion: extract-form

// region: fill
// ---------------------------------------------------------------------------
// fill
//
// Values in, keyed by the stable selectors extract-form handed out. The rule
// that shapes every branch below: fill NEVER submits. It types like a user so
// client-side validation fires, then re-reads each field from the live DOM and
// reports what is actually there rather than what was sent. Password fields
// and newline-into-single-line values are refused per field, and the rest of
// the form still fills — a refusal is recorded, not fatal.
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct FillSpec {
    pub fields: Vec<FillField>,
}

#[derive(serde::Deserialize)]
pub struct FillField {
    pub selector: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub checked: Option<bool>,
    #[serde(default)]
    pub file: Option<String>,
}

pub fn read_fill_spec(path: &str) -> Result<FillSpec, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path, e))?;
    serde_json::from_str(&raw).map_err(|e| {
        format!(
            "{}: invalid values JSON ({}) — expected {{\"fields\": [{{\"selector\": \"…\", \"value\"|\"checked\"|\"file\": …}}]}}",
            path, e
        )
    })
}

/// One field's precheck: what is at the selector, is it fillable.
async fn precheck(page: &chromiumoxide::Page, selector: &str) -> serde_json::Value {
    let js = format!(
        r#"(() => {{ {resolver}
            const meta = __bmResolveMeta({sel});
            if (!meta.found) return JSON.stringify({{ found: false }});
            return JSON.stringify({{
                found: true,
                tag: meta.tag,
                type: meta.type,
                editable: meta.editable,
                disabled: meta.disabled
            }});
        }})()"#,
        resolver = RESOLVE_JS,
        sel = serde_json::to_string(selector).unwrap()
    );
    page.evaluate(js.as_str())
        .await
        .ok()
        .and_then(|v| v.into_value::<String>().ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({ "found": false }))
}

/// Single-line inputs (tag `input`, not contenteditable) dispatch an implicit
/// form submission on Enter; newlines in their values are therefore refused.
/// NOTE: match on the TAG only — a real `<textarea>` has tag "textarea" and is
/// already excluded. Do NOT also exempt on `type != "textarea"`: an
/// `<input type="textarea">` is an UNKNOWN type that browsers fall back to
/// `text`, so Enter still submits — exempting it reopens the exact bypass
/// (Opus review caught this).
fn is_single_line_input(tag: &str, _ftype: &str, editable: bool) -> bool {
    tag == "input" && !editable
}

/// Live re-read of a field's current state.
async fn read_back(page: &chromiumoxide::Page, selector: &str) -> String {
    let js = format!(
        r#"(() => {{ {resolver}
            const el = __bmResolveEl({sel});
            if (!el) return '';
            if (el.type === 'checkbox' || el.type === 'radio') return el.checked ? 'checked' : 'unchecked';
            if (el.type === 'file') return Array.from(el.files || []).map((f) => f.name).join(',');
            if (el.isContentEditable) return el.innerText;
            if (el.tagName === 'SELECT') {{
                const o = el.selectedOptions && el.selectedOptions[0];
                return o ? (o.text.trim() + ' [' + el.value + ']') : String(el.value ?? '');
            }}
            return String(el.value ?? '');
        }})()"#,
        resolver = RESOLVE_JS,
        sel = serde_json::to_string(selector).unwrap()
    );
    page.evaluate(js.as_str())
        .await
        .ok()
        .and_then(|v| v.into_value::<String>().ok())
        .unwrap_or_default()
}

async fn set_file_input(
    page: &chromiumoxide::Page,
    selector: &str,
    file: &str,
) -> Result<(), String> {
    use chromiumoxide::cdp::browser_protocol::dom::{
        GetDocumentParams, QuerySelectorParams, SetFileInputFilesParams,
    };
    let path = std::path::Path::new(file);
    if !path.exists() {
        return Err(format!("file not found: {}", file));
    }
    let doc = page
        .execute(GetDocumentParams::default())
        .await
        .map_err(|e| e.to_string())?;
    let node = page
        .execute(QuerySelectorParams::new(doc.result.root.node_id, selector))
        .await
        .map_err(|e| e.to_string())?;
    if node.result.node_id.inner() == &0 {
        return Err("selector matched no element".to_string());
    }
    page.execute(
        SetFileInputFilesParams::builder()
            .files(vec![file.to_string()])
            .node_id(node.result.node_id)
            .build()
            .map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| format!("setFileInputFiles: {}", e))?;
    Ok(())
}

async fn set_shadow_file_input(
    page: &chromiumoxide::Page,
    selector: &str,
    file: &str,
) -> Result<(), String> {
    let path = std::path::Path::new(file);
    if !path.exists() {
        return Err(format!("file not found: {}", file));
    }
    let bytes = std::fs::read(file).map_err(|e| format!("read file: {}", e))?;
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let js = format!(
        r#"(() => {{ {resolver}
            const el = __bmResolveEl({sel});
            if (!el) return 'selector matched no element';
            if (el.type !== 'file') return 'not a file input';
            const dt = new DataTransfer();
            dt.items.add(new File([new Uint8Array({bytes})], {name}));
            el.files = dt.files;
            el.dispatchEvent(new Event('input', {{bubbles:true}}));
            el.dispatchEvent(new Event('change', {{bubbles:true}}));
            return '';
        }})()"#,
        resolver = RESOLVE_JS,
        sel = serde_json::to_string(selector).unwrap(),
        bytes = serde_json::to_string(&bytes).unwrap(),
        name = serde_json::to_string(name).unwrap()
    );
    let err: String = page
        .evaluate(js.as_str())
        .await
        .map_err(|e| format!("set shadow file input: {}", e))?
        .into_value::<String>()
        .unwrap_or_default();
    if err.is_empty() {
        Ok(())
    } else {
        Err(err)
    }
}

/// `fill --session S --values f.json [--screenshot out.png]`
pub async fn fill(
    c: &Connected,
    pol: &Policy,
    spec: &FillSpec,
    screenshot_out: Option<&str>,
) -> Result<serde_json::Value, String> {
    // Interaction gate: allowlist required, current URL must pass.
    let url = c.page.url().await.ok().flatten().unwrap_or_default();
    pol.check_interaction(&url)?;

    let mut results = Vec::with_capacity(spec.fields.len());
    for f in &spec.fields {
        let pre = precheck(&c.page, &f.selector).await;
        let found = pre.get("found").and_then(|v| v.as_bool()).unwrap_or(false);
        if !found {
            let err = pre
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("selector matched no element");
            results.push(serde_json::json!({
                "selector": f.selector, "ok": false, "error": err
            }));
            continue;
        }
        let ftype = pre.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if ftype == "password" {
            // Structural refusal — recorded per-field, the rest still fills.
            results.push(serde_json::json!({
                "selector": f.selector, "ok": false,
                "refused": "password fields are structurally unreachable — the binary never handles credentials"
            }));
            continue;
        }
        if pre
            .get("disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            results.push(serde_json::json!({
                "selector": f.selector, "ok": false, "error": "field is disabled"
            }));
            continue;
        }

        let tag = pre.get("tag").and_then(|v| v.as_str()).unwrap_or("");
        let ftype = pre.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let editable = pre
            .get("editable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if let Some(value) = &f.value {
            if is_single_line_input(tag, ftype, editable)
                && (value.contains('\n') || value.contains('\r'))
            {
                results.push(serde_json::json!({
                    "selector": f.selector,
                    "ok": false,
                    "refused": "newline in value would trigger form submission; multi-line values are only allowed for textarea"
                }));
                continue;
            }
        }

        // Outcome carries an optional select match (value, text) so the ok
        // check can compare exactly against either without substring matching.
        let mut select_match: Option<(String, String)> = None;
        let outcome: Result<(), String> = if let Some(file) = &f.file {
            if ftype != "file" {
                Err("'file' given but element is not input[type=file]".to_string())
            } else if selector_needs_in_page(&f.selector) {
                set_shadow_file_input(&c.page, &f.selector, file).await
            } else {
                set_file_input(&c.page, &f.selector, file).await
            }
        } else if let Some(checked) = f.checked {
            let js = format!(
                r#"(() => {{ {resolver}
                    const el = __bmResolveEl({sel});
                    if (!el) return 'selector matched no element';
                    if (el.type !== 'checkbox' && el.type !== 'radio') return 'not a checkbox/radio';
                    if (el.checked !== {want}) el.click();
                    if (el.checked !== {want}) {{ el.checked = {want}; el.dispatchEvent(new Event('input', {{bubbles:true}})); el.dispatchEvent(new Event('change', {{bubbles:true}})); }}
                    return '';
                }})()"#,
                resolver = RESOLVE_JS,
                sel = serde_json::to_string(&f.selector).unwrap(),
                want = checked
            );
            match c.page.evaluate(js.as_str()).await {
                Ok(v) => {
                    let e = v.into_value::<String>().unwrap_or_default();
                    if e.is_empty() {
                        Ok(())
                    } else {
                        Err(e)
                    }
                }
                Err(e) => Err(e.to_string()),
            }
        } else if let Some(value) = &f.value {
            let tag = pre.get("tag").and_then(|v| v.as_str()).unwrap_or("");
            let editable = pre
                .get("editable")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if tag == "select" {
                let js = format!(
                    r#"(() => {{ {resolver}
                        const el = __bmResolveEl({sel});
                        if (!el) return JSON.stringify({{ ok: false, error: 'selector matched no element' }});
                        let m = null;
                        for (const o of el.options) if (o.value === {val} || o.text.trim() === {val}) {{ m = o; break; }}
                        if (!m) return JSON.stringify({{ ok: false, error: 'no option matching value or visible text' }});
                        el.value = m.value;
                        el.dispatchEvent(new Event('input', {{bubbles:true}}));
                        el.dispatchEvent(new Event('change', {{bubbles:true}}));
                        return JSON.stringify({{ ok: true, value: el.value, text: m.text.trim() }});
                    }})()"#,
                    resolver = RESOLVE_JS,
                    sel = serde_json::to_string(&f.selector).unwrap(),
                    val = serde_json::to_string(value).unwrap()
                );
                match c.page.evaluate(js.as_str()).await {
                    Ok(v) => {
                        let s: String = v.into_value::<String>().unwrap_or_default();
                        match serde_json::from_str::<serde_json::Value>(&s) {
                            Ok(obj) => {
                                let ok = obj.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
                                if ok {
                                    let value = obj
                                        .get("value")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let text = obj
                                        .get("text")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    select_match = Some((value, text));
                                    Ok(())
                                } else {
                                    Err(obj
                                        .get("error")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("select failed")
                                        .to_string())
                                }
                            }
                            Err(_) => Err(s),
                        }
                    }
                    Err(e) => Err(e.to_string()),
                }
            } else if editable {
                let js = format!(
                    r#"(() => {{ {resolver}
                        const el = __bmResolveEl({sel});
                        if (!el) return 'selector matched no element';
                        el.focus(); el.innerText = {val};
                        el.dispatchEvent(new Event('input', {{bubbles:true}}));
                        return '';
                    }})()"#,
                    resolver = RESOLVE_JS,
                    sel = serde_json::to_string(&f.selector).unwrap(),
                    val = serde_json::to_string(value).unwrap()
                );
                c.page
                    .evaluate(js.as_str())
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            } else if tag == "textarea" {
                let js = format!(
                    r#"(() => {{ {resolver}
                        const el = __bmResolveEl({sel});
                        if (!el) return 'selector matched no element';
                        el.focus(); el.value = {val};
                        el.dispatchEvent(new Event('input', {{bubbles:true}}));
                        el.dispatchEvent(new Event('change', {{bubbles:true}}));
                        return '';
                    }})()"#,
                    resolver = RESOLVE_JS,
                    sel = serde_json::to_string(&f.selector).unwrap(),
                    val = serde_json::to_string(value).unwrap()
                );
                match c.page.evaluate(js.as_str()).await {
                    Ok(v) => {
                        let e = v.into_value::<String>().unwrap_or_default();
                        if e.is_empty() {
                            Ok(())
                        } else {
                            Err(e)
                        }
                    }
                    Err(e) => Err(e.to_string()),
                }
            } else if selector_needs_in_page(&f.selector) {
                // Shadow / frame elements cannot be reached by chromiumoxide's
                // find_element; type in-page (input/change events still fire).
                let type_js = format!(
                    r#"(() => {{ {resolver}
                        const el = __bmResolveEl({sel});
                        if (!el) return 'selector matched no element';
                        el.focus();
                        el.value = {val};
                        el.dispatchEvent(new Event('input', {{bubbles:true}}));
                        el.dispatchEvent(new Event('change', {{bubbles:true}}));
                        return '';
                    }})()"#,
                    resolver = RESOLVE_JS,
                    sel = serde_json::to_string(&f.selector).unwrap(),
                    val = serde_json::to_string(value).unwrap()
                );
                match c.page.evaluate(type_js.as_str()).await {
                    Ok(v) => {
                        let e = v.into_value::<String>().unwrap_or_default();
                        if e.is_empty() {
                            Ok(())
                        } else {
                            Err(e)
                        }
                    }
                    Err(e) => Err(e.to_string()),
                }
            } else {
                // Real typing: focus-click, clear, key events — JS validation fires.
                match c.page.find_element(&f.selector).await {
                    Ok(el) => {
                        let clear_js = format!(
                            "(() => {{ {resolver} const el = __bmResolveEl({sel}); if (!el) return ''; el.focus(); el.value = ''; el.dispatchEvent(new Event('input', {{bubbles:true}})); return ''; }})()",
                            resolver = RESOLVE_JS,
                            sel = serde_json::to_string(&f.selector).unwrap()
                        );
                        let _ = c.page.evaluate(clear_js.as_str()).await;
                        el.click().await.map_err(|e| e.to_string()).and(
                            el.type_str(value)
                                .await
                                .map(|_| ())
                                .map_err(|e| e.to_string()),
                        )
                    }
                    Err(e) => Err(format!("find element: {}", e)),
                }
            }
        } else {
            Err("field entry needs one of value/checked/file".to_string())
        };

        let now = read_back(&c.page, &f.selector).await;
        let requested = f
            .value
            .clone()
            .or(f.file.clone())
            .or(f.checked.map(|b| {
                if b {
                    "checked".into()
                } else {
                    "unchecked".into()
                }
            }))
            .unwrap_or_default();
        let ok = outcome.is_ok()
            && (f.checked.is_some() && now == requested
                || f.file.is_some() && !now.is_empty()
                || f.value.is_some()
                    && (if tag == "select" {
                        if let Some((value, text)) = &select_match {
                            requested == *value || requested == *text
                        } else {
                            false
                        }
                    } else {
                        now == requested
                    }));
        results.push(serde_json::json!({
            "selector": f.selector,
            "requested": requested,
            "now_contains": now,
            "ok": ok,
            "error": outcome.err()
        }));
    }

    let filled_ok = results.iter().filter(|r| r["ok"] == true).count();
    let mut out = serde_json::json!({
        "command": "fill",
        "session": c.id,
        "url": url,
        "fields_requested": spec.fields.len(),
        "fields_ok": filled_ok,
        "results": results,
        "note": "fill NEVER submits — review the filled state, then use `submit` (two-key rule) or submit manually",
        "evidence": evidence()
    });
    if let Some(shot) = screenshot_out {
        if let Ok(sc) = crate::chromehand::actions::screenshot(&c.page, shot).await {
            out["screenshot"] = sc;
        }
    }
    Ok(out)
}

// endregion: fill

// region: submit, and the two-key rule
// ---------------------------------------------------------------------------
// submit, and the two-key rule
//
// The only code here that can cause something irreversible to happen on
// somebody else's server, and it is gated twice over. Auto-submit needs BOTH
// the calling agent's `--yes-actually-submit` AND `allow_auto_submit` in the
// user's own config file — one key each, held by different parties, and an
// agent that could turn both would be holding no gate at all. Without the
// flag, `submit` is observe mode: it requires a headful session and waits for
// the human to click. Either way the attempt is logged, and `submitted` is
// only ever claimed on an observed URL change rather than on a click landing.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct UserConfig {
    allow_auto_submit: bool,
    allow_auto_submit_domains: Option<Vec<String>>,
}

fn read_user_config() -> UserConfig {
    let Ok(raw) = std::fs::read_to_string(USER_CONFIG_PATH) else {
        return UserConfig::default();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return UserConfig::default();
    };
    UserConfig {
        allow_auto_submit: v
            .get("allow_auto_submit")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        allow_auto_submit_domains: v
            .get("allow_auto_submit_domains")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str())
                    .map(|s| s.trim_start_matches('.').to_lowercase())
                    .collect()
            }),
    }
}

fn log_submit(url: &str, domain: &str, mode: &str) {
    let entry = serde_json::json!({
        "timestamp": now_iso(), "url": url, "domain": domain, "mode": mode
    });
    if let Some(dir) = std::path::Path::new(SUBMIT_LOG_PATH).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(SUBMIT_LOG_PATH)
    {
        use std::io::Write;
        let _ = writeln!(f, "{}", entry);
    }
}

/// `submit --session S --selector CSS [--yes-actually-submit]`
pub async fn submit(
    c: &Connected,
    pol: &Policy,
    selector: &str,
    yes_actually_submit: bool,
    headful_session: bool,
    timeout_ms: u64,
) -> Result<serde_json::Value, String> {
    let url = c.page.url().await.ok().flatten().unwrap_or_default();
    pol.check_interaction(&url)?;
    let domain = url::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_lowercase()))
        .unwrap_or_default();

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
        .ok()
        .and_then(|v| v.into_value::<String>().ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({ "found": false }));
    if !pre.get("found").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err(format!("submit control not found: {}", selector));
    }
    if !pre
        .get("isSubmit")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Err(format!(
            "refused: {} is not a submit-type control — submit only clicks submit controls",
            selector
        ));
    }

    if yes_actually_submit {
        // KEY 2: the user's own config file. An agent cannot write this for
        // them — its absence is a refusal, not a prompt to create it.
        let cfg = read_user_config();
        if !cfg.allow_auto_submit {
            return Err(format!(
                "refused: auto-submit needs BOTH keys; missing key 2 — the user must set \"allow_auto_submit\": true in {} themselves (agents may not edit that file)",
                USER_CONFIG_PATH
            ));
        }
        if let Some(domains) = &cfg.allow_auto_submit_domains {
            let allowed = domains
                .iter()
                .any(|d| domain == *d || domain.ends_with(&format!(".{}", d)));
            if !allowed {
                return Err(format!(
                    "refused: allow_auto_submit_domains in {} does not include '{}'",
                    USER_CONFIG_PATH, domain
                ));
            }
        }

        // Both keys present: click the submit control, observe the outcome.
        let before = mini_digest(&c.page).await;
        let before_url = before["final_url"].as_str().unwrap_or("").to_string();
        if selector_needs_in_page(selector) {
            let click_js = format!(
                r#"(() => {{ {resolver}
                    const el = __bmResolveEl({sel});
                    if (!el) return 'submit control not found';
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
                .map_err(|e| format!("submit control not found: {}", e))?;
            el.click().await.map_err(|e| format!("click: {}", e))?;
        }
        bounded_nav_wait(&c.page, timeout_ms.min(15_000)).await;
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        let after = mini_digest(&c.page).await;
        let final_url = after["final_url"].as_str().unwrap_or("").to_string();
        let url_changed =
            !before_url.is_empty() && !final_url.is_empty() && final_url != before_url;
        let submitted = url_changed;
        log_submit(&before_url, &domain, "auto");
        let note = if submitted {
            "auto-submitted; navigation/URL change observed"
        } else {
            "click issued but no navigation/URL change observed — the form may have validated client-side or submitted via XHR; verify via a follow-up digest"
        };
        return Ok(serde_json::json!({
            "command": "submit",
            "session": c.id,
            "mode": "auto",
            "selector": selector,
            "submitted": submitted,
            "url_before": before_url,
            "final_url": final_url,
            "url_changed": url_changed,
            "page_title": after["page_title"],
            "submit_log": SUBMIT_LOG_PATH,
            "note": note,
            "evidence": evidence()
        }));
    }

    // Default: OBSERVE mode — the human's click is the confirmation.
    if !headful_session {
        return Err(
            "refused: default submit is human-observed and needs a --headful session (open one with `session open --headful`), or pass --yes-actually-submit with the user config key set (two-key rule)"
                .to_string(),
        );
    }
    eprintln!("submit[observe]: review the form in the Chrome window and click submit yourself; watching for the outcome...");
    let before_url = url.clone();
    // Watch for a navigation or URL change for up to timeout_ms.
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    let mut submitted = false;
    let mut final_url = before_url.clone();
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let cur = c.page.url().await.ok().flatten().unwrap_or_default();
        if !cur.is_empty() && cur != before_url {
            submitted = true;
            final_url = cur;
            break;
        }
    }
    if submitted {
        log_submit(&before_url, &domain, "observed-human");
    }
    let after = mini_digest(&c.page).await;
    Ok(serde_json::json!({
        "command": "submit",
        "session": c.id,
        "mode": "observe",
        "submitted": submitted,
        "url_before": before_url,
        "final_url": final_url,
        "page_title": after["page_title"],
        "note": if submitted { "human-submitted; outcome recorded" } else { "no navigation observed within the timeout — nothing was submitted" },
        "evidence": evidence()
    }))
}

// endregion: submit, and the two-key rule
