//! The facts in `README.md` that source can contradict, generated from it.
//!
//! Blocks: readme-providers, readme-tools, readme-budgets.
//!
//! Everything else in this module's siblings is an SVG for a page under
//! `docs/`. These produce Markdown for the README, through the same markers
//! and the same freshness test, because the README drifts the same way: a
//! docs page beside a generated diagram said "one provider exists" for weeks
//! after the second shipped, and the README's "two providers" sentence will say
//! two after the third. `tests/current.rs` is what notices.
//!
//! # What is generated and what is not
//!
//! Only the falsifiable part. The README's worth is the sentences no generator
//! writes, so each block is the smallest span that names a thing the source
//! declares: which providers exist, which tools register, what the budgets
//! default to. The words around each list are a template, kept short so that a
//! third provider or a new tool reads as the README would have read had a
//! person typed it.
//!
//! # Invariants
//!
//! Output is Markdown with no em-dash in it, wrapped at the README's column
//! width, and idempotent: the same source produces the same bytes. A fact that
//! cannot be read is an error, never an empty list, for the reason
//! [`crate::rust`] gives.

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::rust;

/// The column the README wraps its prose at.
const WIDTH: usize = 80;

/// `**Any model that can call tools.** Anthropic over the API, or Ollama on
/// your own machine.`
///
/// The names come from `llm::kind::KINDS`, the list `emma set-provider`
/// measures a name against, so a provider that is written and not registered
/// is not named here either. Which side of the "or" each one lands on is read
/// from `ProviderKind::requires_key`: a provider that needs no key is one that
/// runs on the user's machine, which is the trait's own reason for the method.
pub fn providers(root: &Path) -> Result<String> {
    let dir = root.join("crates/llm/src");
    let kind_path = dir.join("kind.rs");
    let kinds = rust::parse(&kind_path)?;
    let types = rust::const_list(&kinds, "KINDS")
        .with_context(|| format!("{} declares KINDS", kind_path.display()))?;
    let default_needs_key = trait_default_requires_key(&kinds)
        .with_context(|| format!("{} declares ProviderKind", kind_path.display()))?;

    // Each kind's `impl ProviderKind` lives in its own module, and `KINDS`
    // names the type without saying which. Every file in the crate is a
    // candidate; the first one carrying the impl answers.
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "rs") {
            files.push((path.clone(), rust::parse(&path)?));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hosted = Vec::new();
    let mut local = Vec::new();
    for ty in &types {
        let mut found = None;
        for (path, file) in &files {
            if let Some(name) = rust::impl_method_value(file, &ty.name, "name")? {
                let needs_key = match rust::impl_method_value(file, &ty.name, "requires_key")? {
                    Some(v) => v == "true",
                    None => default_needs_key,
                };
                found = Some((name, needs_key, path));
                break;
            }
        }
        let Some((name, needs_key, _)) = found else {
            bail!(
                "KINDS names `{}` and no file under {} implements ProviderKind for it",
                ty.name,
                dir.display()
            );
        };
        if needs_key {
            hosted.push(format!("{} over the API", capitalise(&name)));
        } else {
            local.push(format!("{} on your own machine", capitalise(&name)));
        }
    }
    let parts: Vec<String> = hosted.into_iter().chain(local).collect();
    if parts.is_empty() {
        bail!("KINDS is empty, so there is no provider to name");
    }
    Ok(wrap(&format!(
        "**Any model that can call tools.** {}.",
        join_or(&parts)
    )))
}

/// What `ProviderKind::requires_key` answers when an impl does not override it.
fn trait_default_requires_key(file: &syn::File) -> Result<bool> {
    for item in &file.items {
        let syn::Item::Trait(t) = item else { continue };
        if t.ident != "ProviderKind" {
            continue;
        }
        for it in &t.items {
            let syn::TraitItem::Fn(f) = it else { continue };
            if f.sig.ident != "requires_key" {
                continue;
            }
            let Some(body) = &f.default else {
                bail!("ProviderKind::requires_key has no default, so an impl without one is a compile error and not a fact");
            };
            if let Some(syn::Stmt::Expr(syn::Expr::Lit(l), None)) = body.stmts.last() {
                if let syn::Lit::Bool(b) = &l.lit {
                    return Ok(b.value);
                }
            }
            bail!("ProviderKind::requires_key's default is not a bare bool");
        }
        bail!("ProviderKind no longer has requires_key");
    }
    bail!("no `trait ProviderKind` in this file")
}

/// The paragraph under Configuration that lists the tools.
///
/// Two lists. The first is the registry every run assembles, built the way
/// `main::run` and `tests/prompt_size.rs` build it, so the names are the ones
/// `Tool::name` returns at runtime and not a reading of the source. The second
/// is read from source, because each of those tools registers only when this
/// machine has what it needs and the generator's machine is not the reader's:
/// `Skill` wants a skill in the configuration directory, `Delegate` an agent,
/// `WebFetch` and the browser tools a Chrome. The conditions themselves are
/// prose here; what `main.rs` checks for each is
/// not something a parser states, and a reader who wants the argument has
/// `main.rs`'s comments.
pub fn tools(root: &Path) -> Result<String> {
    let mut registry = emma_tool_api::Registry::new();
    let (fs, _tracker) = emma_tools_fs::fs_tools();
    for tool in fs {
        registry.register(tool);
    }
    for tool in emma_tools_tasks::task_tools() {
        registry.register(tool);
    }
    let (lsp, _pool) = emma_tools_lsp::lsp_tools();
    for tool in lsp {
        registry.register(tool);
    }
    let always: Vec<String> = registry
        .names()
        .into_iter()
        .map(|n| format!("`{n}`"))
        .collect();
    if always.is_empty() {
        bail!("the unconditional registry is empty");
    }

    let name_of = |rel: &str, ty: &str| -> Result<String> {
        let path = root.join(rel);
        let file = rust::parse(&path)?;
        rust::impl_method_value(&file, ty, "name")?
            .with_context(|| format!("{} implements Tool for {ty}", path.display()))
    };
    let skill = name_of("crates/emma/src/skill.rs", "Skill")?;
    let delegate = name_of("crates/emma/src/delegate.rs", "Delegate")?;
    let fetch = name_of("tools/web/src/fetch.rs", "WebFetch")?;
    let browser_mod = rust::parse(&root.join("tools/web/src/browser/mod.rs"))?;
    let browser_types = rust::vec_types(&browser_mod, "browser_tools")
        .context("tools/web/src/browser/mod.rs builds the browser surface")?;
    let browser_impls = rust::parse(&root.join("tools/web/src/browser/tools.rs"))?;
    let mut browser = Vec::new();
    for ty in &browser_types {
        let name = rust::impl_method_value(&browser_impls, ty, "name")?
            .with_context(|| format!("tools/web/src/browser/tools.rs implements Tool for {ty}"))?;
        browser.push(format!("`{name}`"));
    }

    Ok(wrap(&format!(
        "Tools available to the model on every run: {}. Registered only when this \
         machine can back them: `{skill}`, when the configuration directory declares a \
         skill; `{delegate}`, when it declares an agent; `{fetch}` and the browser tools \
         {}, when a Chrome can be found. Web search is not a tool of Emma's: the \
         provider runs it, on the same key, when the provider has one.",
        join_and(&always),
        join_and(&browser),
    )))
}

/// The budget table, from `Budgets::default()` in `agent.rs`.
///
/// The numbers are read; the flag names and the third column are not. Each
/// row here pairs a field with the flag `cli.rs` matches on and a phrase, and
/// the generator refuses when the two drift apart in either direction: a
/// field with no row, a row whose field is gone, or a flag `cli.rs` no longer
/// has a match arm for. A new budget therefore cannot ship without somebody
/// writing its sentence, which is the point of keeping the sentence here.
pub fn budgets(root: &Path) -> Result<String> {
    const ROWS: &[(&str, &str, &str)] = &[
        ("max_iterations", "--max-iterations", "model calls per goal"),
        ("max_tokens", "--max-tokens", "billable tokens per goal"),
        ("wall_clock", "--timeout", "seconds of wall clock per goal"),
        (
            "max_kicks",
            "--max-kicks",
            "times the loop may say \"not done, continue\"",
        ),
        (
            "max_context",
            "--max-context",
            "request size before the conversation is compacted",
        ),
    ];

    let agent_path = root.join("crates/emma/src/agent.rs");
    let agent = rust::parse(&agent_path)?;
    let fields = rust::default_fields(&agent, "Budgets")
        .with_context(|| format!("{} declares Budgets::default", agent_path.display()))?;
    let cli_path = root.join("crates/emma/src/cli.rs");
    let flags = rust::string_literals(&rust::parse(&cli_path)?);

    let mut table: Vec<[String; 3]> = Vec::new();
    for field in &fields {
        let Some((_, flag, what)) = ROWS.iter().find(|(f, _, _)| *f == field.name) else {
            bail!(
                "Budgets has a field `{}` with no README row; add one to ROWS in pages/readme.rs",
                field.name
            );
        };
        if !flags.iter().any(|s| s == flag) {
            bail!(
                "{} has no `{flag}` literal, so the README would document a flag the parser \
                 does not accept",
                cli_path.display()
            );
        }
        if field.detail.parse::<u64>().is_err() {
            bail!(
                "Budgets::default sets `{}` to `{}`, which is not a number this reader can print",
                field.name,
                field.detail
            );
        }
        table.push([format!("`{flag}`"), field.detail.clone(), what.to_string()]);
    }
    for (f, _, _) in ROWS {
        if !fields.iter().any(|x| x.name == *f) {
            bail!("ROWS names `{f}`, which Budgets no longer has");
        }
    }
    Ok(markdown_table(
        &["Flag", "Default", "What it bounds"],
        &table,
    ))
}

/// A pipe table in the layout the README already uses: every column padded to
/// its widest cell, so the source reads as a table too.
fn markdown_table(head: &[&str; 3], rows: &[[String; 3]]) -> String {
    let mut width = [head[0].len(), head[1].len(), head[2].len()];
    for r in rows {
        for (i, cell) in r.iter().enumerate() {
            width[i] = width[i].max(cell.len());
        }
    }
    let line = |cells: [&str; 3]| -> String {
        format!(
            "| {:<w0$} | {:<w1$} | {:<w2$} |",
            cells[0],
            cells[1],
            cells[2],
            w0 = width[0],
            w1 = width[1],
            w2 = width[2]
        )
    };
    let mut out = String::new();
    out.push_str(&line(*head));
    out.push('\n');
    out.push_str(&format!(
        "| {} | {} | {} |",
        "-".repeat(width[0]),
        "-".repeat(width[1]),
        "-".repeat(width[2])
    ));
    for r in rows {
        out.push('\n');
        out.push_str(&line([r[0].as_str(), r[1].as_str(), r[2].as_str()]));
    }
    out
}

/// Greedy word wrap at [`WIDTH`], the way the README's own paragraphs are
/// wrapped, so a generated block does not stand out as one long line.
fn wrap(text: &str) -> String {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > WIDTH {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines.join("\n")
}

/// `a`, `a and b`, `a, b and c`: the README's own list style, no serial comma.
fn join_and(items: &[String]) -> String {
    join_with(items, "and")
}

/// `a`, `a, or b`, `a, b, or c`. The comma before "or" is kept with two items
/// because the README's sentence has one: the halves are clauses, not nouns.
fn join_or(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        _ => {
            let (last, rest) = items.split_last().expect("two or more");
            format!("{}, or {last}", rest.join(", "))
        }
    }
}

fn join_with(items: &[String], word: &str) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        _ => {
            let (last, rest) = items.split_last().expect("two or more");
            format!("{} {word} {last}", rest.join(", "))
        }
    }
}

/// `anthropic` is how the source spells it and `Anthropic` is how prose does.
fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root")
    }

    /// **If this breaks:** the README names a provider set the binary does not
    /// have. Asserted by membership rather than count, so registering a third
    /// provider does not need this test edited -- the README will regenerate
    /// and `tests/current.rs` will ask for the commit.
    #[test]
    fn the_provider_sentence_names_the_registered_kinds_and_says_which_is_local() {
        let text = providers(&root()).expect("the sentence builds");
        // Membership is checked on the unwrapped sentence: a phrase can break
        // across the README's 80-column wrap, and did on the first run.
        let flat = text.replace('\n', " ");
        assert!(flat.contains("Anthropic over the API"), "{text}");
        assert!(flat.contains("Ollama on your own machine"), "{text}");
        assert!(
            text.starts_with("**Any model that can call tools.**"),
            "{text}"
        );
        assert!(
            !text.contains('\u{2014}'),
            "an em-dash reached the README: {text}"
        );
        for line in text.lines() {
            assert!(line.len() <= WIDTH, "over {WIDTH} columns: {line}");
        }
    }

    /// **If this breaks:** the tool list names a tool the registry does not
    /// register, or presents a conditional one as always there.
    #[test]
    fn the_tool_paragraph_lists_the_registry_and_marks_the_conditional_ones() {
        let text = tools(&root()).expect("the paragraph builds");
        let (always, conditional) = text
            .split_once("Registered only when")
            .expect("two halves: {text}");
        for name in ["`Read`", "`Bash`", "`TaskCreate`", "`FindReferences`"] {
            assert!(
                always.contains(name),
                "{name} is missing from the unconditional list: {text}"
            );
        }
        for name in ["`Skill`", "`Delegate`", "`WebFetch`", "`BrowserOpen`"] {
            assert!(
                conditional.contains(name),
                "{name} is missing from the conditional list: {text}"
            );
            assert!(
                !always.contains(name),
                "{name} is presented as unconditional: {text}"
            );
        }
        for line in text.lines() {
            assert!(line.len() <= WIDTH, "over {WIDTH} columns: {line}");
        }
    }

    /// **If this breaks:** the budget table shows a default the loop does not
    /// use. The expected number is read from `agent.rs` here too, so the test
    /// fails only when the table and the source disagree, not when the owner
    /// changes a budget.
    #[test]
    fn the_budget_table_carries_the_defaults_agent_rs_declares() {
        let root = root();
        let agent = rust::parse(&root.join("crates/emma/src/agent.rs")).expect("agent.rs parses");
        let fields = rust::default_fields(&agent, "Budgets").expect("Budgets::default");
        let text = budgets(&root).expect("the table builds");
        let iterations = &fields
            .iter()
            .find(|f| f.name == "max_iterations")
            .expect("max_iterations is a budget")
            .detail;
        let row = text
            .lines()
            .find(|l| l.contains("`--max-iterations`"))
            .expect("a row for --max-iterations");
        assert!(row.contains(&format!("| {iterations} ")), "{row}");
        let wall = &fields
            .iter()
            .find(|f| f.name == "wall_clock")
            .expect("wall_clock is a budget")
            .detail;
        let row = text
            .lines()
            .find(|l| l.contains("`--timeout`"))
            .expect("a row for --timeout");
        assert!(
            row.contains(&format!("| {wall} ")),
            "seconds, not a Duration: {row}"
        );
        assert!(text.starts_with("| Flag "), "{text}");
        assert_eq!(text.lines().count(), fields.len() + 2, "{text}");
    }

    /// **If this breaks:** a generated paragraph is one long line, or a word is
    /// split, and the README source stops reading like the rest of it.
    #[test]
    fn wrap_breaks_between_words_at_the_readme_column() {
        let text = "a ".repeat(100);
        let out = wrap(&text);
        assert!(out.lines().all(|l| l.len() <= WIDTH), "{out}");
        assert!(out.lines().count() > 1, "{out}");
        assert_eq!(out.split_whitespace().count(), 100);
        let long = "x".repeat(WIDTH + 5);
        assert_eq!(
            wrap(&long),
            long,
            "a word longer than the width is not split"
        );
    }

    #[test]
    fn list_joins_read_like_the_readme() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(join_and(&s(&["a"])), "a");
        assert_eq!(join_and(&s(&["a", "b"])), "a and b");
        assert_eq!(join_and(&s(&["a", "b", "c"])), "a, b and c");
        assert_eq!(join_or(&s(&["a", "b"])), "a, or b");
        assert_eq!(join_or(&s(&["a", "b", "c"])), "a, b, or c");
    }

    /// **If this breaks:** the generator draws the README from a tree that is
    /// not this workspace and reports success.
    #[test]
    fn a_missing_source_fails_the_block_rather_than_emptying_it() {
        let root = Path::new("definitely-not-a-workspace");
        assert!(providers(root).is_err());
        assert!(budgets(root).is_err());
        assert!(tools(root).is_err());
    }
}
