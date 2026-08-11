//! What a session verb reads like.
//!
//! `crate::digest_md` renders a *page*. This renders the two things a session
//! produces that a page read does not: the 600-character snapshot every verb in
//! `chromehand::actions` returns, and the selector-keyed delta between one read
//! and the last.
//!
//! **The cheap answer and the expensive one, kept apart.** Every verb returns
//! `mini_digest` — `final_url`, `page_title`, `looks_blocked`, no payload — and
//! that is the answer to "did it work, and where am I now". It is what
//! [`act`] renders and it costs about forty tokens. The payload is
//! `BrowserRead`'s job, and by default it is a *delta*, because `compute_delta`
//! diffs by stable selector: a click that opens a menu reports the menu as an
//! addition rather than reporting that the whole page moved. Those two together
//! are what make a ten-step session affordable; collapsing them — returning the
//! page after every click — is what makes one unaffordable.
//!
//! **An honest negative is not an error and must not read like one.** A click
//! that matched no element, a `wait_for` that timed out, a `select` with no
//! matching option: all of them are `Ok` with a sentence saying what happened,
//! because the model's next move differs for each and "the tool failed" tells it
//! none of them apart.

use serde_json::Value;

/// One verb's result, for the model and for the terminal.
pub struct Rendered {
    pub content: String,
    pub display: String,
}

/// The snapshot every action verb returns.
///
/// `action` is the word the model used, echoed back so a transcript reads as a
/// sequence of moves rather than a sequence of identical URL reports.
pub fn act(action: &str, session: &str, v: &Value) -> Rendered {
    let url = s(v, "final_url");
    let title = s(v, "page_title");
    let mut out = format!("{action} on session {session}\n");
    out.push_str(&format!(
        "- URL: {}\n",
        if url.is_empty() { "(unknown)" } else { url }
    ));
    if !title.is_empty() {
        out.push_str(&format!("- Title: {title}\n"));
    }
    if let Some(status) = v.get("http_status").and_then(Value::as_i64) {
        out.push_str(&format!("- Status: HTTP {status}\n"));
    }

    // The verb-specific facts. Each arm is a sentence the model can route on;
    // none of them is an error, and that is the point.
    for line in outcome_lines(v) {
        out.push_str(&format!("- {line}\n"));
    }

    if v.get("looks_blocked").and_then(Value::as_bool) == Some(true) {
        out.push_str(
            "\n**This page is serving an anti-bot challenge rather than its content.** \
             Interaction from here will not work, and there is no way around it — this needs \
             a human or a different source.\n",
        );
    }
    out.push_str("\nThis is the snapshot, not the page: call BrowserRead to see what changed.\n");

    let display = format!("{action} — {}", if title.is_empty() { url } else { title });
    Rendered {
        content: out,
        display,
    }
}

/// The facts one verb reports beyond "where am I".
///
/// Split out so the negatives are in one list rather than scattered through
/// `act`'s formatting: every line here is a thing that did not happen, and the
/// hazard is one of them being dropped and the result reading as success.
fn outcome_lines(v: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(sel) = v.get("selector").and_then(Value::as_str) {
        lines.push(format!("Selector: `{sel}`"));
    }
    if v.get("clicked").and_then(Value::as_bool) == Some(false) {
        lines.push(format!(
            "Nothing was clicked: {}",
            s_or(v, "error", "the selector matched no element")
        ));
    }
    if let Some(met) = v.get("met").and_then(Value::as_bool) {
        let waited = v.get("waited_ms").and_then(Value::as_u64).unwrap_or(0);
        lines.push(if met {
            format!("Condition met after {waited} ms")
        } else {
            format!(
                "Condition NOT met within {waited} ms — the page never reached that state; \
                 this is the answer, not a failure to look"
            )
        });
    }
    if v.get("navigated").and_then(Value::as_bool) == Some(false) {
        lines.push(format!(
            "Did not move: {}",
            s_or(v, "reason", "no history entry in that direction")
        ));
    }
    // `type` and `select` both report `ok`, and both re-read the live DOM rather
    // than trusting the write — so the value below is what is actually in the
    // field, which is the only version worth reporting.
    if let Some(ok) = v.get("ok").and_then(Value::as_bool) {
        if ok {
            if let Some(value) = v.get("value").and_then(Value::as_str) {
                lines.push(format!("Field now reads: {value}"));
            }
        } else {
            lines.push(format!(
                "The field was not set: {}",
                s_or(v, "error", "the page did not accept the value")
            ));
            if let Some(options) = v.get("options").and_then(Value::as_array) {
                let names: Vec<String> = options
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                if !names.is_empty() {
                    lines.push(format!("Available options: {}", names.join(", ")));
                }
            }
        }
    }
    lines
}

/// A `compute_delta` output — what changed since the previous read.
///
/// The `baseline: true` case is not a delta at all: it is the first read, and
/// chromehand hands back the full digest annotated. The caller renders that
/// through `digest_md` instead; this function only sees the diff.
pub fn delta(session: &str, v: &Value) -> Rendered {
    let d = v.get("delta").cloned().unwrap_or(Value::Null);
    let url = s(v, "final_url");
    let title = s(v, "page_title");

    let mut out = format!(
        "# {}\n\n",
        if title.is_empty() {
            "(untitled page)"
        } else {
            title
        }
    );
    out.push_str(&format!("- URL: {url}\n"));
    out.push_str(&format!("- Session: {session}\n"));
    out.push_str("- This is a **delta**: only what changed since the previous read.\n\n");

    let text_changed = d.get("text_changed").and_then(Value::as_bool) == Some(true);
    if text_changed {
        out.push_str("## Content (changed)\n\n");
        let text = d.get("text").and_then(Value::as_str).unwrap_or("");
        if text.trim().is_empty() {
            out.push_str("(the page's readable text is now empty)\n\n");
        } else {
            out.push_str(text.trim_end());
            out.push_str("\n\n");
        }
    } else {
        out.push_str("## Content\n\nUnchanged since the previous read.\n\n");
    }

    let mut anything = text_changed;
    for (key, heading) in [
        ("added", "Appeared"),
        ("removed", "Gone"),
        ("changed", "Changed"),
    ] {
        let items = d
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if items.is_empty() {
            continue;
        }
        anything = true;
        out.push_str(&format!("## {heading} ({})\n\n", items.len()));
        for item in items {
            out.push_str(&format!("- {}\n", element_line(item)));
        }
        if d.get(format!("{key}_truncated")).and_then(Value::as_bool) == Some(true) {
            // chromehand's own per-category budget. Named rather than hidden:
            // a delta that quietly dropped items is a model reasoning about a
            // page it only half saw.
            out.push_str(
                "\n[truncated: chromehand caps each delta category at 60 items; re-read with \
                 delta=false for the whole page]\n",
            );
        }
        out.push('\n');
    }

    if !anything {
        // The governing rule, made visible. "Nothing changed" is an answer, and
        // a model told otherwise will click again to make something happen.
        out.push_str(
            "Nothing changed on this page since the previous read — no text difference and no \
             elements added, removed or altered.\n",
        );
    }

    Rendered {
        display: format!("delta — {}", if anything { "changes" } else { "no change" }),
        content: out,
    }
}

/// One delta entry: its selector first, because the selector is what the next
/// call needs.
///
/// `compute_delta` labels a *removed* element with its category but hands back
/// an *added* one verbatim, exactly as the page inventory held it — so for
/// additions the category has to be read off the shape. Worth the four lines:
/// without them every new element on the page is called "element", and the
/// difference between a link appearing and a form field appearing is the whole
/// content of the line.
fn element_line(item: &Value) -> String {
    let kind = match item.get("kind").and_then(Value::as_str) {
        Some(kind) => kind,
        None if item.get("href").is_some() => "link",
        None if item.get("tag").is_some() => "field",
        None if item.get("label").is_some() => "button",
        None => "element",
    };
    let selector = s_or(item, "selector", "(no selector)");
    let label = ["text", "label", "href", "type"]
        .iter()
        .filter_map(|k| item.get(*k).and_then(Value::as_str))
        .find(|v| !v.trim().is_empty())
        .unwrap_or("");
    if label.is_empty() {
        format!("{kind} `{selector}`")
    } else {
        format!("{kind} `{selector}` — {}", label.replace('\n', " "))
    }
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn s_or<'a>(v: &'a Value, key: &str, fallback: &'a str) -> &'a str {
    match v.get(key).and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s,
        _ => fallback,
    }
}

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Every one of these is about a negative that could plausibly render as a
// success. A click that hit nothing, a wait that expired, a delta with no
// changes: if any of them reads as "done", the model's next move is wrong and
// nothing in the transcript says why.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_click_that_hit_nothing_says_so_rather_than_reading_as_done() {
        let r = act(
            "click",
            "a1",
            &json!({
                "final_url": "https://example.com/",
                "page_title": "Example",
                "selector": "#missing",
                "clicked": false,
                "error": "selector matched no element"
            }),
        );
        assert!(r.content.contains("Nothing was clicked"), "{}", r.content);
        assert!(r.content.contains("#missing"), "{}", r.content);
    }

    #[test]
    fn a_wait_that_expired_is_an_answer_and_names_the_time() {
        let r = act(
            "wait_for",
            "a1",
            &json!({ "final_url": "https://x/", "met": false, "waited_ms": 5000 }),
        );
        assert!(r.content.contains("NOT met"), "{}", r.content);
        assert!(r.content.contains("5000"), "{}", r.content);
        assert!(
            r.content.contains("not a failure to look"),
            "a timeout read as a malfunction: {}",
            r.content
        );
    }

    #[test]
    fn a_blocked_page_is_the_loudest_thing_in_an_action_result() {
        let r = act(
            "navigate",
            "a1",
            &json!({ "final_url": "https://x/", "page_title": "Just a moment…", "looks_blocked": true }),
        );
        assert!(r.content.contains("anti-bot challenge"), "{}", r.content);
    }

    /// The economy the whole session design rests on: an action result carries
    /// no page. If this ever grows the payload, a ten-step session costs ten
    /// full pages and the delta machinery is pointless.
    #[test]
    fn an_action_result_carries_no_page_and_says_where_the_page_is() {
        let r = act(
            "click",
            "a1",
            &json!({ "final_url": "https://x/", "page_title": "T", "clicked": true }),
        );
        assert!(r.content.len() < 400, "{}", r.content);
        assert!(r.content.contains("BrowserRead"), "{}", r.content);
    }

    #[test]
    fn a_delta_with_no_changes_says_nothing_changed() {
        let r = delta(
            "a1",
            &json!({
                "final_url": "https://x/",
                "page_title": "T",
                "delta": { "baseline": false, "text_changed": false, "added": [], "removed": [], "changed": [] }
            }),
        );
        assert!(r.content.contains("Nothing changed"), "{}", r.content);
    }

    #[test]
    fn a_delta_leads_with_the_selector_because_that_is_what_the_next_call_needs() {
        let r = delta(
            "a1",
            &json!({
                "final_url": "https://x/",
                "page_title": "T",
                "delta": {
                    "baseline": false,
                    "text_changed": false,
                    // No `kind`, exactly as `compute_delta` hands back an
                    // addition — the label is what makes it a button.
                    "added": [{ "selector": "#buy", "label": "Buy now", "type": "button" }],
                    "removed": [], "changed": []
                }
            }),
        );
        let line = r
            .content
            .lines()
            .find(|l| l.contains("#buy"))
            .expect("the added button was not listed");
        assert!(line.contains("`#buy`"), "{line}");
        assert!(line.contains("Buy now"), "{line}");
        assert!(
            line.starts_with("- button"),
            "an added element was labelled generically — a link appearing and a field \
             appearing read the same: {line}"
        );
    }

    #[test]
    fn a_capped_delta_category_admits_it() {
        let items: Vec<Value> = (0..60)
            .map(|i| json!({ "kind": "link", "selector": format!("#l{i}") }))
            .collect();
        let r = delta(
            "a1",
            &json!({
                "final_url": "https://x/", "page_title": "T",
                "delta": { "baseline": false, "text_changed": false, "added": items,
                           "added_truncated": true, "removed": [], "changed": [] }
            }),
        );
        assert!(r.content.contains("truncated"), "{}", r.content);
        assert!(r.content.contains("delta=false"), "{}", r.content);
    }
}

// endregion: Tests
