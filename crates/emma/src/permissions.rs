//! Permission rules that outlive the process.
//!
//! `approval.rs` used to end with a section titled *what is deliberately
//! absent*, and the thing it named was this file: no persistent always-allow, on
//! either axis, because "a permission the user cannot see is a permission they
//! have forgotten they granted, and the place they would not see it is a config
//! file written six weeks ago."
//!
//! That argument was right about the hazard and wrong about the cost. One goal —
//! *search the web for AI news* — asked the owner to approve
//! `api.search.brave.com`, then `www.reuters.com`, then `tech.yahoo.com`, then
//! `openai.com`, then `apnews.com`: five prompts, each granting only for that
//! process, all five asked again on the next run. A gate that costs five
//! keystrokes per errand is the click-through trainer the module was written to
//! avoid, arriving through the other door. The answer is not to lengthen the
//! session grant silently; it is to make the longer grant **a file the user can
//! read, delete and diff**, written only when they ask for it by name and shown
//! to them before it is written.
//!
//! **The format is Claude Code's, deliberately.** An existing
//! `.claude/settings.json` should work here, and a rule Emma writes should be a
//! rule the other program understands. See `docs` — the shape is
//! `permissions: { allow: [], deny: [], ask: [] }` and a rule is `Tool` or
//! `Tool(specifier)`.
//!
//! **What is supported, and what is refused.**
//!
//! | Rule | Meaning |
//! |---|---|
//! | `WebSearch` | every call to that tool |
//! | `WebSearch(*)` | the same thing; Claude Code documents them as equivalent |
//! | `WebFetch(domain:apnews.com)` | egress to exactly that host |
//! | `WebFetch(domain:*.example.com)` | any subdomain at any depth, **not** `example.com` itself |
//! | `WebFetch(domain:example.*)` | `example.org`; the `*` is one label and cannot cross a dot |
//! | `Bash(git *)` | a word-boundary prefix over the `command` argument |
//! | `Read(./src/**)`, `Read(Cargo.toml)` | a glob over the path argument, both sides normalised |
//! | `Read(src/a\*b)`, `Read(../x)` | a parse error: ambiguous across platforms, or unmatchable |
//!
//! **The last three rows used to read "parsed, never matches, said out loud at
//! boot", and that was the whole of it.** `Bash` and path specifiers were inert:
//! announced, and then matching nothing in any list, including `deny`. Measured
//! against a real settings file, **62 of its 64 rules were inert**. That is the
//! defect ARCH-002 closed, and the announcement was doing the job a matcher
//! should have been doing.
//!
//! What survives from that design is the rule about silence. A specifier this
//! build genuinely cannot evaluate is *inert and announced*, never inert and
//! quiet — and one that is not even well-formed (`WebFetch(`, `(domain:x)`,
//! `WebFetch(domain:)`) is a parse error carrying the file it came from. Neither
//! is ever resolved by guessing. What changed is how rarely that applies.
//!
//! **Two `domain:` spellings are inert for a subtler reason and get the same
//! treatment**: `WebFetch(domain:bücher.example)` and `WebFetch(domain:::1)`.
//! Both parse, both are kept, and neither can ever meet a host — hosts arrive
//! from `Url::host_str`, which has already punycoded a Unicode name and already
//! bracketed an IPv6 literal. So they are announced at boot too, with the
//! spelling that would work (`xn--bcher-kva.example`, `[::1]`) computed and
//! offered. What is *not* done is converting them: see `Inert`. The matcher
//! does no IDNA, deliberately.
//!
//! **The wildcard rules are the security surface of this file.** A rule that
//! accidentally matches everything is the worst bug available here, so matching
//! is label-wise and length-checked rather than substring-based:
//! `domain:example.com` does not match `evil-example.com`, does not match
//! `example.com.attacker.net`, and does not match `sub.example.com`. Those three
//! are pinned by tests, because each one is a host an attacker can register.

use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use emma_harness::{PermissionEntry, PermissionKind};

// region: What a rule is
// ---------------------------------------------------------------------------
// What a rule is
//
// Parsing `Tool(specifier)` into something with exactly three shapes, one of
// which — `Unsupported` — exists so that a rule this build cannot evaluate is a
// value with a name rather than a silent absence.
// ---------------------------------------------------------------------------

/// One parsed rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The tool name, matched exactly. **No globbing in this position**, which
    /// Claude Code allows for deny rules and Emma does not: `*` in the tool slot
    /// is precisely the rule that accidentally matches everything, and the two
    /// lines it would save are not worth owning that failure mode.
    pub tool: String,
    pub spec: Spec,
}

/// What a rule matches within its tool.
#[derive(Debug, Clone)]
pub enum Spec {
    /// A bare tool name, or `Tool(*)`. Every call.
    All,
    /// `Tool(domain:…)`. Matches a [`emma_tool_api::NetworkTarget`]'s host, and
    /// nothing else — notably **not** the tool's local-damage question. A grant
    /// naming one host cannot also be a grant to write files.
    Domain(DomainPattern),
    /// `Bash(git *)`, `Bash(npm run test:*)`. Matches the `command` argument by
    /// **prefix**, on whitespace-normalised text, with a trailing `*` or `:*`
    /// meaning "and anything after".
    ///
    /// Prefix rather than glob because that is what the shape means to the
    /// people who write it: `Bash(git *)` is "any git command", not a pattern
    /// language. A prefix is also the only reading that is safe to get slightly
    /// wrong in a deny list — a prefix that matches too little still denies
    /// something, where a glob that matches too little can silently deny
    /// nothing.
    Command(String, String),
    /// `Read(./src/**)`, `Edit(/etc/**)`. Matches a path-shaped argument
    /// against a glob.
    Path(String, globset::GlobMatcher),
    /// A specifier this build parsed and cannot evaluate. Never matches
    /// anything, in any list, and is reported at boot so nobody relies on it.
    Unsupported(String),
}

/// Compared by what was written, not by the compiled matcher.
///
/// `globset::GlobMatcher` is not `Eq` — two matchers built from one pattern are
/// distinct values — so equality is on the pattern text, which is what a reader
/// means by "the same rule" anyway.
impl PartialEq for Spec {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Spec::All, Spec::All) => true,
            (Spec::Domain(a), Spec::Domain(b)) => a == b,
            (Spec::Command(a, _), Spec::Command(b, _)) => a == b,
            (Spec::Path(a, _), Spec::Path(b, _)) => a == b,
            (Spec::Unsupported(a), Spec::Unsupported(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Spec {}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.spec {
            Spec::All => write!(f, "{}", self.tool),
            Spec::Domain(d) => write!(f, "{}(domain:{})", self.tool, d.raw),
            Spec::Command(raw, _) => write!(f, "{}({raw})", self.tool),
            Spec::Path(raw, _) => write!(f, "{}({raw})", self.tool),
            Spec::Unsupported(s) => write!(f, "{}({s})", self.tool),
        }
    }
}

impl Rule {
    /// The rule that grants one host to one tool — what the `[r]emember` answer
    /// writes.
    pub fn domain(tool: &str, host: &str) -> Self {
        Self {
            tool: tool.to_string(),
            // Constructed from a `NetworkTarget::host`, which is already
            // lowercased and stripped of its trailing dot, so it round-trips
            // through `parse` unchanged.
            spec: Spec::Domain(DomainPattern::literal(host)),
        }
    }

    /// The rule that grants every call to one tool — what `[t]rust` writes.
    pub fn every_call(tool: &str) -> Self {
        Self {
            tool: tool.to_string(),
            spec: Spec::All,
        }
    }

    /// `Tool` or `Tool(specifier)`, or an error naming what is wrong with it.
    ///
    /// **Everything that is not a clean parse is an error, never a rule that
    /// matches nothing.** The distinction matters in the `deny` list: a rule the
    /// operator wrote and Emma quietly dropped is a protection they believe they
    /// have. `Spec::Unsupported` is the *other* half of that promise — a
    /// specifier that is well-formed but outside this build's vocabulary is kept
    /// as a value so the caller can announce it.
    pub fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            bail!("a permission rule cannot be empty");
        }
        let Some(open) = raw.find('(') else {
            return Ok(Self {
                tool: check_tool(raw)?,
                spec: Spec::All,
            });
        };
        let (tool, rest) = raw.split_at(open);
        let tool = check_tool(tool)?;
        let inner = rest
            .strip_prefix('(')
            .and_then(|r| r.strip_suffix(')'))
            .with_context(|| format!("`{raw}` is missing its closing `)`"))?;
        let inner = inner.trim();
        // Claude Code documents `Bash(*)` as equivalent to `Bash`. Reading them
        // as two things would make the shorter spelling the safe one by
        // accident.
        if inner.is_empty() || inner == "*" {
            return Ok(Self {
                tool,
                spec: Spec::All,
            });
        }
        if let Some(pattern) = inner.strip_prefix("domain:") {
            let pattern = pattern.trim();
            if pattern.is_empty() {
                bail!("`{raw}` names no domain — a `domain:` rule with nothing after the colon");
            }
            // `domain:*` is documented as equivalent to the bare tool name. It
            // is written back as `Spec::All` rather than kept as a pattern that
            // happens to match everything, so there is one shape in the code
            // that means "everything" and it is easy to grep for.
            let spec = match pattern {
                "*" => Spec::All,
                _ => Spec::Domain(DomainPattern::parse(pattern)?),
            };
            return Ok(Self { tool, spec });
        }
        // **A path-shaped specifier is decided by the tool, and by nothing
        // else.** The first version asked whether the text held a separator or
        // a glob character, which made `Read(Cargo.toml)` a *command* prefix —
        // matched against a `command` argument that a `Read` call does not
        // have, so it matched nothing at all, silently. A review supplied it as
        // a working bypass of a deny rule naming one file.
        //
        // **The fix for that kept the punctuation test as an extra disjunct for
        // `Bash`, and thereby rebuilt the same defect facing the other way.**
        // `Bash(rm -rf /)`, `Bash(curl https://*)`, `Bash(./deploy.sh)` — every
        // one contains a separator, so every one compiled to a `Spec::Path`
        // glob, and a `Bash` call carries `command` rather than `file_path`, so
        // `path_of` returned `None` and the rule could never fire. Two
        // independent reviewers found it on the same afternoon; one measured the
        // owner's real `settings.json` and reported **24 of 53 `Bash` rules
        // silently inert**, deny rules among them. It was worse than the state
        // it replaced: `Spec::Unsupported` is announced at boot, `Spec::Path` is
        // not, so the rules went from loud-and-inert to silent-and-inert.
        //
        // Claude Code has no path form for `Bash` — the inner text is a command,
        // always. So the tool decides alone, with no punctuation clause to grow
        // an exception back into.
        if tool != "Bash" {
            // **A backslash in a path rule means two different things on two
            // platforms, so it means nothing here.** `globset` escapes with it
            // on unix and treats it as a separator on Windows — so
            // `Read(src/a\*b)` names a literal asterisk on one machine and the
            // directory `src/a` on the other, from the same settings file. A
            // cross-model review found the first half of that and reported it
            // as a plain bug; it is worse than a bug, because either reading is
            // defensible and the file cannot say which it meant. Refused, with
            // the fix in the message, rather than resolved by guessing.
            if inner.contains('\\') {
                bail!(
                    "`{raw}` contains a backslash, which means an escape on unix and a separator on Windows — write the path with `/`"
                );
            }
            // A `..` cannot be matched before the path is resolved, and a rule
            // that can never be evaluated is refused loudly rather than kept as
            // something that quietly matches nothing.
            if inner.split(['/', '\\']).any(|seg| seg == "..") {
                bail!(
                    "`{raw}` contains `..`, which cannot be matched before the path is resolved — write the path without it"
                );
            }
            // The compiled pattern is normalised and the raw text is kept for
            // display, so a rule reads back exactly as written while matching
            // the same file however the call spells it.
            // Case-insensitive on Windows because the filesystem is: a rule
            // naming `src/main.rs` and a call naming `SRC/Main.rs` are one file
            // there, and a case-sensitive matcher is one the operating system
            // disagrees with. Done in the builder rather than by folding the
            // pattern, which would rewrite a `[A-Z]` class into a different one.
            if let Ok(g) = globset::GlobBuilder::new(&normalise_pattern(inner))
                .case_insensitive(cfg!(windows))
                .build()
            {
                return Ok(Self {
                    tool,
                    spec: Spec::Path(inner.to_string(), g.compile_matcher()),
                });
            }
        }
        // A command prefix. `git *`, `npm run test:*`, and the bare `git` form
        // that people write meaning the same thing.
        let prefix = inner
            .strip_suffix(":*")
            .or_else(|| inner.strip_suffix('*'))
            .unwrap_or(inner);
        let prefix = normalise_command(prefix);
        if !prefix.is_empty() {
            return Ok(Self {
                tool,
                spec: Spec::Command(inner.to_string(), prefix),
            });
        }
        Ok(Self {
            tool,
            spec: Spec::Unsupported(inner.to_string()),
        })
    }
}

/// Collapse runs of whitespace so `git   log` and `git log` are one command.
///
/// Not a shell parser, and deliberately not: quoting, substitution and operator
/// precedence are the shell's business, and a half-parser here would produce
/// rules whose meaning differs from what the shell does with the same string —
/// which is a worse failure than a rule that is simply literal.
/// Whether a command is composed of more than one thing, and therefore cannot
/// be judged by a prefix.
///
/// Deliberately over-broad. A false positive costs a question the human answers;
/// a false negative is a deny rule that did not fire. Those are not the same
/// size of mistake, and the check is tuned accordingly.
fn is_composed(command: &str) -> bool {
    // Chaining and piping.
    //
    // **A newline is a command separator and was missing from this list**, and
    // it was the worst of the three misses found in one review because the
    // evidence of it was being destroyed before this function ran. `command_of`
    // calls `normalise_command`, which is `split_whitespace().join(" ")`, so
    // `echo hi\ngit push` arrived here as `echo hi git push`: first word
    // `echo`, not composed, and a `Bash(git *)` deny never fired. The call site
    // now passes the raw argument; the token is here as well, because a
    // function called `is_composed` must be right about its own input rather
    // than right about one caller's.
    if ["&&", "||", ";", "|", "&", "\n", "\r"]
        .iter()
        .any(|t| command.contains(t))
    {
        return true;
    }
    // Substitution and grouping.
    //
    // `(` and `{` join `$(` because `(git push)` and `{ git push; }` are
    // ordinary shell, and the first word of each is `(git` or `{`, which no
    // prefix rule reads. The cost is a question on a command containing a
    // bracket for some other reason -- a `--format` string, a quoted message --
    // and only when a `deny` prefix rule for this tool already exists. That is
    // the trade this function's own doc chooses: a false positive costs a
    // question, a false negative is a deny rule that did not fire.
    if command.contains("$(")
        || command.contains('`')
        || command.contains('(')
        || command.contains('{')
    {
        return true;
    }
    // A leading `VAR=value` or a wrapper that swallows the real command.
    let first = command.split_whitespace().next().unwrap_or_default();
    if first.contains('=') {
        return true;
    }
    // A path-qualified binary. `/usr/bin/git push` and `.\tools\git.exe push`
    // are plainly git, and a prefix rule reading `git` does not match either —
    // found by a second adversarial pass, and an ordinary spelling rather than
    // an exotic one. The same reasoning as the wrappers below: the first word
    // is not the command name the rule is written against, so a prefix cannot
    // judge it.
    if first.contains('/') || first.contains('\\') {
        return true;
    }
    // Wrappers that swallow the real command, so the first word is not the
    // program a rule is written against.
    //
    // **`command` was the third miss.** It is a POSIX builtin in exactly the
    // class of `env` and `xargs` -- `command git push` runs git -- and it was
    // absent, so a `Bash(git *)` deny did not fire and nothing was said. The
    // others in the second row are the same shape and were added at the same
    // time rather than one review at a time.
    matches!(
        first,
        "env"
            | "sh"
            | "bash"
            | "zsh"
            | "cmd"
            | "powershell"
            | "pwsh"
            | "nice"
            | "time"
            | "xargs"
            | "command"
            | "exec"
            | "sudo"
            | "doas"
            | "su"
            | "nohup"
            | "timeout"
            | "setsid"
            | "stdbuf"
            | "ionice"
            | "busybox"
    )
}

/// The command a call would run, if it names one.
fn command_of(args: &Value) -> Option<String> {
    args.get("command")
        .and_then(Value::as_str)
        .map(normalise_command)
}

/// One spelling for a path, so a rule and a call can be compared at all.
///
/// **This is a matcher, not a filesystem.** `tools/fs/src/path.rs` resolves
/// `./src/x` and `src/x` to the same file long after the gate has decided, so
/// until this existed the two were one file to the tool and two strings to the
/// rule — a deny written one way and a call spelled the other way passed in
/// silence.
///
/// What it does is lexical and small: separators become `/`, repeated
/// separators collapse, and `./` segments go. On Windows two more aliases are
/// folded, because the filesystem itself folds them and a matcher that does not
/// is a matcher the operating system disagrees with: the trailing dots and
/// spaces that `CreateFile` strips before it ever reaches the disk. Case is
/// folded too, but by the glob itself rather than here — lowercasing a
/// *pattern* would quietly rewrite a `[A-Z]` class into something else.
/// A cross-model review supplied `crates/emma/src/permissions.rs.` and
/// `CRATES/emma/...` as working spellings of a file a rule named exactly.
///
/// What it deliberately does **not** do is resolve `..`, because `a/link/../b`
/// is `a/b` only when `link` is a directory, and a matcher that guesses wrong
/// about that gets a deny rule wrong in whichever direction the guess fell.
/// `..` is handled by refusing to judge instead — see `path_unjudgeable`.
fn normalise_path(p: &str) -> String {
    let unified = p.replace('\\', "/");
    let leading = if is_absolute(&unified) { "/" } else { "" };
    let mut body: Vec<String> = unified
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != ".")
        .map(str::to_string)
        .collect();
    if cfg!(windows) {
        for seg in &mut body {
            // Trailing dots and spaces are not part of the name Windows opens.
            let trimmed = seg.trim_end_matches(['.', ' ']);
            // ...except when trimming would leave nothing: `..` is a real
            // component this function must hand on untouched for
            // `path_unjudgeable` to see.
            if !trimmed.is_empty() {
                *seg = trimmed.to_string();
            }
        }
        // A drive letter is part of the root, not a component to be matched.
        if let Some(first) = body.first() {
            if is_drive(first) {
                body.remove(0);
            }
        }
    }
    format!("{leading}{}", body.join("/"))
}

/// The same folding for a rule's **pattern**, which is a glob rather than a path.
///
/// **A pattern has no backslashes to fold**, because `Rule::parse` refuses one:
/// `globset` reads it as an escape on unix and as a separator on Windows, so
/// one settings file would mean two different things on two machines. A path
/// arriving from a call is the opposite case — it comes from the operating
/// system, where a backslash is unambiguously a separator — which is why
/// `normalise_path` folds and this does not.
fn normalise_pattern(pattern: &str) -> String {
    let leading = if is_absolute(pattern) { "/" } else { "" };
    let mut body: Vec<String> = pattern
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != ".")
        .map(str::to_string)
        .collect();
    if cfg!(windows) {
        if let Some(first) = body.first() {
            if is_drive(first) {
                body.remove(0);
            }
        }
    }
    format!("{leading}{}", body.join("/"))
}

/// Whether a spelling names a root rather than something relative to one.
///
/// Three shapes, not one. A leading separator is the unix form; `C:/x` is the
/// Windows form and was missed by the first version of this check, so an
/// absolute Windows path compared as if it were relative and matched nothing;
/// `//server/share` is a UNC root, which has no drive letter at all.
fn is_absolute(p: &str) -> bool {
    p.starts_with('/')
        || p.starts_with("\\")
        || is_drive(p.split(['/', '\\']).next().unwrap_or_default())
}

/// `C:` and friends — a drive designator, not a path component.
fn is_drive(seg: &str) -> bool {
    let b = seg.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Whether a rule pattern and a call's path cannot honestly be compared.
///
/// Both cases are about the **call**, never about the rule. An earlier version
/// asked the same question of the pattern, and a review showed what that costs:
/// one deny rule containing `..` turned every unrelated read into a prompt,
/// because the pattern was unjudgeable regardless of what was being read. A
/// gate that asks about everything is a gate nobody reads. A pattern that
/// cannot be evaluated is refused at parse time and said out loud, which is
/// where an unusable rule belongs.
///
/// - **A `..` component in the path.** Resolving it needs to know what is a
///   directory and what is a link, which the gate cannot know before the tool
///   runs.
/// - **Disagreeing absoluteness.** A rule reading `src/**` and a call naming
///   `/home/me/project/src/x` may well be the same file, and the gate does not
///   hold the root that would settle it. Comparing them as strings answers a
///   question nobody asked.
fn path_unjudgeable(pattern: &str, path: &str) -> bool {
    if path.split('/').any(|seg| seg == "..") {
        return true;
    }
    pattern.starts_with('/') != path.starts_with('/')
}

/// The path a call would touch, if it names one.
///
/// Both spellings, because the tools use both: `file_path` for the ones that
/// address a single file, `path` for the ones that address a base.
fn path_of(args: &Value) -> Option<String> {
    ["file_path", "path"]
        .iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
        .map(normalise_path)
}

/// Whether a command falls under a prefix rule.
///
/// **Word-boundary, not raw string prefix.** `git` must not match `github-cli`,
/// which a bare `starts_with` would do — and in a deny list that is the
/// difference between blocking what was written and blocking something the
/// operator never named. Either the command equals the prefix, or it continues
/// with a space.
fn command_matches(prefix: &str, command: &str) -> bool {
    let prefix = normalise_program(prefix);
    let command = normalise_program(command);
    command == prefix
        || (command.starts_with(&prefix) && command.as_bytes().get(prefix.len()) == Some(&b' '))
}

/// Fold the spellings of a program name that name one program.
///
/// **Three ordinary Windows spellings walked past a `Bash(git *)` deny, and
/// none of them was listed as a known residual.** A reviewer measured them:
/// `git push` denied; `git.exe push`, `GIT push` and `"git" push` all
/// unmatched, with no `Ask` and no boot note. `is_composed`'s own doc says
/// *"a false negative is a deny rule that did not fire"*, and these are three.
///
/// - **`.exe` is the spelling on this platform.** A model writing the full
///   filename is writing the ordinary thing, not evading anything.
/// - **Case.** Windows program lookup is case-insensitive, so `GIT push` runs.
///   The *path* matcher was made `case_insensitive(cfg!(windows))` for exactly
///   this reason and the command matcher was left alone — one file, two
///   answers to the same question about the same operating system.
/// - **A leading quote** is ordinary shell quoting, and `is_composed` does not
///   consider it, so it reached here as part of the program name.
///
/// Only the first token is touched. Everything after it is the command's
/// arguments, where folding case would make `git commit -m Fix` and
/// `git commit -m fix` the same rule — which they are not.
fn normalise_program(command: &str) -> String {
    let (head, rest) = match command.split_once(' ') {
        Some((h, r)) => (h, Some(r)),
        None => (command, None),
    };
    // **Every quote, not just the ones at the ends.** `trim_matches` stripped a
    // leading and trailing quote, so `"git" push` folded and `gi"t" push` did
    // not — and the second is equally ordinary shell, equally runs `git`, and
    // reached `command_matches` as the literal program name `gi"t`. A reviewer
    // measured it evading `deny Bash(git *)` with no Deny and no Ask, because
    // `is_composed` has no quote branch either. Removing them all is what the
    // shell does to a program name before the OS ever sees it.
    let head: String = head.chars().filter(|c| *c != '"' && *c != '\'').collect();
    let head = head.as_str();
    // Windows only. On unix `git` and `GIT` are two programs and folding them
    // would be inventing a match the operating system does not make.
    let mut head = if cfg!(windows) {
        head.to_ascii_lowercase()
    } else {
        head.to_string()
    };
    if cfg!(windows) {
        for ext in [".exe", ".cmd", ".bat", ".com"] {
            if let Some(stem) = head.strip_suffix(ext) {
                head = stem.to_string();
                break;
            }
        }
    }
    match rest {
        Some(r) => format!("{head} {r}"),
        None => head,
    }
}

fn normalise_command(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn check_tool(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a permission rule needs a tool name before the `(`");
    }
    if name.contains('*') {
        bail!(
            "`{name}` globs the tool name. Emma matches tool names exactly — a `*` in that \
             position is the rule that matches everything, which is the one mistake this \
             file cannot afford"
        );
    }
    if name.contains(char::is_whitespace) {
        bail!("`{name}` is not a tool name");
    }
    Ok(name.to_string())
}

// endregion: What a rule is

// region: Matching a host
// ---------------------------------------------------------------------------
// Matching a host
//
// The whole security surface of this file. Label-wise and length-checked, never
// substring: every near-miss below is a host somebody can register.
// ---------------------------------------------------------------------------

/// A `domain:` pattern, kept beside the text it was written as so the rule can
/// be rendered back exactly as the user will see it in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainPattern {
    raw: String,
    /// `*.example.com` — matches a strict subdomain at any depth. Split out at
    /// parse time because it is the one form whose label count is not fixed, and
    /// mixing it into the general path is how "any depth" becomes "any host".
    any_subdomain: bool,
    /// The labels, right to left is irrelevant — compared positionally against a
    /// host with the *same* number of labels, which is the length check that
    /// stops `example.com` from matching `example.com.attacker.net`.
    labels: Vec<String>,
}

impl DomainPattern {
    fn literal(host: &str) -> Self {
        Self {
            raw: host.to_string(),
            any_subdomain: false,
            labels: host.split('.').map(str::to_string).collect(),
        }
    }

    fn parse(pattern: &str) -> Result<Self> {
        let normalised = normalise(pattern);
        if normalised.is_empty() {
            bail!("`domain:{pattern}` is not a host");
        }
        let (any_subdomain, rest) = match normalised.strip_prefix("*.") {
            Some(rest) => (true, rest.to_string()),
            None => (false, normalised),
        };
        if rest.is_empty() || rest.split('.').any(str::is_empty) {
            bail!("`domain:{pattern}` has an empty label");
        }
        Ok(Self {
            raw: pattern.trim().to_string(),
            any_subdomain,
            labels: rest.split('.').map(str::to_string).collect(),
        })
    }

    /// Does this pattern permit that host?
    ///
    /// The host arrives from [`emma_tool_api::NetworkTarget`], already
    /// lowercased and stripped of a trailing dot; it is normalised again anyway,
    /// because a matcher that is only correct for one caller is a matcher with a
    /// hole waiting for the second one.
    pub fn matches(&self, host: &str) -> bool {
        let host = normalise(host);
        let labels: Vec<&str> = host.split('.').collect();
        if labels.iter().any(|l| l.is_empty()) {
            return false;
        }
        if self.any_subdomain {
            // Strict: `*.example.com` covers `api.example.com` and
            // `a.b.example.com` and **not** `example.com`. There has to be at
            // least one label in front of the suffix, or the pattern quietly
            // becomes a longer spelling of the bare host.
            if labels.len() <= self.labels.len() {
                return false;
            }
            let tail = &labels[labels.len() - self.labels.len()..];
            return self.label_wise(tail);
        }
        // The length check. Without it, `example.com` would have to be matched
        // by suffix or by substring, and both of those match
        // `example.com.attacker.net`.
        labels.len() == self.labels.len() && self.label_wise(&labels)
    }

    fn label_wise(&self, host: &[&str]) -> bool {
        self.labels
            .iter()
            .zip(host)
            .all(|(pattern, label)| label_matches(pattern, label))
    }
}

/// One label against one pattern label. `*` inside a label matches any run of
/// characters **within that label**, which is what keeps `example.*` from
/// matching `example.evil.com`: the host is split on dots before it ever gets
/// here, so no pattern can cross one.
fn label_matches(pattern: &str, label: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == label;
    }
    // A tiny glob rather than a regex: the parts between the stars must appear
    // in order, the first anchored at the start and the last at the end.
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = label;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            let Some(stripped) = rest.strip_prefix(part) else {
                return false;
            };
            rest = stripped;
            continue;
        }
        if i == parts.len() - 1 {
            return rest.len() >= part.len() && rest.ends_with(part);
        }
        let Some(at) = rest.find(part) else {
            return false;
        };
        rest = &rest[at + part.len()..];
    }
    true
}

/// The same normalisation [`emma_tool_api::NetworkTarget::new`] applies, so a
/// rule and a target agree on what one host is.
fn normalise(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Why a well-formed `domain:` rule will never meet a real host, when that is
/// true of it.
///
/// **Two spellings a person writes and a URL never produces.** A host reaches
/// [`Rules::for_egress`] from `Url::host_str`, which has already applied IDNA
/// and already bracketed an IPv6 literal. So `domain:bücher.example` and
/// `domain:::1` parse cleanly, match nothing any fetch can present, and — in a
/// `deny` list — are a protection the operator believes they have. Exactly the
/// `Bash(rm *)` shape, and it gets the same answer: speech, at boot, in the
/// register the list deserves.
///
/// **Speech and not conversion**, decided rather than defaulted. Converting the
/// pattern would move matching — the whole security surface of this file — from
/// "compare ASCII labels" to "run UTS-46 over attacker-influencable strings",
/// where a bug can *widen* a match; wildcards are not valid IDNA input and would
/// need a label-skipping policy nobody has asked for; and the rule stays live
/// for a caller that hands the matcher a Unicode host directly, which the
/// matrix pins. A warning cannot widen anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inert {
    /// Non-ASCII: real hosts arrive punycode-encoded.
    Unicode,
    /// A colon outside brackets: `Url::host_str` writes IPv6 as `[::1]`, and
    /// the brackets are part of the label this matcher compares.
    UnbracketedColon,
}

impl Inert {
    fn of(pattern: &str) -> Option<Self> {
        if !pattern.is_ascii() {
            return Some(Self::Unicode);
        }
        if pattern.contains(':') && !pattern.starts_with('[') {
            return Some(Self::UnbracketedColon);
        }
        None
    }

    /// The clause explaining what this build will actually do with the rule.
    fn because(self) -> &'static str {
        match self {
            Self::Unicode => {
                "hosts reach this matcher already punycode-encoded, and a domain written in \
                 Unicode never meets one"
            }
            Self::UnbracketedColon => {
                "an IPv6 host arrives with its brackets, which are part of the name this \
                 matcher compares, and an unbracketed rule never meets one"
            }
        }
    }

    /// How the operator should have spelled it, or `None` when this build
    /// cannot say — in which case the note offers nothing rather than
    /// inventing a rewrite that would not work either.
    fn rewrite(self, pattern: &str) -> Option<String> {
        match self {
            Self::UnbracketedColon => Some(format!("[{pattern}]")),
            // Label-wise, because a `domain:` pattern is not a domain: it may
            // carry `*` in any label, and `*` is not valid IDNA input. An
            // ASCII label is already its own answer, a wildcard label cannot be
            // converted, and anything `idna` refuses ends the attempt — the
            // do-not-fabricate rule applies to warnings too, and a
            // hand-mangled suggestion is how `xn--bcher-kva` becomes
            // `xn--bucher-kva`.
            Self::Unicode => {
                let mut out = Vec::new();
                for label in pattern.split('.') {
                    if label.is_ascii() {
                        out.push(label.to_string());
                    } else if label.contains('*') {
                        return None;
                    } else {
                        let ascii = idna::domain_to_ascii(label).ok()?;
                        if ascii.is_empty() || !ascii.is_ascii() {
                            return None;
                        }
                        out.push(ascii);
                    }
                }
                Some(out.join("."))
            }
        }
    }
}

/// The boot note for one such rule, in the register its list deserves.
///
/// Two sentences for the same reason [`Rules::parse`]'s unsupported branch has
/// two: an `allow` that evaluates to nothing costs a prompt, a `deny` that
/// evaluates to nothing costs a protection.
fn inert_note(source: &Path, rule: &Rule, kind: PermissionKind, why: Inert) -> String {
    let source = source.display();
    let fix = match why.rewrite(match &rule.spec {
        Spec::Domain(d) => &d.raw,
        _ => "",
    }) {
        Some(ascii) => format!(" Write `{}(domain:{ascii})` instead.", rule.tool),
        None => String::new(),
    };
    match kind {
        PermissionKind::Deny | PermissionKind::Ask => format!(
            "{source}: `{rule}` is a {} rule that matches no real call — {}, so it blocks \
             NOTHING.{fix}",
            kind.word(),
            why.because(),
        ),
        PermissionKind::Allow => format!(
            "{source}: `{rule}` is an allow rule that matches no real call — {}, so it \
             approves nothing and those calls will still ask.{fix}",
            why.because(),
        ),
    }
}

// endregion: Matching a host

// region: The rule set, and the order it is read in
// ---------------------------------------------------------------------------
// The rule set, and the order it is read in
//
// Three lists and one function that consults them in a fixed order. The order
// is the design, exactly as it is in `approval.rs`, and it is stated there as
// well because that is where it is applied.
// ---------------------------------------------------------------------------

/// What the rules say about one call, when they say anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Refuse. Outranks everything below it and both the bypass and a human.
    Deny,
    /// Ask anyway — even for a read, even for an exempt tool, even when an
    /// `allow` rule also matches, and even when this process already has a
    /// session grant. The escape hatch for "I allowed this tool but not today".
    Ask,
    /// Run without asking.
    Allow,
}

/// Every rule this run is operating under, already parsed.
#[derive(Debug, Default, Clone)]
pub struct Rules {
    deny: Vec<Rule>,
    ask: Vec<Rule>,
    allow: Vec<Rule>,
}

impl Rules {
    /// Parse a set of entries, returning the rules and one sentence per entry
    /// that will not do anything.
    ///
    /// **Never fails the boot**, and that is a considered position rather than
    /// laziness. `.claude/settings.json` is a file written for another program
    /// with a larger vocabulary; refusing to start because one of forty rules is
    /// `Read(./secrets/**)` would be the outage this workspace keeps ruling
    /// against (`harness/src/claude.rs`, and `ClaudeFront` before it). The
    /// alternative failure — a dropped rule nobody hears about — is what the
    /// notes are for, and `main` prints every one of them at startup.
    pub fn parse(entries: &[PermissionEntry]) -> (Self, Vec<String>) {
        let mut rules = Self::default();
        let mut notes = Vec::new();
        for entry in entries {
            let source = entry.source.display();
            let rule = match Rule::parse(&entry.rule) {
                Ok(rule) => rule,
                Err(e) => {
                    notes.push(format!("{source}: {e:#} — that rule does nothing"));
                    continue;
                }
            };
            if let Spec::Unsupported(spec) = &rule.spec {
                // Two sentences, because the two lists fail in opposite
                // directions. An `allow` Emma cannot evaluate costs a prompt the
                // user did not want; a `deny` it cannot evaluate costs them a
                // protection they think they have, and that one is worth
                // alarming prose.
                notes.push(match entry.kind {
                    PermissionKind::Deny | PermissionKind::Ask => format!(
                        "{source}: `{}` is a {} rule Emma cannot evaluate — `{spec}` is not a \
                         specifier this build understands, so it blocks NOTHING. Emma matches \
                         a bare tool name and `domain:`; write `{}` to cover every call.",
                        rule,
                        entry.kind.word(),
                        rule.tool
                    ),
                    PermissionKind::Allow => format!(
                        "{source}: `{rule}` is an allow rule Emma cannot evaluate — `{spec}` is \
                         not a specifier this build understands, so it approves nothing and \
                         those calls will still ask."
                    ),
                });
                continue;
            }
            // A rule this build parsed, kept, and will never match against
            // anything a real fetch presents. Announced and then **kept**: it
            // still answers for a caller that hands `for_egress` a Unicode
            // host directly, and demoting it would be claiming more inertness
            // than is true.
            if let Spec::Domain(pattern) = &rule.spec {
                if let Some(why) = Inert::of(&pattern.raw) {
                    notes.push(inert_note(&entry.source, &rule, entry.kind, why));
                }
            }
            match entry.kind {
                PermissionKind::Deny => rules.deny.push(rule),
                PermissionKind::Ask => rules.ask.push(rule),
                PermissionKind::Allow => rules.allow.push(rule),
            }
        }
        (rules, notes)
    }

    /// Rules that parsed, were kept, and cannot match anything **this run** offers.
    ///
    /// Run after the registry exists, which is why it is not part of [`Rules::parse`]:
    /// the tool surface is chosen later than the rules are read, and neither of the
    /// two mistakes below can be seen without it.
    ///
    /// **Deliberately narrow.** A rule naming a tool a persona filtered out is
    /// correctly inert and says nothing here — the operator asked for that. Only two
    /// shapes are reported, and both are unambiguously a mistake rather than a
    /// choice:
    ///
    /// 1. **A tool name that differs from a real one only by case.** Matching is
    ///    exact, so `deny: ["bash"]` never touches `Bash`. In a deny list that is a
    ///    protection the operator believes they have.
    /// 2. **A `domain:` specifier on a tool that never reaches the network.** The
    ///    egress gate answers `Allow` before consulting rules for anything with
    ///    `reaches_network: false`, so `Bash(domain:evil.com)` matches nothing — and
    ///    `Bash` in particular *can* reach the network, by running `curl`, which is
    ///    exactly what makes the rule look like it is doing something.
    ///
    /// The `Unicode` and unbracketed-colon cases are announced at parse time
    /// already; this covers the two that needed the registry.
    pub fn unmatchable_here(
        entries: &[PermissionEntry],
        known: &[&'static str],
        reaches_network: &[&'static str],
    ) -> Vec<String> {
        let mut notes = Vec::new();
        for entry in entries {
            let Ok(rule) = Rule::parse(&entry.rule) else {
                continue;
            };
            let named = rule.tool.as_str();
            if !known.contains(&named) {
                if let Some(real) = known.iter().find(|k| k.eq_ignore_ascii_case(named)) {
                    notes.push(format!(
                        "{}: `{}` names `{named}`, and the tool is spelled `{real}`. Tool names \
                     match exactly, so this rule can never fire.",
                        entry.source.display(),
                        entry.rule
                    ));
                } else {
                    // **The other half of this row's own title, and it fell out
                    // of the `if` in silence.** Only the case-variant branch
                    // said anything, so `deny("Bahs")` produced nothing at all —
                    // a rule the operator believes is protecting them, matching
                    // no tool that exists.
                    //
                    // The argument for the silence was that a persona may have
                    // filtered a real tool out of this run, so an unknown name
                    // is not necessarily a typo. True, and it does not follow
                    // that nothing should be said: the note below states both
                    // readings rather than choosing one, which is what an
                    // operator needs in order to tell which it is.
                    notes.push(format!(
                        "{}: `{}` names `{named}`, which is not a tool in this run. Either it \
                     is a typo, or a persona filtered that tool out — in both cases the rule \
                     matches nothing here.",
                        entry.source.display(),
                        entry.rule
                    ));
                }
                continue;
            }
            // **A prefix that ends mid-word can never fire, and that was
            // silent too.** `command_matches` is a word-boundary test on
            // purpose — `git` must not match `github-cli`, which in a deny list
            // is the difference between blocking what was written and blocking
            // something nobody named. So `Bash(curl https://*)` strips its `*`,
            // leaves a prefix ending in `/`, and matches no command ever
            // written. The matching is right; the silence was not, and this is
            // `DEF-015`'s class in a shape `DEF-015` did not cover.
            if let Spec::Command(raw, prefix) = &rule.spec {
                let star_is_a_wildcard =
                    raw.ends_with(":*") || raw.ends_with(" *") || !raw.ends_with('*');
                // **A rule with no star is not inert and must not be
                // announced as inert. This was got wrong for half a day.**
                //
                // A reviewer observed that `Bash(curl https://*)` is announced
                // and `Bash(curl https://)` is not, called the second
                // "identically inert", and the check was widened to any prefix
                // ending `/`, `:`, `=` or `-`. That was accepted too readily.
                //
                // The premise is false. `command_matches` matches on equality
                // OR on the prefix followed by a space, so a prefix always
                // matches at least one command: itself. `Bash(curl https://)`
                // exactly matches the command `curl https://` -- useless in
                // practice, and not inert.
                //
                // And the widening could not tell that case from
                // `Bash(rm -rf /)`, which has no star, ends in `/`, and is a
                // complete command somebody meant exactly as written. A second
                // reviewer printed the two together: the rule denied `rm -rf /`
                // AND was announced at every boot as unable to match. The
                // counterexample was already in this file's own test module,
                // `a_bash_deny_containing_a_slash_still_denies`.
                //
                // Telling an operator that a live protection is dead is how a
                // live protection gets deleted, and the note's own remedy
                // ("write `rm -rf / *`") strips to the same prefix and changes
                // nothing -- so following it teaches them the tool lied twice.
                // The wrong note is worse than the missing one, so the no-star
                // case is silent again. Distinguishing "useless exact rule"
                // from "intended exact rule" needs the author's intent, which
                // is not in the string.
                // **This said "it can never match", and that was false.**
                //
                // Third pass over this branch, and the first two were wrong in
                // the same direction. The argument that silenced the no-star
                // case is written eleven lines above: `command_matches` tries
                // equality before the word-boundary test, so a non-empty prefix
                // always matches at least one command — itself. That is exactly
                // as true of `Bash(sudo*)`, whose star the parser consumed,
                // leaving the prefix `sudo`. It denies the command `sudo`.
                //
                // A reviewer reproduced it against the release binary. The cost
                // of the wrong sentence is not a wasted minute: it appears in
                // the output of the command whose job is "why can it not do X",
                // it says a deny rule is inert, and the obvious next move is to
                // delete the rule. Telling an operator a live protection is dead
                // is how a live protection gets deleted.
                //
                // The surprise is real and worth keeping — `sudo*` looks like a
                // wildcard and is not — so the note stays and states what is
                // true: it matches that one command and nothing after it. The
                // remedy it already gave was right all along.
                if !star_is_a_wildcard && !prefix.is_empty() {
                    notes.push(format!(
                        "{}: `{}` matches only the exact command `{prefix}`, and not \
                     `{prefix}` followed by anything — the `*` is consumed as part of \
                     the prefix rather than treated as a wildcard, and commands match \
                     at a word boundary. If that is what you meant, it is doing it. If \
                     you meant `{prefix}` with arguments, write `{prefix} *`, or name \
                     the whole first word and use `:*` for the arguments.",
                        entry.source.display(),
                        entry.rule
                    ));
                }
            }
            if matches!(rule.spec, Spec::Domain(_)) && !reaches_network.contains(&named) {
                notes.push(format!(
                    "{}: `{}` puts a `domain:` specifier on `{named}`, which does not declare \
                 that it reaches the network. The egress gate answers before \
                 consulting rules for such a tool, so this rule matches nothing. \
                 If the worry is `{named}` reaching out by other means, a bare \
                 `{named}` rule is the one that bites.",
                    entry.source.display(),
                    entry.rule
                ));
            }
        }
        notes
    }

    pub fn is_empty(&self) -> bool {
        self.deny.is_empty() && self.ask.is_empty() && self.allow.is_empty()
    }

    /// Add a rule this run just wrote to disk, so it applies to the very next
    /// call rather than to the next process. Without this, "yes, and remember"
    /// would still prompt again five minutes later and the user would reasonably
    /// conclude it had not worked.
    pub fn adopt(&mut self, rule: Rule) {
        self.allow.push(rule);
    }

    /// What the rules say about **the local-damage question** for one tool.
    ///
    /// Bare rules only. A `WebFetch(domain:x)` allow rule says one host is an
    /// acceptable destination; reading it as permission to *run* the tool
    /// generally would let a narrow grant answer a question nobody asked it.
    pub fn for_tool(&self, tool: &str) -> Option<Decision> {
        self.decide(|r| r.tool == tool && r.spec == Spec::All)
    }

    /// What the rules say about **this call**, arguments included.
    ///
    /// `for_tool` answers about a tool; this answers about one invocation of
    /// it, which is the question `Bash(git *)` and `Read(./src/**)` were
    /// written to ask. Until this existed both shapes parsed, were kept, were
    /// announced as unevaluable, and matched nothing — so a `deny` list copied
    /// from a Claude Code project protected only what it named baldly.
    ///
    /// A bare rule still counts, because `Bash` means every `Bash` call and a
    /// narrower rule cannot take that away — the ladder decides which list
    /// wins, not which rule is more specific.
    pub fn for_call(&self, tool: &str, args: &Value) -> Option<Decision> {
        // **A command a prefix rule cannot honestly be tested against does not
        // get to pass quietly.** `Spec::Command` matches argument text, and
        // `Bash` runs that text through a shell — so `env git push`,
        // `cd . && git push` and `GIT_SSH_COMMAND=x git push` all evade
        // `Bash(git *)` while plainly being git pushes. An adversarial review
        // found this after the matcher shipped, and it is a real hole rather
        // than a theoretical one: the whole point of a deny rule is that the
        // operator wrote down something they did not want run.
        //
        // The fix is not a shell parser. Quoting, substitution and operator
        // precedence are the shell's business, and a half-parser would give
        // rules a meaning that differs from what the shell actually does, which
        // is worse than a rule that is literal. What is done instead is
        // narrower and honest: when a **deny** rule for this tool carries a
        // command prefix, and the command is *composed* — chained, piped,
        // substituted, or prefixed with an environment assignment — the match
        // cannot be trusted either way, so the answer is `Ask` rather than
        // silence. The human is shown the command and decides.
        //
        // Deny rules only. An allow rule that fails to match already falls
        // through to a question, so there is nothing to protect there, and
        // widening this to allow rules would turn every `&&` into a prompt.
        // **A deny that matched outranks a refusal to judge.** Both preflights
        // below answer `Ask`, and `Ask` is weaker than `Deny` — so asking the
        // question before the ladder has run let a rule that plainly matched be
        // downgraded to a prompt. Two shapes of that reached a review: a bare
        // `Read` deny beside a narrow `Read(src/**)`, and a `Bash(git *)` deny
        // on a call that also carried a `file_path`. The ladder is consulted
        // first and its `Deny` is final; the preflights only speak when it did
        // not deny.
        let ladder = self.decide(|r| {
            r.tool == tool
                && match &r.spec {
                    Spec::All => true,
                    Spec::Command(_, prefix) => command_of(args)
                        .map(|c| command_matches(prefix, &c))
                        .unwrap_or(false),
                    Spec::Path(_, g) => path_of(args).map(|p| g.is_match(&p)).unwrap_or(false),
                    // A destination is a different question, answered by
                    // `for_egress`. A host grant is not permission to run.
                    Spec::Domain(_) => false,
                    Spec::Unsupported(_) => false,
                }
        });
        if ladder == Some(Decision::Deny) {
            return ladder;
        }

        // **The RAW argument, not the normalised one.** `command_of` collapses
        // whitespace so a prefix can be compared against it, and that collapse
        // turns a newline into a space -- destroying the one signal that says
        // two commands were sent as one. `echo hi\ngit push` reached
        // `is_composed` as `echo hi git push` and walked past a `Bash(git *)`
        // deny with no `Deny`, no `Ask` and no boot note.
        if let Some(raw) = args.get("command").and_then(Value::as_str) {
            let denies_by_prefix = self
                .deny
                .iter()
                .any(|r| r.tool == tool && matches!(r.spec, Spec::Command(..)));
            if denies_by_prefix && is_composed(raw) {
                return Some(Decision::Ask);
            }
        }
        // **The same reasoning for paths, and it was the half left open when
        // the command half landed.** A `deny` naming a path shape, a call whose
        // path cannot honestly be compared against it — a `..` in either, or
        // one absolute and the other relative — and no deny that actually
        // matched: that is not a miss, it is a question. `Ask`, not `Deny`,
        // because the rule genuinely did not match and saying otherwise would
        // be the same dishonesty in the other direction.
        if let Some(path) = path_of(args) {
            let denies_by_path: Vec<&str> = self
                .deny
                .iter()
                .filter(|r| r.tool == tool)
                .filter_map(|r| match &r.spec {
                    Spec::Path(raw, _) => Some(raw.as_str()),
                    _ => None,
                })
                .collect();
            if !denies_by_path.is_empty()
                && denies_by_path
                    .iter()
                    .any(|raw| path_unjudgeable(&normalise_pattern(raw), &path))
            {
                return Some(Decision::Ask);
            }
        }
        ladder
    }

    /// What the rules say about **the egress question** for one tool reaching
    /// one host. Bare rules for that tool count, and so do its `domain:` rules.
    pub fn for_egress(&self, tool: &str, host: &str) -> Option<Decision> {
        self.decide(|r| {
            r.tool == tool
                && match &r.spec {
                    Spec::All => true,
                    Spec::Domain(d) => d.matches(host),
                    // Neither shape says anything about a destination. A
                    // command prefix is about what runs; a path is about what
                    // is touched. Reading either as egress permission would let
                    // a narrow grant answer a question nobody asked it.
                    Spec::Command(..) | Spec::Path(..) => false,
                    Spec::Unsupported(_) => false,
                }
        })
    }

    /// **deny, then ask, then allow — the first list with a match wins, and rule
    /// specificity never changes that.** Same order Claude Code documents, and
    /// the reason is the same: a `deny` that could be narrowed away by a more
    /// specific `allow` is not a deny, it is a default.
    fn decide(&self, matches: impl Fn(&Rule) -> bool) -> Option<Decision> {
        if self.deny.iter().any(&matches) {
            return Some(Decision::Deny);
        }
        if self.ask.iter().any(&matches) {
            return Some(Decision::Ask);
        }
        if self.allow.iter().any(&matches) {
            return Some(Decision::Allow);
        }
        None
    }
}

// endregion: The rule set, and the order it is read in

// region: Writing one down
// ---------------------------------------------------------------------------
// Writing one down
//
// The half of this file that touches the user's disk. It merges, it refuses
// anything it cannot parse, and it never invents a shape — because this
// document belongs to another program too.
// ---------------------------------------------------------------------------

/// The list a decision is written to in a settings file.
fn list_key(d: Decision) -> &'static str {
    match d {
        Decision::Allow => "allow",
        Decision::Ask => "ask",
        Decision::Deny => "deny",
    }
}

/// Every **bare tool name** rule in `file`, as tool to decision.
///
/// Bare names only, and specifier rules are deliberately invisible here: a
/// `WebFetch(domain:docs.rs)` grant answers the egress question for one host
/// and says nothing about whether the tool may run, so folding it into a
/// three-state row would put a word on screen the file does not support.
/// deny beats ask beats allow, the order [`Rules::decide`] reads them in.
///
/// Any problem reading the file answers "nothing said", because this is what a
/// screen draws and [`set_bare_rule`] is where the refusals live.
pub fn bare_rules(file: &Path) -> std::collections::BTreeMap<String, Decision> {
    let Some(doc) = std::fs::read_to_string(file)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
    else {
        return Default::default();
    };
    let mut out: std::collections::BTreeMap<String, Decision> = Default::default();
    // Weakest first, so a stronger answer for the same tool overwrites it and
    // the result reads the way the gate will.
    for decision in [Decision::Allow, Decision::Ask, Decision::Deny] {
        let Some(list) = doc["permissions"][list_key(decision)].as_array() else {
            continue;
        };
        for entry in list.iter().filter_map(|v| v.as_str()) {
            if let Ok(rule) = Rule::parse(entry) {
                if rule.spec == Spec::All {
                    out.insert(rule.tool, decision);
                }
            }
        }
    }
    out
}

/// Put one tool's **bare-name** rule into `file`: `Some(decision)` writes it,
/// `None` removes it, and the file keeps everything else it had.
///
/// The merge discipline is [`remember`]'s, for the same reason: a file that is
/// not JSON, or not an object, or whose `permissions` is not an object, is
/// **not written to at all** and the caller is told why. Losing a grant is an
/// annoyance; losing somebody's `hooks` block is a defect they find weeks
/// later.
///
/// **The coexistence rule, which is the whole reason this is not a rewrite.**
/// Only entries that parse to `tool` with [`Spec::All`] are removed. A
/// hand-written `Bash(cargo *)` or `WebFetch(domain:docs.rs)` is never touched
/// by any state of this control, in any list. The two kinds of rule answer
/// different questions, so they stack rather than replace each other, and
/// where they meet [`Rules::decide`] settles it in one direction only: deny is
/// read before ask, ask before allow. A bare `Deny` therefore outranks a
/// narrower hand-written allow, which is what denying a tool has to mean; a
/// bare `Allow` does **not** erase a hand-written `Bash(cargo *)`, it merely
/// makes it redundant.
///
/// `None` is the absence, which is how a tool goes back to asking by the
/// default path rather than by a rule. That is a real difference: a
/// `Decision::Ask` rule forces a prompt even where a session grant already
/// exists, and writing one for every tool somebody left alone would be a
/// settings screen quietly changing what "default" means.
pub fn set_bare_rule(file: &Path, tool: &str, decision: Option<Decision>) -> Result<()> {
    let mut doc: serde_json::Value = match std::fs::read_to_string(file) {
        Ok(raw) if raw.trim().is_empty() => serde_json::json!({}),
        Ok(raw) => serde_json::from_str(&raw).with_context(|| {
            format!(
                "{} is not valid JSON. Nothing was written to it; fix the file by hand",
                file.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let shape = kind_of(&doc);
    let root = doc.as_object_mut().with_context(|| {
        format!(
            "{} holds a JSON {shape} rather than an object, so it is not a settings file. \
             Nothing was written to it",
            file.display(),
        )
    })?;
    let permissions = root
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    let permissions = permissions.as_object_mut().with_context(|| {
        format!(
            "{}: `permissions` is not an object. Nothing was written to it",
            file.display()
        )
    })?;

    let is_ours = |v: &serde_json::Value| match v.as_str() {
        Some(text) => matches!(Rule::parse(text), Ok(r) if r.tool == tool && r.spec == Spec::All),
        None => false,
    };
    // Every list is swept before anything is added, so a tool cannot end up
    // named in two of them and the row cannot show a state the gate disagrees
    // with. A list that is not an array refuses the whole write rather than
    // being replaced: it is somebody's, and this function did not put it there.
    for key in ["allow", "ask", "deny"] {
        let Some(slot) = permissions.get_mut(key) else {
            continue;
        };
        let list = slot.as_array_mut().with_context(|| {
            format!(
                "{}: `permissions.{key}` is not an array. Nothing was written to it",
                file.display()
            )
        })?;
        list.retain(|v| !is_ours(v));
    }
    if let Some(decision) = decision {
        let slot = permissions
            .entry(list_key(decision))
            .or_insert_with(|| serde_json::json!([]));
        let list = slot.as_array_mut().with_context(|| {
            format!(
                "{}: `permissions.{}` is not an array. Nothing was written to it",
                file.display(),
                list_key(decision)
            )
        })?;
        list.push(serde_json::Value::String(tool.to_string()));
    }

    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    // Temp-and-rename, as `remember` does: a crash mid-write must not leave a
    // half-written settings file behind.
    let body = format!("{}\n", serde_json::to_string_pretty(&doc)?);
    let temp = file.with_extension("json.emma-tmp");
    std::fs::write(&temp, body).with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, file).with_context(|| format!("writing {}", file.display()))?;
    Ok(())
}

/// Append `rule` to `permissions.allow` in `file`, creating the file if it is
/// not there. Returns `true` when something was written and `false` when the
/// rule was already present.
///
/// **It merges, and the refusals are the feature.** This project has already
/// been bitten once by a write path that replaced a shared configuration
/// document and silently deleted another program's key. So: the file is read,
/// parsed as a whole document, and written back with every key it had —
/// `hooks`, `statusLine`, `env`, whatever else is in there. A file that does not
/// parse as JSON, or that is not an object, or whose `permissions` or
/// `permissions.allow` are not the shapes this function needs, is **not written
/// to at all**. The user is told which file and what is wrong with it, and their
/// grant lasts the session instead. Losing a grant is an annoyance; losing
/// somebody's `hooks` block is a defect they discover weeks later.
///
/// The write goes to a sibling temporary file and is renamed over the target, so
/// an interrupted run cannot leave a half-written settings file behind.
pub fn remember(file: &Path, rule: &Rule) -> Result<bool> {
    let mut doc: serde_json::Value = match std::fs::read_to_string(file) {
        Ok(raw) if raw.trim().is_empty() => serde_json::json!({}),
        Ok(raw) => serde_json::from_str(&raw).with_context(|| {
            format!(
                "{} is not valid JSON. Nothing was written to it — fix the file by hand, or \
                 this grant stays for this session only",
                file.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };

    // The shape is named before the borrow, so the refusal can say *what* the
    // file holds rather than only that it is not an object.
    let shape = kind_of(&doc);
    let root = doc.as_object_mut().with_context(|| {
        format!(
            "{} holds a JSON {shape} rather than an object, so it is not a settings file. \
             Nothing was written to it",
            file.display(),
        )
    })?;
    let permissions = root
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    let permissions = permissions.as_object_mut().with_context(|| {
        format!(
            "{}: `permissions` is not an object. Nothing was written to it",
            file.display()
        )
    })?;
    let allow = permissions
        .entry("allow")
        .or_insert_with(|| serde_json::json!([]));
    let allow = allow.as_array_mut().with_context(|| {
        format!(
            "{}: `permissions.allow` is not an array. Nothing was written to it",
            file.display()
        )
    })?;

    let text = rule.to_string();
    if allow.iter().any(|v| v.as_str() == Some(text.as_str())) {
        return Ok(false);
    }
    allow.push(serde_json::Value::String(text));

    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let body = format!("{}\n", serde_json::to_string_pretty(&doc)?);
    let temp = file.with_extension("json.emma-tmp");
    std::fs::write(&temp, body).with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, file).with_context(|| format!("writing {}", file.display()))?;
    Ok(true)
}

fn kind_of(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Where a remembered rule goes, and why it is not `.claude/settings.json`.
///
/// **`settings.local.json`, beside whichever harness this project actually
/// loaded.** Three things decided it:
///
/// - *It is the personal file, not the project's policy.* Claude Code documents
///   `settings.local.json` as git-ignored and says outright that it is where it
///   "saves permanent 'don't ask again' permission approvals". These grants are
///   one person's, made at a prompt, for their machine. Writing them into
///   `settings.json` would commit one developer's convenience as everybody's
///   policy on the next `git add`.
/// - *Beside the loaded harness, not always `.claude/`.* Emma discovers `.emma/`
///   or `.claude/` and `.emma/` wins outright where both exist. A project that
///   deliberately has `.emma/` should not acquire a `.claude/` directory because
///   somebody answered a prompt — that is Emma creating configuration for a
///   different program without being asked. The file name and the JSON shape are
///   Claude Code's in both directories, so a `.claude/` project gets a file the
///   other program reads natively.
/// - *Not `~`.* A grant made in one repository is not a grant everywhere; see
///   `user_permissions` in the harness for the other half of that argument.
pub fn file_for(root: &Path) -> PathBuf {
    root.join("settings.local.json")
}

/// Rule texts already in `file`, for a caller that wants to say whether a grant
/// would be new. Any problem reading it answers "nothing", because this is
/// cosmetic and [`remember`] is where the refusals live.
pub fn already_written(file: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|doc| doc["permissions"]["allow"].as_array().cloned())
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// endregion: Writing one down

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Three groups, and the middle one is the point. Parsing is checked for what it
// refuses as much as what it accepts; matching is checked against hosts an
// attacker can register; precedence is checked in the direction that costs
// something if it inverts.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: PermissionKind, rule: &str) -> PermissionEntry {
        PermissionEntry {
            rule: rule.to_string(),
            kind,
            source: PathBuf::from("settings.local.json"),
        }
    }

    fn rules(deny: &[&str], ask: &[&str], allow: &[&str]) -> Rules {
        let mut entries = Vec::new();
        for r in deny {
            entries.push(entry(PermissionKind::Deny, r));
        }
        for r in ask {
            entries.push(entry(PermissionKind::Ask, r));
        }
        for r in allow {
            entries.push(entry(PermissionKind::Allow, r));
        }
        Rules::parse(&entries).0
    }

    // -----------------------------------------------------------------------
    // Parsing
    // -----------------------------------------------------------------------

    /// Renamed from `the_three_shapes_a_rule_can_have`: there are five now.
    /// `Bash(git log:*)` and `Read(./src/**)` used to land in `Unsupported`,
    /// which is what this test asserted — correctly, of the build it was
    /// written for.
    /// A `Bash` deny that contains a slash must actually deny.
    ///
    /// **This is the test that was missing, and its absence cost a silent
    /// deny-list hole twice in one direction and once in the other.** Round
    /// one: shape was chosen by punctuation, so `Read(Cargo.toml)` became a
    /// command prefix and matched nothing. Round two: the fix kept the
    /// punctuation test as an extra disjunct for `Bash`, so every `Bash` rule
    /// containing `/` became a path glob and matched nothing — measured at 24
    /// of 53 rules in the owner's real `settings.json`, deny rules among them.
    ///
    /// Both rounds passed every test in this file. What neither had was a case
    /// asserting that a rule of each shape **fires on a call of its own tool**.
    /// A rule that parses is not a rule that matches, and every test here was
    /// about parsing.
    ///
    /// The commands below are the ordinary spellings a deny list is written
    /// with. Each contains a separator; none is a path.
    /// The ordinary spellings of one program are one program.
    ///
    /// **Three of these ran past a `Bash(git *)` deny with no `Ask` and no
    /// note**, and none was listed among the residual bypasses the row
    /// enumerates. `.exe` is *the* spelling on Windows; case is how Windows
    /// looks programs up; a leading quote is ordinary shell quoting. The path
    /// matcher was already case-insensitive on Windows for exactly this reason,
    /// so the file held two answers to one question about one operating system.
    #[test]
    fn the_ordinary_spellings_of_a_program_do_not_evade_a_deny() {
        let r = rules(&["Bash(git *)"], &[], &[]);
        for command in [
            "git push",
            "\"git\" push",
            // Interior quoting, found by review: `trim_matches` stripped the
            // ends only, so this reached the matcher as the program name
            // `gi"t` and evaded the deny with no Deny and no Ask.
            "gi\"t\" push",
        ] {
            assert_eq!(
                r.for_call("Bash", &serde_json::json!({ "command": command })),
                Some(Decision::Deny),
                "`{command}` against deny Bash(git *)"
            );
        }

        // These suffixes are aliases supplied by Windows program lookup. On
        // unix they name distinct files, so folding them there would invent a
        // match the operating system does not make.
        for command in [
            "git.exe push",
            "git.cmd push",
            // Declared in the strip list and previously untested, which a
            // reviewer pointed out is its own gap: dropping them from
            // `normalise_program` used to leave this green.
            "git.bat push",
            "git.com push",
        ] {
            let expected = if cfg!(windows) {
                Some(Decision::Deny)
            } else {
                None
            };
            assert_eq!(
                r.for_call("Bash", &serde_json::json!({ "command": command })),
                expected,
                "`{command}` against deny Bash(git *)"
            );
        }

        // Case, Windows only and asserted only there, because on unix `GIT` is
        // a different program and pretending otherwise is its own defect.
        if cfg!(windows) {
            assert_eq!(
                r.for_call("Bash", &serde_json::json!({ "command": "GIT push" })),
                Some(Decision::Deny),
                "Windows program lookup is case-insensitive, so `GIT push` runs"
            );
        }

        // The boundary the fold must not cross: arguments are not folded, and a
        // different program is still a different program.
        assert_eq!(
            r.for_call("Bash", &serde_json::json!({ "command": "github-cli push" })),
            None,
            "the word boundary was lost — `git` must not match `github-cli`"
        );
    }

    /// Every rule that can never fire says so at boot.
    ///
    /// **Both of these were silent, and both are the sentence `DEF-015` was
    /// filed over: a protection the operator believes they have.** One is the
    /// unregistered tool name in that row's own title, which was never
    /// implemented — only the case-variant branch spoke, and an unknown name
    /// fell out of the `if` in silence. The other is a command prefix that ends
    /// inside a word: `command_matches` is a word-boundary test on purpose, so
    /// such a prefix matches nothing however it is written.
    ///
    /// Asserted on the notes, which is the operator's channel, and not on the
    /// parse — a rule that parses is not a rule that fires, and that gap is
    /// where every defect in this file has lived.
    #[test]
    fn a_rule_that_can_never_fire_is_announced() {
        let known: &[&'static str] = &["Bash", "Read", "WebFetch"];
        let network: &[&'static str] = &["WebFetch"];

        for (rule, must_say) in [
            // A tool that is not in this run at all. Previously silent.
            ("Bahs(rm)", "not a tool in this run"),
            // A prefix the parser left ending mid-word. Previously silent.
            ("Bash(curl https://*)", "matches only the exact command"),
            // **The rule that made this branch lie.** `sudo*` looks like a
            // wildcard and is not: the parser consumes the star, leaving the
            // prefix `sudo`, and the rule denies the command `sudo`. It was
            // announced as unable to match anything at all.
            ("Bash(sudo*)", "matches only the exact command"),
            ("Bash(rm -rf /*)", "matches only the exact command"),
            // The two that already spoke, kept so this cannot pass by the
            // older branches having been deleted.
            ("bash(rm)", "spelled `Bash`"),
            ("Read(domain:example.com)", "does not declare"),
        ] {
            let notes =
                Rules::unmatchable_here(&[entry(PermissionKind::Deny, rule)], known, network);
            assert!(
                notes.iter().any(|n| n.contains(must_say)),
                "`{rule}` was not announced with `{must_say}`: {notes:?}"
            );
        }

        // **The rules that must stay silent, and this half was missing.**
        //
        // For half a day the check also fired on any prefix ending `/`, `:`,
        // `=` or `-`, so `Bash(rm -rf /)` denied `rm -rf /` and was announced
        // at every boot as unable to match. A reviewer printed the decision and
        // the note side by side. The original positive control below could not
        // catch it, because none of its four rules ends in one of those
        // characters -- a control only controls for what it contains.
        //
        // These are exact-match rules, which is a legitimate thing to write:
        // `command_matches` matches on equality as well as on prefix-plus-space,
        // so a rule with no star matches the command it spells out.
        for rule in [
            "Bash(rm -rf /)",
            "Bash(cd /)",
            "Bash(make CC=gcc)",
            "Bash(ls -l)",
        ] {
            let notes =
                Rules::unmatchable_here(&[entry(PermissionKind::Deny, rule)], known, network);
            // **The marker had to change with the note, or this control would
            // have stopped controlling.** It used to look for "inside a word",
            // which the corrected note no longer contains anywhere -- so after
            // the fix it would have passed against any output whatsoever,
            // including a note announcing every one of these as dead. An
            // assertion that survives the disappearance of what it looks for is
            // the false receipt this file has already been rewritten over
            // twice.
            assert!(
                !notes
                    .iter()
                    .any(|n| n.contains("matches only the exact command")),
                "`{rule}` is an exact-match rule that fires, and was announced as dead. \
                 Telling an operator a live protection is dead is how a live protection \
                 gets deleted: {notes:?}"
            );
        }

        // **And the sentence that was false, named so it cannot come back.**
        // Three passes over this branch, two of them wrong in the same
        // direction; the specific falsehood was that a rule "can never match".
        // No rule this function sees can be truthfully described that way,
        // because `command_matches` tries equality first and a non-empty prefix
        // always matches itself.
        for rule in [
            "Bash(sudo*)",
            "Bash(rm -rf /*)",
            "Bash(curl https://*)",
            "Bash(rm -rf /)",
        ] {
            let notes =
                Rules::unmatchable_here(&[entry(PermissionKind::Deny, rule)], known, network);
            assert!(
                !notes.iter().any(|n| n.contains("never match")),
                "`{rule}` was announced as unable to match. It matches at least the \
                 command it spells: equality is tried before the word-boundary test. \
                 An operator who believes this deletes a rule that works: {notes:?}"
            );
        }

        // The positive control, and it is load-bearing: a note on every rule is
        // noise, and would also make every assertion above pass for the wrong
        // reason.
        for rule in [
            "Bash(git *)",
            "Bash(npm run test:*)",
            "Read(src/**)",
            "Bash",
        ] {
            let notes =
                Rules::unmatchable_here(&[entry(PermissionKind::Deny, rule)], known, network);
            assert!(
                notes.is_empty(),
                "`{rule}` fires perfectly well and was announced as inert: {notes:?}"
            );
        }
    }

    /// Three ordinary shell spellings that walked past a deny rule in silence.
    ///
    /// **`None` is the finding, not `Ask`.** A deny rule for `Bash(git *)` was
    /// present, the command ran git, and the gate did not deny it, did not ask,
    /// and printed no boot note. `is_composed`'s own doc sets the standard this
    /// fails: *"a false negative is a deny rule that did not fire."*
    ///
    /// Found by an independent reviewer driving `Rules` directly. The newline
    /// case is the one worth understanding, because the evidence was being
    /// destroyed before the check ran: `command_of` normalises with
    /// `split_whitespace().join(" ")`, so `echo hi\ngit push` reached
    /// `is_composed` as `echo hi git push` — one command, first word `echo`.
    /// Adding the token alone would have fixed nothing; the call site had to
    /// stop handing over the normalised string.
    ///
    /// The answer is `Ask` rather than `Deny` on purpose. The rule genuinely
    /// does not match these strings, and claiming it did would be the same
    /// dishonesty pointing the other way. What is not acceptable is silence.
    #[test]
    fn an_ordinary_shell_spelling_cannot_walk_past_a_deny_rule() {
        for command in [
            // A newline is a command separator.
            "echo hi\ngit push origin main",
            "echo hi\r\ngit push origin main",
            // A bare subshell and a group. `$(` was caught and `(` was not.
            "(git push origin main)",
            "{ git push origin main; }",
            // `command` is a POSIX builtin in the same class as `env`.
            "command git push origin main",
            "sudo git push origin main",
            "nohup git push origin main",
            "timeout 5 git push origin main",
        ] {
            let r = rules(&["Bash(git *)"], &[], &[]);
            let got = r.for_call("Bash", &serde_json::json!({ "command": command }));
            assert!(
                matches!(got, Some(Decision::Deny) | Some(Decision::Ask)),
                "`{command}` reached the gate with a `Bash(git *)` deny in force and got \
                 {got:?} — no deny, no question, and nothing said. That is the deny rule \
                 not firing, which is the failure this check exists to prevent"
            );
        }

        // **The control, and it is the half that keeps the widening honest.**
        // `is_composed` is deliberately over-broad, and over-broad has a floor:
        // a plain command with a deny rule present that does not name it must
        // still come back clean, or every `Bash` call in a repository with one
        // `git` deny becomes a prompt and the operator learns to hit `y`.
        for command in ["ls -la", "cargo test --workspace", "echo hello"] {
            let r = rules(&["Bash(git *)"], &[], &[]);
            assert_eq!(
                r.for_call("Bash", &serde_json::json!({ "command": command })),
                None,
                "`{command}` names no git and contains nothing composed, and was \
                 questioned anyway — a prompt that fires when it should not is how an \
                 operator learns to answer without reading"
            );
        }
    }

    #[test]
    fn a_bash_deny_containing_a_slash_still_denies() {
        for (rule, command) in [
            ("Bash(rm -rf /)", "rm -rf /"),
            ("Bash(curl:*)", "curl https://evil.example/x"),
            ("Bash(./deploy.sh)", "./deploy.sh --prod"),
            ("Bash(node scripts/x.js)", "node scripts/x.js --flag"),
        ] {
            let r = rules(&[rule], &[], &[]);
            assert_eq!(
                r.for_call("Bash", &serde_json::json!({ "command": command })),
                Some(Decision::Deny),
                "`{rule}` did not fire on `{command}` — an inert deny rule is a \
                 protection the operator believes they have"
            );
        }

        // And the converse, so this cannot be satisfied by denying everything:
        // a command the rule does not name is not denied by it.
        let r = rules(&["Bash(rm -rf /)"], &[], &[]);
        assert_eq!(
            r.for_call("Bash", &serde_json::json!({ "command": "ls -la" })),
            None,
            "the rule denied a command it does not name"
        );

        // The other half of the same bug, kept beside it: a non-Bash tool's
        // specifier is a path however it is punctuated.
        let r = rules(&["Read(Cargo.toml)"], &[], &[]);
        assert_eq!(
            r.for_call("Read", &serde_json::json!({ "file_path": "Cargo.toml" })),
            Some(Decision::Deny),
            "a bare filename stopped being a path again"
        );
    }

    #[test]
    fn the_shapes_a_rule_can_have() {
        assert_eq!(Rule::parse("WebSearch").unwrap().spec, Spec::All);
        assert_eq!(Rule::parse("WebSearch(*)").unwrap().spec, Spec::All);
        assert_eq!(Rule::parse("WebFetch(domain:*)").unwrap().spec, Spec::All);
        assert!(matches!(
            Rule::parse("WebFetch(domain:apnews.com)").unwrap().spec,
            Spec::Domain(_)
        ));
        assert!(matches!(
            Rule::parse("Bash(git log:*)").unwrap().spec,
            Spec::Command(..)
        ));
        assert!(matches!(
            Rule::parse("Read(./src/**)").unwrap().spec,
            Spec::Path(..)
        ));
    }

    /// A command prefix matches at a word boundary and nowhere else.
    ///
    /// `git` must not cover `github-cli`, which a raw `starts_with` would do.
    /// In a deny list that is the difference between blocking what the operator
    /// wrote and blocking something they never named.
    #[test]
    fn a_command_prefix_stops_at_a_word_boundary() {
        assert!(command_matches("git", "git"));
        assert!(command_matches("git", "git log --oneline"));
        assert!(!command_matches("git", "github-cli release"));
        assert!(!command_matches("git log", "git"));
        // Whitespace is normalised on both sides, so a double space is not a
        // way past a deny rule.
        assert!(command_matches(
            "git log",
            &normalise_command("git   log -n1")
        ));
    }

    /// A command a prefix rule cannot judge does not slip past a deny.
    ///
    /// `Spec::Command` matches argument text and `Bash` runs that text through a
    /// shell, so `env git push` and `cd . && git push` are plainly git pushes
    /// that a `Bash(git *)` prefix does not match. Found by adversarial review
    /// after the matcher shipped.
    ///
    /// The answer is `Ask`, not `Deny`: the rule genuinely did not match, and
    /// claiming it did would be its own dishonesty. What is refused is *silence*.
    #[test]
    fn a_composed_command_cannot_slip_past_a_deny_prefix() {
        use serde_json::json;
        let mut rules = Rules::default();
        rules.deny.push(Rule::parse("Bash(git *)").unwrap());
        let ask = |c: &str| rules.for_call("Bash", &json!({ "command": c }));

        // The plain case still denies outright.
        assert_eq!(ask("git push"), Some(Decision::Deny));

        // The evasions become questions rather than silence.
        for evasion in [
            "env git push",
            "cd . && git push",
            "GIT_SSH_COMMAND=x git push",
            "sh -c 'git push'",
            "echo hi | git push",
            // Path-qualified, found by a second adversarial pass. Plainly git,
            // and `git` as a prefix does not match either spelling.
            "/usr/bin/git push origin main",
            r".\\tools\\git.exe push",
        ] {
            assert_eq!(
                ask(evasion),
                Some(Decision::Ask),
                "`{evasion}` slipped past a deny rule"
            );
        }

        // An ordinary uncomposed command nobody wrote a rule about is still
        // silent here — turning every call into a question is how a prompt
        // stops being read.
        assert_eq!(ask("cargo test"), None);
    }

    /// With no command-prefix deny rule in force, composition changes nothing.
    #[test]
    fn composition_only_matters_when_a_prefix_deny_exists() {
        use serde_json::json;
        let rules = Rules::default();
        assert_eq!(
            rules.for_call("Bash", &json!({ "command": "cd . && git push" })),
            None,
            "composition was treated as suspicious with no rule to protect"
        );
    }

    /// The rules that used to match nothing now bite, in both directions.
    #[test]
    fn a_command_and_a_path_rule_actually_match_a_call() {
        use serde_json::json;
        let deny = |r: &str| {
            let mut rules = Rules::default();
            rules.deny.push(Rule::parse(r).unwrap());
            rules
        };
        let r = deny("Bash(git *)");
        assert_eq!(
            r.for_call("Bash", &json!({ "command": "git push" })),
            Some(Decision::Deny)
        );
        assert_eq!(
            r.for_call("Bash", &json!({ "command": "cargo test" })),
            None
        );

        let r = deny("Read(./src/**)");
        assert_eq!(
            r.for_call("Read", &json!({ "file_path": "./src/main.rs" })),
            Some(Decision::Deny)
        );
        assert_eq!(
            r.for_call("Read", &json!({ "file_path": "./docs/index.html" })),
            None
        );
    }

    /// A path rule and the call it is written against are the same file however
    /// each is spelled. Until DEF-031's path half landed, `Read(./src/**)` was
    /// blind to `src/main.rs` and `Read(src/**)` was blind to `./src/main.rs` —
    /// two strings for one file, and a deny that protected only the spelling
    /// the operator happened to use.
    #[test]
    fn a_path_rule_is_not_defeated_by_respelling_the_same_file() {
        use serde_json::json;
        let deny = |raw: &str| {
            let mut r = Rules::default();
            r.deny.push(Rule::parse(raw).unwrap());
            r
        };
        for rule in ["Read(./src/**)", "Read(src/**)"] {
            let r = deny(rule);
            for spelling in [
                "src/main.rs",
                "./src/main.rs",
                "src//main.rs",
                "./src/./main.rs",
                r"src\\main.rs",
            ] {
                assert_eq!(
                    r.for_call("Read", &json!({ "file_path": spelling })),
                    Some(Decision::Deny),
                    "rule {rule} was defeated by spelling the same file {spelling}"
                );
            }
            assert_eq!(
                r.for_call("Read", &json!({ "file_path": "./docs/index.html" })),
                None,
                "rule {rule} widened to a file it does not name"
            );
        }
    }

    /// What normalisation cannot settle, it refuses to answer quietly. A `..`
    /// needs to know what is a directory and what is a link; an absolute call
    /// against a relative rule needs a root the gate does not hold. Both reach
    /// the human instead of passing in silence.
    #[test]
    fn a_path_a_rule_cannot_be_compared_against_asks_rather_than_passes() {
        use serde_json::json;
        let mut r = Rules::default();
        r.deny.push(Rule::parse("Read(src/**)").unwrap());
        for unjudgeable in [
            "src/../etc/passwd",
            "build/../src/main.rs",
            "/home/me/project/src/main.rs",
        ] {
            // Deny or Ask, never silence. A `..` path that the glob happens to
            // match anyway gets the stricter answer, which needs no question;
            // what must not happen is passing through as if the rule had been
            // consulted and had nothing to say.
            assert!(
                matches!(
                    r.for_call("Read", &json!({ "file_path": unjudgeable })),
                    Some(Decision::Deny) | Some(Decision::Ask)
                ),
                "{unjudgeable} slipped past a deny rule without a question"
            );
        }
        // An ordinary relative path is still judged, not questioned: a gate that
        // asks about everything is a gate nobody reads.
        assert_eq!(
            r.for_call("Read", &json!({ "file_path": "docs/index.html" })),
            None
        );
    }

    /// Seven bypasses a cross-model review supplied against the first version of
    /// the path matcher. Each is an exact input it gave, kept as the input
    /// rather than a paraphrase of it, because the paraphrase is what the
    /// original code was already written against.
    ///
    /// Two of the seven are the *false-positive* direction — a rule answering
    /// where it should have stayed quiet. Those matter as much: a gate that asks
    /// about everything is a gate nobody reads.
    #[test]
    fn the_respelling_bypasses_a_review_found_are_all_closed() {
        use serde_json::json;
        let rules = |raws: &[&str]| {
            let mut r = Rules::default();
            for raw in raws {
                r.deny.push(Rule::parse(raw).unwrap());
            }
            r
        };

        // 1. A backslash in a path rule is refused. The review reported the
        //    unix reading — `\*` is a literal asterisk, and folding it to a
        //    separator broke the rule in both directions — and it is right
        //    there. On Windows globset reads the same byte as a separator, so
        //    the rule would have meant something else again. One settings file
        //    cannot mean two things, and neither reading is wrong enough to
        //    pick over the other.
        for ambiguous in [r"Read(src/a\*b)", r"Read(.\src\**)"] {
            let e = Rule::parse(ambiguous).expect_err("a backslash rule was accepted");
            assert!(
                e.to_string().contains("write the path with `/`"),
                "the refusal did not say what to write instead: {e}"
            );
        }

        // 2. A drive-absolute Windows path is absolute. Reading only a leading
        //    slash made it compare as if it were relative.
        let r = rules(&["Read(crates/emma/src/permissions.rs)"]);
        for absolute in [
            r"C:\src\emma\crates\emma\src\permissions.rs",
            "C:/src/emma/crates/emma/src/permissions.rs",
            r"\server\share\crates\emma\src\permissions.rs",
        ] {
            assert!(
                r.for_call("Read", &json!({ "file_path": absolute }))
                    .is_some(),
                "{absolute} passed a deny rule naming that exact file"
            );
        }

        // 3. Windows folds case and strips trailing dots and spaces before it
        //    opens anything. A matcher that does not is one the filesystem
        //    disagrees with.
        if cfg!(windows) {
            for alias in [
                "CRATES/emma/src/permissions.rs",
                "crates/emma/src/permissions.rs.",
                "crates/emma/src/permissions.rs ",
            ] {
                assert_eq!(
                    r.for_call("Read", &json!({ "file_path": alias })),
                    Some(Decision::Deny),
                    "{alias} is the same file to Windows and a different string to the rule"
                );
            }
        }

        // 4. A deny that matched outranks a refusal to judge. `Ask` is weaker
        //    than `Deny`, so asking before the ladder ran downgraded a rule that
        //    plainly fired.
        let r = rules(&["Read", "Read(src/**)"]);
        assert_eq!(
            r.for_call("Read", &json!({ "file_path": "/tmp/not-src" })),
            Some(Decision::Deny),
            "a bare deny was downgraded to a question by a narrower rule beside it"
        );

        // 5. ...and the same, across the two shapes: a command deny that
        //    matched, on a call that also carried a path. `Bash(src/**)` is a
        //    *command* prefix — `Bash` has no path form — so it is simply a
        //    second rule that does not match, which is the condition this case
        //    needs.
        let r = rules(&["Bash(git *)", "Bash(src/**)"]);
        assert_eq!(
            r.for_call(
                "Bash",
                &json!({ "command": "git push", "file_path": "/tmp/not-src" })
            ),
            Some(Decision::Deny),
            "a matching command deny was hidden by the path preflight"
        );

        // 6. A `..` in a rule cannot be evaluated, so the rule is refused out
        //    loud at parse time. It used to be kept, and then made every
        //    unrelated read ask.
        assert!(
            Rule::parse("Read(build/../src/main.rs)").is_err(),
            "a rule that can never be matched was kept rather than refused"
        );

        // 7. A bare filename is a path, not a command prefix. `Read` has no
        //    `command` argument, so parsing it as one matched nothing at all.
        let r = rules(&["Read(Cargo.toml)"]);
        assert_eq!(
            r.for_call("Read", &json!({ "file_path": "Cargo.toml" })),
            Some(Decision::Deny),
            "a deny naming one file by name protected nothing"
        );
        // Bash keeps prefix semantics: that is the tool that runs a command.
        let r = rules(&["Bash(git)"]);
        assert_eq!(
            r.for_call("Bash", &json!({ "command": "git push" })),
            Some(Decision::Deny)
        );
    }

    #[test]
    fn a_rule_round_trips_through_its_own_text() {
        // The written file is the user interface. A rule that renders back as
        // something that parses differently is a grant that means one thing on
        // screen and another on the next boot.
        for raw in [
            "WebSearch",
            "WebFetch(domain:apnews.com)",
            "WebFetch(domain:*.example.com)",
            "Bash(git log:*)",
        ] {
            let once = Rule::parse(raw).unwrap();
            assert_eq!(once.to_string(), raw);
            assert_eq!(Rule::parse(&once.to_string()).unwrap(), once);
        }
    }

    #[test]
    fn a_malformed_rule_is_refused_rather_than_silently_inert() {
        // Every one of these used to have a plausible "just ignore it" reading.
        // The hazard is the `deny` list: a rule the operator wrote and Emma
        // dropped without a word is a protection they believe they have.
        for bad in [
            "",
            "   ",
            "WebFetch(",
            "WebFetch(domain:apnews.com",
            "(domain:apnews.com)",
            "WebFetch(domain:)",
            "WebFetch(domain: )",
            "WebFetch(domain:..)",
            "WebFetch(domain:a..b)",
            "Web Fetch",
        ] {
            assert!(
                Rule::parse(bad).is_err(),
                "`{bad}` parsed instead of being refused"
            );
        }
    }

    #[test]
    fn a_glob_in_the_tool_position_is_refused() {
        // Claude Code permits this for deny rules. Emma does not permit it at
        // all, because the same syntax in the `allow` list is one typo away from
        // approving the entire tool surface for ever.
        for bad in ["*", "*(domain:x)", "Web*", "mcp__*"] {
            let err = Rule::parse(bad)
                .err()
                .unwrap_or_else(|| panic!("`{bad}` was accepted"))
                .to_string();
            assert!(
                err.contains("exactly") || err.contains("tool name"),
                "{err}"
            );
        }
    }

    /// **This test used to assert the defect.** `Bash(rm *)` and
    /// `Read(./src/**)` were `Unsupported` — parsed, kept, announced as
    /// matching nothing — so a deny list copied from a Claude Code project
    /// protected only what it named baldly. Both evaluate now, and the
    /// assertions here are inverted to say so rather than deleted, because the
    /// old expectations are the record of what changed.
    #[test]
    fn the_shapes_that_used_to_be_inert_now_evaluate() {
        use serde_json::json;
        let (rules, notes) = Rules::parse(&[
            entry(PermissionKind::Deny, "Bash(rm *)"),
            entry(PermissionKind::Allow, "Read(./src/**)"),
        ]);
        assert!(!rules.is_empty(), "both rules were dropped");
        assert_eq!(
            rules.for_call("Bash", &json!({ "command": "rm -rf build" })),
            Some(Decision::Deny),
            "a deny rule copied from a Claude Code project still protects nothing"
        );
        assert_eq!(
            rules.for_call("Read", &json!({ "file_path": "./src/main.rs" })),
            Some(Decision::Allow)
        );
        // Nothing to announce any more: an announcement that fires on a rule
        // which works is how announcements stop being read.
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// The announcement itself is unchanged for anything still unevaluable, and
    /// the deny wording still carries the alarm.
    #[test]
    fn a_specifier_that_still_cannot_be_evaluated_is_announced() {
        let (rules, notes) = Rules::parse(&[entry(PermissionKind::Deny, "Bash(:*)")]);
        assert!(
            rules.is_empty(),
            "an unevaluable rule was kept as if it worked"
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("blocks NOTHING"), "{}", notes[0]);
    }

    #[test]
    fn an_unsupported_rule_that_reaches_a_list_anyway_still_matches_nothing() {
        // `parse` drops these before they reach a list, so the matcher's
        // `Unsupported => false` arm is unreachable through it — and an arm
        // nothing can reach is an arm nothing checks. A mutation flipping it to
        // `true` went undetected until this test existed, which is precisely how
        // `Bash(rm *)` would one day start matching every call to `Bash`.
        //
        // `adopt` is the other way in, so it is the way in used here.
        //
        // **The specifier had to change, and the reason is the point.** This
        // test used to adopt `Bash(git log:*)`, which parsed to
        // `Spec::Unsupported` when it was written. ARCH-002 gave that spelling a
        // real matcher, so the test went on passing while no longer constructing
        // the variant it exists to guard — flipping the arm to `true` would not
        // have reddened it. A false receipt, and it took a docs audit rather
        // than the suite to notice, because a test that stops reaching its
        // subject looks exactly like a test that passes.
        //
        // So the shape is asserted first. If a later change gives *this*
        // spelling a matcher too, this line fails and says why, rather than the
        // guard quietly stopping being guarded again.
        let rule = Rule::parse("Bash(:*)").unwrap();
        assert!(
            matches!(rule.spec, Spec::Unsupported(_)),
            "this test no longer constructs the arm it guards: {rule}"
        );
        let mut r = Rules::default();
        r.adopt(rule);
        assert_eq!(r.for_tool("Bash"), None);
        assert_eq!(
            r.for_call("Bash", &serde_json::json!({ "command": "rm -rf /" })),
            None
        );
        assert_eq!(r.for_egress("Bash", "docs.rs"), None);
        assert_eq!(r.for_egress("Bash", "anything.at.all"), None);
    }

    #[test]
    fn a_unicode_domain_rule_is_announced_and_the_deny_wording_is_the_loud_one() {
        // The defect: this parses, warns about nothing, and matches nothing,
        // because every host `WebFetch` presents has already been punycoded by
        // `Url::host_str`. In a deny list that is a protection that protects
        // nothing, and nothing said so.
        let (rules, notes) = Rules::parse(&[
            entry(PermissionKind::Deny, "WebFetch(domain:b\u{fc}cher.example)"),
            entry(
                PermissionKind::Allow,
                "WebFetch(domain:b\u{fc}cher.example)",
            ),
        ]);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("blocks NOTHING"), "{}", notes[0]);
        assert!(notes[1].contains("still ask"), "{}", notes[1]);
        // The rewrite is computed, not left to the operator: hand-punycoding is
        // how `xn--bcher-kva` becomes `xn--bucher-kva`.
        for note in &notes {
            assert!(
                note.contains("WebFetch(domain:xn--bcher-kva.example)"),
                "{note}"
            );
        }
        // …and the rule is still live for a caller that presents a Unicode
        // host directly. The warning changes no matching behaviour at all.
        assert_eq!(
            rules.for_egress("WebFetch", "b\u{fc}cher.example"),
            Some(Decision::Deny)
        );
    }

    #[test]
    fn an_unbracketed_ipv6_rule_is_announced_with_the_brackets_it_needs() {
        let (rules, notes) = Rules::parse(&[entry(PermissionKind::Deny, "WebFetch(domain:::1)")]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("blocks NOTHING"), "{}", notes[0]);
        assert!(notes[0].contains("WebFetch(domain:[::1])"), "{}", notes[0]);
        // Still exactly as inert against the host a URL actually produces…
        assert_eq!(rules.for_egress("WebFetch", "[::1]"), None);
        // …and a rule that *is* written with brackets says nothing at boot.
        let (_, quiet) = Rules::parse(&[entry(PermissionKind::Deny, "WebFetch(domain:[::1])")]);
        assert!(quiet.is_empty(), "{quiet:?}");
    }

    #[test]
    fn a_suggested_rewrite_keeps_the_wildcard_it_was_written_with() {
        let (_, notes) = Rules::parse(&[entry(
            PermissionKind::Deny,
            "WebFetch(domain:*.b\u{fc}cher.example)",
        )]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("WebFetch(domain:*.xn--bcher-kva.example)"),
            "{}",
            notes[0]
        );
    }

    #[test]
    fn a_rewrite_that_cannot_be_computed_is_omitted_rather_than_invented() {
        // Two ways this build declines to answer, and neither may guess. A
        // wildcard is not valid IDNA input, so a label carrying both a star and
        // a non-ASCII character cannot be converted at all; and anything `idna`
        // itself refuses ends the attempt. A wrong `xn--` spelling in a warning
        // is a chore that fails silently at the next boot.
        for rule in [
            "WebFetch(domain:b*cher\u{fc}.example)",
            "WebFetch(domain:a\u{fffd}b.example)",
        ] {
            let (_, notes) = Rules::parse(&[entry(PermissionKind::Deny, rule)]);
            assert_eq!(notes.len(), 1, "{rule}: {notes:?}");
            assert!(notes[0].contains("blocks NOTHING"), "{}", notes[0]);
            assert!(
                !notes[0].contains("Write `"),
                "a rewrite was invented for `{rule}`: {}",
                notes[0]
            );
        }
    }

    #[test]
    fn an_ordinary_ascii_rule_says_nothing_at_boot() {
        // The half that is hard to get right: the warning must not fire on
        // working configuration. `tests/permissions.rs` proves this over the
        // whole matrix; this is the unit-level floor.
        let (_, notes) = Rules::parse(&[
            entry(PermissionKind::Deny, "WebFetch(domain:evil.example)"),
            entry(PermissionKind::Allow, "WebFetch(domain:*.example.com)"),
            entry(
                PermissionKind::Allow,
                "WebFetch(domain:xn--bcher-kva.example)",
            ),
            entry(PermissionKind::Allow, "WebFetch(domain:127.0.0.*)"),
            entry(PermissionKind::Allow, "WebFetch(domain:[::1])"),
            entry(PermissionKind::Allow, "WebSearch"),
        ]);
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn a_bad_rule_costs_that_rule_and_not_the_boot() {
        let (rules, notes) = Rules::parse(&[
            entry(PermissionKind::Allow, "WebFetch("),
            entry(PermissionKind::Allow, "WebSearch"),
        ]);
        assert_eq!(rules.for_tool("WebSearch"), Some(Decision::Allow));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("settings.local.json"), "{}", notes[0]);
    }

    // -----------------------------------------------------------------------
    // Matching a host
    //
    // Every "does not match" below is a host somebody can register today. If
    // one of them starts matching, a user who approved `apnews.com` has
    // approved an attacker.
    // -----------------------------------------------------------------------

    #[test]
    fn a_domain_rule_does_not_match_a_lookalike_host() {
        let d = match Rule::parse("WebFetch(domain:example.com)").unwrap().spec {
            Spec::Domain(d) => d,
            other => panic!("{other:?}"),
        };
        assert!(d.matches("example.com"));
        // Case and a trailing dot are the same host — `NetworkTarget` already
        // says so, and a rule that disagreed would re-prompt for a host the user
        // approved a second ago.
        assert!(d.matches("EXAMPLE.COM."));
        for near in [
            "evil-example.com",
            "example.com.attacker.net",
            "sub.example.com",
            "exampleXcom",
            "notexample.com",
            "example.como",
            "example.co",
            "wwwexample.com",
        ] {
            assert!(
                !d.matches(near),
                "`{near}` was matched by domain:example.com"
            );
        }
    }

    #[test]
    fn a_leading_star_is_a_strict_subdomain_and_not_the_host_itself() {
        let d = match Rule::parse("WebFetch(domain:*.example.com)").unwrap().spec {
            Spec::Domain(d) => d,
            other => panic!("{other:?}"),
        };
        assert!(d.matches("api.example.com"));
        assert!(d.matches("a.b.example.com"));
        for near in [
            // Documented behaviour, and the one people get wrong: the bare host
            // is *not* covered.
            "example.com",
            "attacker-example.com",
            "api.example.com.attacker.net",
            "example.com.evil.net",
        ] {
            assert!(
                !d.matches(near),
                "`{near}` was matched by domain:*.example.com"
            );
        }
    }

    #[test]
    fn a_star_inside_a_label_cannot_cross_a_dot() {
        // The rule Claude Code documents, and the reason for it: a trailing
        // wildcard that crossed a dot would match domains an attacker could
        // register under any suffix they like.
        let d = match Rule::parse("WebFetch(domain:example.*)").unwrap().spec {
            Spec::Domain(d) => d,
            other => panic!("{other:?}"),
        };
        assert!(d.matches("example.org"));
        assert!(d.matches("example.com"));
        assert!(!d.matches("example.evil.com"));
        assert!(!d.matches("example"));
    }

    #[test]
    fn a_domain_rule_is_scoped_to_its_tool() {
        let r = rules(&[], &[], &["WebFetch(domain:apnews.com)"]);
        assert_eq!(
            r.for_egress("WebFetch", "apnews.com"),
            Some(Decision::Allow)
        );
        // A grant written for one tool does not travel to another. This is the
        // mirror of the session grant, which deliberately *does* — a host a
        // human approved out loud is a host, and a rule in a file names a tool.
        assert_eq!(r.for_egress("WebSearch", "apnews.com"), None);
        // …and it is not permission to run the tool for its own sake. `WebFetch`
        // is read-only so this costs nothing today; it would cost everything the
        // first time a writing tool grew a `network_target`.
        assert_eq!(r.for_tool("WebFetch"), None);
    }

    #[test]
    fn a_bare_tool_rule_answers_both_questions() {
        // "all web searches in the directory", which is what the owner asked
        // for in so many words.
        let r = rules(&[], &[], &["WebSearch"]);
        assert_eq!(r.for_tool("WebSearch"), Some(Decision::Allow));
        assert_eq!(
            r.for_egress("WebSearch", "search.example.com"),
            Some(Decision::Allow)
        );
        assert_eq!(
            r.for_egress("WebSearch", "anywhere.example"),
            Some(Decision::Allow)
        );
        assert_eq!(r.for_tool("Bash"), None);
    }

    // -----------------------------------------------------------------------
    // Precedence
    // -----------------------------------------------------------------------

    #[test]
    fn deny_beats_allow_however_specific_the_allow_is() {
        // The guarantee. If this inverts, every `deny` in every settings file in
        // the world becomes a suggestion.
        let r = rules(&["WebFetch"], &[], &["WebFetch(domain:apnews.com)"]);
        assert_eq!(r.for_egress("WebFetch", "apnews.com"), Some(Decision::Deny));
        // …and in the other arrangement, where the deny is the narrow one.
        let r = rules(&["WebFetch(domain:apnews.com)"], &[], &["WebFetch"]);
        assert_eq!(r.for_egress("WebFetch", "apnews.com"), Some(Decision::Deny));
        // The broad allow still covers everything the narrow deny does not.
        assert_eq!(
            r.for_egress("WebFetch", "docs.rs"),
            Some(Decision::Allow),
            "a narrow deny swallowed a host it does not name"
        );
    }

    #[test]
    fn ask_sits_between_them_and_beats_allow() {
        let r = rules(&[], &["WebFetch"], &["WebFetch(domain:apnews.com)"]);
        assert_eq!(r.for_egress("WebFetch", "apnews.com"), Some(Decision::Ask));
        // …and loses to deny, which is the half that makes it safe to offer.
        let r = rules(&["WebFetch"], &["WebFetch"], &["WebFetch"]);
        assert_eq!(r.for_tool("WebFetch"), Some(Decision::Deny));
    }

    #[test]
    fn silence_is_not_a_decision() {
        // `None` is what sends the call to the human. A rule set that answered
        // `Allow` for a tool nobody wrote a rule about would be the whole gate
        // switched off by an empty file.
        let r = rules(&[], &[], &[]);
        assert_eq!(r.for_tool("Bash"), None);
        assert_eq!(r.for_egress("WebFetch", "docs.rs"), None);
        assert!(r.is_empty());
    }

    // -----------------------------------------------------------------------
    // Writing
    // -----------------------------------------------------------------------

    #[test]
    fn writing_a_rule_keeps_every_other_key_in_the_file() {
        // The defect this is written about: a write path that replaced a shared
        // configuration document and silently deleted another program's key.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.local.json");
        std::fs::write(
            &file,
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash"}]},
                "permissions":{"deny":["WebFetch(domain:evil.example)"],"allow":["WebSearch"]}}"#,
        )
        .unwrap();

        assert!(remember(&file, &Rule::domain("WebFetch", "apnews.com")).unwrap());
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(doc["hooks"]["PreToolUse"].is_array(), "{doc}");
        assert_eq!(
            doc["permissions"]["deny"][0],
            "WebFetch(domain:evil.example)"
        );
        assert_eq!(doc["permissions"]["allow"][0], "WebSearch");
        assert_eq!(
            doc["permissions"]["allow"][1],
            "WebFetch(domain:apnews.com)"
        );
    }

    #[test]
    fn writing_the_same_rule_twice_does_not_write_it_twice() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.local.json");
        let rule = Rule::every_call("WebSearch");
        assert!(remember(&file, &rule).unwrap());
        assert!(!remember(&file, &rule).unwrap());
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(doc["permissions"]["allow"].as_array().unwrap().len(), 1);
        assert_eq!(already_written(&file).len(), 1);
    }

    #[test]
    fn a_file_that_cannot_be_parsed_is_left_exactly_as_it_was() {
        // Refusing to write is the safe direction: the user loses a grant, not
        // somebody else's configuration.
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [
            ("broken.json", "{ not json"),
            ("array.json", "[]"),
            ("perms.json", r#"{"permissions": 7}"#),
            ("allow.json", r#"{"permissions":{"allow":"WebSearch"}}"#),
        ] {
            let file = dir.path().join(name);
            std::fs::write(&file, body).unwrap();
            let err = remember(&file, &Rule::every_call("WebSearch"))
                .err()
                .unwrap_or_else(|| panic!("{name} was written to"))
                .to_string();
            assert!(err.contains(name), "{err}");
            assert_eq!(std::fs::read_to_string(&file).unwrap(), body, "{name}");
        }
    }

    #[test]
    fn a_missing_file_is_created_with_only_what_was_granted_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_for(dir.path());
        assert!(remember(&file, &Rule::domain("WebFetch", "apnews.com")).unwrap());
        let raw = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            raw,
            "{\n  \"permissions\": {\n    \"allow\": [\n      \
             \"WebFetch(domain:apnews.com)\"\n    ]\n  }\n}\n",
            "the file a user opens after their first `remember` is this, and nothing else"
        );
        // No temporary file survives the rename.
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 1, "{left:?}");
    }

    #[test]
    fn what_is_written_is_what_is_read_back_next_boot() {
        // The round trip that makes the feature true rather than merely
        // plausible: a rule written by the prompt is a rule the parser accepts
        // and the matcher honours.
        let dir = tempfile::tempdir().unwrap();
        let file = file_for(dir.path());
        remember(&file, &Rule::domain("WebFetch", "apnews.com")).unwrap();
        let entries: Vec<_> = already_written(&file)
            .into_iter()
            .map(|rule| PermissionEntry {
                rule,
                kind: PermissionKind::Allow,
                source: file.clone(),
            })
            .collect();
        let (rules, notes) = Rules::parse(&entries);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            rules.for_egress("WebFetch", "apnews.com"),
            Some(Decision::Allow)
        );
        assert_eq!(
            rules.for_egress("WebFetch", "apnews.com.attacker.net"),
            None
        );
    }

    // -----------------------------------------------------------------------
    // What one shape of rule may and may not answer
    //
    // Every rule shape answers exactly one of the gate's two questions. Both
    // tests below are written the way the file's own doc argues they should
    // be: an assertion that the wrong shape says *nothing*, beside a control
    // asserting the right shape still says what it always said. A matcher that
    // has stopped answering anything passes the first half on its own.
    // -----------------------------------------------------------------------

    /// A command prefix and a path glob say nothing about where bytes go.
    ///
    /// **Nothing defended this.** Turning `Spec::Command(..) | Spec::Path(..)`
    /// in `for_egress` from `false` to `true` left the whole workspace green,
    /// and it is a widening in both directions at once: an
    /// `allow: ["WebFetch(./cache/**)"]` — a rule about a *file*, and a
    /// perfectly ordinary thing to find in a settings file written for the
    /// other program — would have approved egress to every host on the
    /// internet without a prompt, and a `deny` of the same shape would have cut
    /// the tool off from all of them.
    ///
    /// The rule the module states is that a narrow grant must not answer a
    /// question nobody asked it. This is that rule on the axis where getting it
    /// wrong is exfiltration.
    #[test]
    fn a_command_or_path_rule_says_nothing_about_where_bytes_go() {
        let allowing = rules(&[], &[], &["WebFetch(./cache/**)", "Bash(curl *)"]);
        assert_eq!(
            allowing.for_egress("WebFetch", "evil.example"),
            None,
            "a rule naming a local path approved egress to a host it never mentioned"
        );
        assert_eq!(
            allowing.for_egress("Bash", "evil.example"),
            None,
            "a command prefix approved egress to a host it never mentioned"
        );

        // The same in the other direction, which costs a capability rather than
        // a secret and is still not what the operator wrote.
        let denying = rules(&["WebFetch(./cache/**)"], &[], &[]);
        assert_eq!(
            denying.for_egress("WebFetch", "docs.rs"),
            None,
            "a rule naming a local path denied egress to a host it never mentioned"
        );

        // The controls. A gate that answered `None` to everything would pass
        // all four assertions above and be useless, so the two shapes that
        // *are* about a destination must still answer here.
        let real = rules(&["WebFetch(domain:evil.example)"], &[], &["WebSearch"]);
        assert_eq!(
            real.for_egress("WebFetch", "evil.example"),
            Some(Decision::Deny)
        );
        assert_eq!(
            real.for_egress("WebSearch", "anywhere.example"),
            Some(Decision::Allow)
        );
    }

    /// A path rule is matched against both spellings the tools use.
    ///
    /// `path_of` reads `file_path` and `path`, and its own doc says why: "the
    /// tools use both — `file_path` for the ones that address a single file,
    /// `path` for the ones that address a base." Nothing tested the second
    /// spelling. Deleting `"path"` from that list left every test in the
    /// workspace green, and what it costs is a `deny` rule that silently stops
    /// applying to `Glob` and `Grep` — the two tools whose whole job is to walk
    /// a directory the operator may have written a rule about.
    #[test]
    fn a_path_rule_is_matched_against_both_spellings_of_the_argument() {
        let r = rules(&["Grep(secrets/**)"], &[], &[]);
        for key in ["file_path", "path"] {
            assert_eq!(
                r.for_call("Grep", &serde_json::json!({ key: "secrets/keys.pem" })),
                Some(Decision::Deny),
                "a deny naming a directory did not fire on a call that spelled it `{key}`"
            );
        }
        // The control, and it is the one that matters: a rule that denied every
        // path would pass both assertions above. An unrelated read is not
        // touched by the rule, in either spelling.
        for key in ["file_path", "path"] {
            assert_eq!(
                r.for_call("Grep", &serde_json::json!({ key: "src/main.rs" })),
                None,
                "the deny fired on a path it does not name, spelled `{key}`"
            );
        }
    }
}

// endregion: Tests

#[cfg(test)]
mod bare_rule_tests {
    use super::*;

    /// The three states round-trip through the parser the gate uses, and
    /// clearing removes the rule rather than writing an `ask` entry.
    #[test]
    fn a_bare_rule_writes_reads_back_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        set_bare_rule(&file, "WebSearch", Some(Decision::Deny)).unwrap();
        assert_eq!(bare_rules(&file).get("WebSearch"), Some(&Decision::Deny));
        set_bare_rule(&file, "WebSearch", Some(Decision::Allow)).unwrap();
        assert_eq!(bare_rules(&file).get("WebSearch"), Some(&Decision::Allow));
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(doc["permissions"]["deny"].as_array().unwrap().is_empty());
        set_bare_rule(&file, "WebSearch", None).unwrap();
        assert!(!bare_rules(&file).contains_key("WebSearch"));
    }

    /// A hand-written specifier rule for the same tool is never touched, in
    /// any list, by any state of the bare rule.
    #[test]
    fn a_specifier_rule_survives_every_state_of_the_bare_rule() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(
            &file,
            r#"{"hooks":{"h":1},"permissions":{"allow":["Bash(cargo *)"]}}"#,
        )
        .unwrap();
        for state in [Some(Decision::Deny), Some(Decision::Allow), None] {
            set_bare_rule(&file, "Bash", state).unwrap();
            let doc: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            assert_eq!(doc["permissions"]["allow"][0], "Bash(cargo *)", "{state:?}");
            assert_eq!(doc["hooks"]["h"], 1, "{state:?}");
        }
        // And the specifier rule is invisible to the bare view.
        assert!(!bare_rules(&file).contains_key("Bash"));
    }

    /// A file that is not a settings file is refused whole, and left alone.
    #[test]
    fn a_file_that_is_not_an_object_is_not_written_to() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, "[1,2,3]").unwrap();
        let err = set_bare_rule(&file, "Bash", Some(Decision::Deny)).unwrap_err();
        assert!(err.to_string().contains("Nothing was written"), "{err}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "[1,2,3]");
    }
}
