//! Reading an answer out loud.
//!
//! Output only: no capture, no microphone, nothing leaving the machine, and
//! off unless asked for. The audit that cleared it is
//! `notes/audits/2026-08-27-divergent-emma-fork.md` §3.2.
//!
//! **The branch this came from is macOS-only, through `say(1)`, and the owner
//! runs Windows.** So it lands with a real second implementation or it lands
//! as a setting that honestly says the feature is unavailable here — never as
//! a row that does nothing, which is the defect class
//! `notes/design/coverage-contract.md` exists to prevent. There is a real
//! second implementation: Windows speaks through
//! `System.Speech.Synthesis.SpeechSynthesizer`, driven by a **fixed** script
//! handed to `powershell.exe`. See the Platform region for what that costs and
//! what it cannot reach.
//!
//! Two decisions from the branch worth keeping rather than re-deriving:
//! a voice is stored **by name**, so a machine without it falls back to the
//! system default and a synced settings file still starts; and an enhanced
//! voice is a **recommendation in the listing, never a silent substitution**,
//! because preferring one would talk a user out of the voice they chose in
//! their own accessibility settings.
//!
//! **Nothing is spawned through a shell, on either platform.** The text of an
//! answer is arbitrary model output; it reaches the synthesiser on **stdin**
//! and never as an argument, so there is no argv element for it to be, no flag
//! for a leading `-` to become, and no command line for it to be split out of.
//! `usertools`' module doc argues why splitting a command line is the first
//! half of running text through a shell; this module never builds one. On
//! Windows the *voice name* travels on stdin too — see [`plan_utterance`],
//! which is the one place that decides what a child is handed.
//!
//! **A failure here is a notice, never an abort.** A missing binary, a voice
//! this machine does not have, a non-zero exit: the answer is already on
//! screen and this is decoration. Nothing in this file returns an error to the
//! agent loop, and nothing in it blocks the loop — [`speak`] spawns and
//! returns, and a thread reaps.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};

// region: What gets spoken
// ---------------------------------------------------------------------------
// Markdown to speech
//
// A pure function, and the part of this module a test can pin down
// completely. Everything below it touches a process or a machine's voice list.
// Nothing in this region is platform-specific: `say` and SAPI both want one
// line of prose and both stutter on newlines.
// ---------------------------------------------------------------------------

/// How much of an answer gets read aloud, in bytes of the stripped text.
///
/// Both synthesisers run near 175–200 words per minute, so 2,000 characters of
/// ordinary English prose is about 320 words — near two minutes. The screen
/// already has the whole answer, so the cut exists to stop a very long one
/// becoming a monologue, not to keep the spoken part short.
///
/// It was 400 on the branch, which is 22 seconds, and the owner heard that as
/// the voice tiring partway through rather than as a limit: an answer of a few
/// ordinary paragraphs was cut every single time. The right value is a fact
/// about the listener, so `voice.spoken_limit` overrides this and this is the
/// default. The cut lands on a sentence boundary because a voice stopping
/// mid-clause sounds like a crash rather than a limit.
pub const SPOKEN_LIMIT: usize = 2_000;

/// What is appended when the answer was longer than [`SPOKEN_LIMIT`].
pub const TRUNCATION_NOTE: &str = "… full answer on screen";

/// What is spoken where a fenced code block was.
///
/// Reading a code fence aloud is unusable — punctuation, indentation and
/// identifiers become noise — so the fence is replaced by two words that tell
/// the listener something was skipped rather than silently dropping it.
pub const CODE_OMITTED: &str = "code omitted.";

/// Markdown in, one line of speakable prose out.
///
/// Three passes, in this order and for a reason: fences go first because
/// their *contents* must never reach the later passes (a `#` inside a shell
/// script is not a heading), inline markers go second, and whitespace
/// collapses last because the first two leave gaps behind them.
pub fn to_speech(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut in_fence = false;
    for line in markdown.lines() {
        // Only backticks open a fence. `~~~` is legal CommonMark and vanishingly
        // rare in model output, while `~~strikethrough~~` is not — treating a
        // tilde run as a fence would swallow a paragraph to catch a case that
        // does not arrive.
        if line.trim_start().starts_with("```") {
            if !in_fence {
                out.push(' ');
                out.push_str(CODE_OMITTED);
            }
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            // A streamed answer can end mid-fence. Everything to the end of
            // the text stays swallowed, which is the safe direction: reading
            // half a shell command aloud is the failure worth avoiding.
            continue;
        }
        out.push(' ');
        out.push_str(&speakable_line(line));
    }
    collapse(&out)
}

/// One non-fence line, with the markers removed and the words kept.
fn speakable_line(line: &str) -> String {
    let without_links = unlink(line);
    let mut out = String::with_capacity(without_links.len());
    for word in without_links.split_whitespace() {
        // A URL read aloud is a minute of "slash, h, t, t, p" and carries
        // nothing a listener can act on; the screen has it.
        if word.starts_with("http://") || word.starts_with("https://") {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    // `_` is deliberately absent. It is markdown emphasis *and* the middle of
    // `max_turns`, and mangling an identifier the answer is about is worse
    // than reading one stray underscore — which neither synthesiser voices.
    let stripped: String = out
        .chars()
        .filter(|c| !matches!(c, '`' | '*' | '~' | '#' | '|' | '>' | '[' | ']'))
        .collect();
    // Leading list and quote markers, after the marker characters are gone:
    // what is left of `- item` is ` item`, but `1. item` keeps its number,
    // which reads correctly.
    stripped.trim_start_matches(['-', '+', ' ']).to_string()
}

/// `[text](url)` becomes `text`. Hand-scanned rather than pattern-matched:
/// the link body can contain brackets and this only has to be right for the
/// shape a model actually writes.
fn unlink(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let Some(close_rel) = rest[open..].find("](") else {
            break;
        };
        let close = open + close_rel;
        let after = close + 2;
        let Some(end_rel) = rest[after..].find(')') else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..close]);
        rest = &rest[after + end_rel + 1..];
    }
    out.push_str(rest);
    out
}

/// Every run of whitespace becomes one space, and the ends are trimmed.
///
/// Not cosmetic: both synthesisers pause on a newline, so an answer full of
/// them is read in a stutter.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cut `text` to at most the limit in force at a sentence boundary, appending
/// [`TRUNCATION_NOTE`] when anything was dropped.
pub fn truncate(text: &str) -> String {
    truncate_to(text, state().spoken_limit)
}

/// [`truncate`] against an explicit limit, which is what makes the cut
/// testable without reaching for the process-wide state.
///
/// The budget is one byte short of the limit so that the space joining the
/// speech to the note still fits inside it — an off-by-one here is a panic on
/// the next multi-byte answer, not a rounding error.
pub fn truncate_to(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let budget = limit.saturating_sub(1);
    // The largest char boundary at or below the budget. Slicing at `budget`
    // directly panics the moment an answer contains an accent or a dash.
    let mut end = budget;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let head = &text[..end];
    // Last sentence end in the head. A full stop with no space after it is
    // usually a decimal or a file extension, so the boundary is punctuation
    // *followed by* whitespace, or punctuation at the very end of the head.
    let boundary = head
        .char_indices()
        .rfind(|(i, c)| {
            matches!(c, '.' | '!' | '?')
                && head[i + c.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
        .map(|(i, c)| i + c.len_utf8());
    let spoken = match boundary {
        Some(at) => &head[..at],
        // No sentence ended inside the budget: one run-on, or a language this
        // heuristic does not punctuate. Cutting at the last word boundary is
        // worse than a sentence and much better than reading the whole thing.
        None => match head.rfind(char::is_whitespace) {
            Some(at) => &head[..at],
            None => head,
        },
    };
    format!("{} {}", spoken.trim_end(), TRUNCATION_NOTE)
}

// endregion: What gets spoken

// region: Platform
// ---------------------------------------------------------------------------
// Two synthesisers, one shape
//
// macOS speaks through `say(1)`; Windows speaks through
// `System.Speech.Synthesis.SpeechSynthesizer`, reached by handing
// `powershell.exe` a script that is a **compile-time constant**. Everything
// that varies — the voice name and the answer — arrives on that script's
// stdin, so no value from settings and no byte of model output is ever part of
// a command line. That is the whole security argument for the Windows arm and
// it is why [`plan_utterance`] is a pure function with tests on it.
//
// **Neither program is looked for on PATH**, which is a deliberate departure
// from `usertools`' PATHEXT-aware probe rather than an oversight. `usertools`
// resolves programs the *user names* — an editor, a shell — so it must search
// the way the user's shell would. Both programs here are parts of their
// operating system at fixed locations, and the branch already made this
// argument for `say`: "a Linux box that happens to have a program called `say`
// on PATH is not what the owner asked for". A `powershell.exe` somebody
// dropped earlier on PATH is the same thing. So each is an absolute path,
// checked with one `exists`, and there is no second answer to the PATHEXT
// question anywhere in this file.
//
// **What Windows cannot reach.** `System.Speech` sees only the SAPI5 voices
// registered under `HKLM\SOFTWARE\Microsoft\Speech\Voices\Tokens`. The modern,
// far more natural OneCore voices live under `Speech_OneCore\Voices\Tokens`
// and are invisible to it. Measured on the owner's box, 2026-08-26:
// `GetInstalledVoices()` returned two — `Microsoft David Desktop` and
// `Microsoft Zira Desktop`, both `en-US` — while the OneCore key held three
// tokens (`…DavidM`, `…MarkM`, `…ZiraM`). So `Mark` is installed on that
// machine and this module cannot offer it. Reaching OneCore needs the WinRT
// `SpeechSynthesizer`, which returns a stream the caller must then play, which
// is an audio pipeline this module does not have and does not want. The
// listing is therefore honest and short rather than complete.
//
// A consequence worth stating plainly: **no Windows voice is ever "natural"**
// in the sense the branch's Mac listing means. SAPI exposes no Enhanced or
// Premium marker and the compact Desktop voices are all there is. The
// recommendation half of the branch's decision simply has nothing to
// recommend here, and it says so rather than inventing a ranking.
// ---------------------------------------------------------------------------

/// Which synthesiser this build talks to.
///
/// Carried as a value rather than read from `cfg!` at each use, so that every
/// decision below is exercised for **both** platforms by tests running on
/// either one. Only [`backend`] is `cfg`-dependent, and it is one expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// macOS `say(1)`.
    Say,
    /// Windows `System.Speech`, via a fixed `powershell.exe` script.
    Sapi,
}

/// The backend this build would use, or `None` where there is no supported
/// synthesiser. Being `Some` says nothing about whether it *works* — that is
/// [`report`], which actually asks the machine.
pub const fn backend() -> Option<Backend> {
    if cfg!(target_os = "macos") {
        Some(Backend::Say)
    } else if cfg!(windows) {
        Some(Backend::Sapi)
    } else {
        None
    }
}

/// What `/voice` and the settings row say where there is no synthesiser at all.
pub const NO_PLATFORM: &str = "voice output needs macOS (say) or Windows (System.Speech)";

const SAY: &str = "/usr/bin/say";

/// Where Windows keeps its own PowerShell, relative to `%SystemRoot%`.
///
/// Windows PowerShell 5.1 and not `pwsh`: `Add-Type -AssemblyName
/// System.Speech` resolves against the .NET Framework assemblies that ship
/// with the OS, and PowerShell 7 runs on .NET where `System.Speech` is a
/// separate package that may not be present. Preferring the one that is part
/// of the operating system is the same reasoning as not searching PATH.
const POWERSHELL_TAIL: &str = r"System32\WindowsPowerShell\v1.0\powershell.exe";

/// Flags common to both scripts. `-NoProfile` because somebody's profile is
/// not this process's business and can print; `-NonInteractive` because there
/// is nobody at this child's keyboard and a prompt would hang it forever.
const PS_FLAGS: [&str; 2] = ["-NoProfile", "-NonInteractive"];

/// Enumerate SAPI voices, one `name<TAB>locale` line each, as UTF-8 bytes.
///
/// Written to the raw stdout stream rather than with `Write-Output` because
/// PowerShell 5.1 encodes a redirected stdout in the console codepage, which
/// mangles any voice name outside ASCII. The disabled voices are skipped:
/// selecting one fails, and a listing that offers a voice the speak path
/// cannot use is the availability contract broken in the direction nobody
/// checks.
const LIST_SCRIPT: &str = r#"Add-Type -AssemblyName System.Speech; $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; $sb = New-Object Text.StringBuilder; foreach ($v in $s.GetInstalledVoices()) { if ($v.Enabled) { $i = $v.VoiceInfo; [void]$sb.Append($i.Name).Append([char]9).Append($i.Culture.Name).Append([char]10) } }; $s.Dispose(); $o = [Console]::OpenStandardOutput(); $b = [Text.Encoding]::UTF8.GetBytes($sb.ToString()); $o.Write($b, 0, $b.Length); $o.Flush()"#;

/// Speak what arrives on stdin: first line the voice name (empty for the
/// system default), everything after it the text.
///
/// Read as raw bytes and decoded as UTF-8 explicitly, for the mirror of the
/// reason [`LIST_SCRIPT`] writes bytes: `[Console]::In` on PowerShell 5.1
/// decodes a redirected stdin in the console codepage, which turns every
/// accented word in an answer into mojibake.
///
/// `SelectVoice` is wrapped in a `try` that falls through to the system
/// default. [`restore`] already drops a name this machine does not have, so
/// this is the second gate rather than the first — and it is what makes a
/// voice uninstalled *between* startup and now a quieter answer instead of a
/// silent one.
const SPEAK_SCRIPT: &str = r#"Add-Type -AssemblyName System.Speech; $si = [Console]::OpenStandardInput(); $ms = New-Object IO.MemoryStream; $si.CopyTo($ms); $in = [Text.Encoding]::UTF8.GetString($ms.ToArray()); $i = $in.IndexOf([char]10); if ($i -lt 0) { exit 0 }; $v = $in.Substring(0, $i).Trim(); $t = $in.Substring($i + 1); $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; if ($v) { try { $s.SelectVoice($v) } catch {} }; $s.Speak($t); $s.Dispose()"#;

/// The absolute path to the backend's program, or `None` when it is not where
/// the operating system keeps it.
fn program_for(backend: Backend) -> Option<PathBuf> {
    match backend {
        Backend::Say => {
            let p = PathBuf::from(SAY);
            p.exists().then_some(p)
        }
        Backend::Sapi => {
            // `windir` is the older spelling and both are set on every
            // supported Windows; taking either means a machine that has lost
            // one of them still speaks.
            let root = std::env::var_os("SystemRoot")
                .or_else(|| std::env::var_os("windir"))
                .unwrap_or_else(|| OsString::from(r"C:\Windows"));
            let p = PathBuf::from(root).join(POWERSHELL_TAIL);
            p.exists().then_some(p)
        }
    }
}

/// One installed voice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    /// Exactly as the system prints it. This is what gets stored, and storing
    /// anything else is what the branch decided against — see the module doc.
    pub name: String,
    /// `en_US`, `en-GB`, `fr_CA`. The two spellings are the two platforms'.
    pub locale: String,
    /// Whether the platform marks this as one of the large, natural-sounding
    /// downloads rather than the compact voice that ships in the image.
    ///
    /// Set by the parser rather than guessed from the name at each use,
    /// because the two platforms answer it differently and only the parser
    /// knows which one produced the row: macOS marks it in the name
    /// (`Samantha (Enhanced)`, `Ava (Premium)`), and SAPI has no such concept
    /// at all, so on Windows this is always false and honestly so.
    pub natural: bool,
}

impl Voice {
    /// Whether this voice speaks some variety of English.
    pub fn is_english(&self) -> bool {
        self.locale.starts_with("en")
    }
}

/// Parse the output of `say -v '?'`.
///
/// The format is `Name<pad>locale<pad># sample`, and the name contains spaces
/// and parentheses — `Eddy (English (US))`, `Bad News`. So the split is from
/// the *right*: drop the sample at the `#`, take the last whitespace-separated
/// token as the locale, and everything before it is the name. Splitting from
/// the left would name a voice `Eddy`.
pub fn parse_say_listing(listing: &str) -> Vec<Voice> {
    let mut out = Vec::new();
    for line in listing.lines() {
        let head = line.split('#').next().unwrap_or("").trim_end();
        let Some((name, locale)) = head.rsplit_once(char::is_whitespace) else {
            continue;
        };
        let (name, locale) = (name.trim(), locale.trim());
        // A locale is `en_US` or `en`. Anything else means this line was not a
        // voice row, and a build that guessed would put a stray word in the
        // listing somebody picks from.
        if name.is_empty() || !looks_like_a_locale(locale) {
            continue;
        }
        out.push(Voice {
            natural: name.contains("(Enhanced)") || name.contains("(Premium)"),
            name: name.to_string(),
            locale: locale.to_string(),
        });
    }
    out
}

/// Parse [`LIST_SCRIPT`]'s output: `name<TAB>locale` per line.
///
/// Tab-separated and not whitespace-separated, because every SAPI name has
/// spaces in it (`Microsoft David Desktop`) and the right-split that works for
/// `say` would be answering a different question here. The script chooses the
/// separator, so this parse is exact rather than tolerant: a line without a
/// tab did not come from the script and is dropped rather than guessed at.
pub fn parse_sapi_listing(listing: &str) -> Vec<Voice> {
    let mut out = Vec::new();
    for line in listing.lines() {
        let Some((name, locale)) = line.split_once('\t') else {
            continue;
        };
        let (name, locale) = (name.trim(), locale.trim());
        if name.is_empty() || !looks_like_a_locale(locale) {
            continue;
        }
        out.push(Voice {
            name: name.to_string(),
            locale: locale.to_string(),
            // SAPI has no Enhanced/Premium axis. See the region comment: the
            // voices that would qualify are the OneCore ones, which
            // System.Speech cannot see at all.
            natural: false,
        });
    }
    out
}

fn looks_like_a_locale(s: &str) -> bool {
    s.len() >= 2
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Whether the backend's listing parses with this parser.
fn parse_listing(backend: Backend, listing: &str) -> Vec<Voice> {
    match backend {
        Backend::Say => parse_say_listing(listing),
        Backend::Sapi => parse_sapi_listing(listing),
    }
}

/// The arguments that ask a backend what voices it has. Pure, so the shape is
/// asserted on either host.
fn listing_args(backend: Backend) -> Vec<OsString> {
    match backend {
        Backend::Say => vec![OsString::from("-v"), OsString::from("?")],
        Backend::Sapi => PS_FLAGS
            .iter()
            .map(OsString::from)
            .chain([OsString::from("-Command"), OsString::from(LIST_SCRIPT)])
            .collect(),
    }
}

/// What this machine can actually do, decided once and remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The synthesiser this build would use, if there is one.
    pub backend: Option<Backend>,
    /// The voices it offered. Empty when `blocked` is set.
    pub voices: Vec<Voice>,
    /// Why voice is unavailable, or empty when it is available.
    ///
    /// A sentence a person can act on, in the register `usertools`' unavailable
    /// details use: what was looked for and not found, never a bare `false`.
    /// This is what a settings row shows when it must be live-and-read-only
    /// rather than editable.
    pub blocked: String,
}

impl Report {
    /// Whether an answer can be read aloud on this machine right now.
    pub fn ready(&self) -> bool {
        self.blocked.is_empty()
    }
}

/// Turn a probe's outcome into a [`Report`]. Pure: the process lives in
/// [`probe`] and this decides what the result means, so every arm — including
/// the one this host cannot produce — is asserted by a test.
///
/// **A backend that lists no voices is blocked, not ready.** That is the check
/// that closes the gap a plain "is the binary there" would leave: a
/// `powershell.exe` whose `Add-Type -AssemblyName System.Speech` fails exits
/// zero with empty stdout, and a build that called that available would report
/// voice on and make no sound — the outcome
/// [`restore`] already argues is the worst of the three.
fn build_report(backend: Option<Backend>, probed: Result<Vec<Voice>, String>) -> Report {
    let Some(backend) = backend else {
        return Report {
            backend: None,
            voices: Vec::new(),
            blocked: NO_PLATFORM.to_string(),
        };
    };
    match probed {
        Err(why) => Report {
            backend: Some(backend),
            voices: Vec::new(),
            blocked: why,
        },
        Ok(voices) if voices.is_empty() => Report {
            backend: Some(backend),
            voices,
            blocked: match backend {
                Backend::Say => "say(1) listed no voices".to_string(),
                Backend::Sapi => format!(
                    "{POWERSHELL_TAIL} listed no System.Speech voices (install one under \
                     Settings → Time & Language → Speech)"
                ),
            },
        },
        Ok(voices) => Report {
            backend: Some(backend),
            voices,
            blocked: String::new(),
        },
    }
}

/// Ask the machine, once. The process costs real time — around 260ms for the
/// PowerShell arm, measured on the owner's box on 2026-08-26 — which is why it
/// is behind a `OnceLock` and why nothing calls it on a draw.
fn probe(backend: Backend) -> Result<Vec<Voice>, String> {
    let Some(program) = program_for(backend) else {
        return Err(match backend {
            Backend::Say => format!("{SAY} is not on this machine"),
            Backend::Sapi => format!(r"%SystemRoot%\{POWERSHELL_TAIL} is not on this machine"),
        });
    };
    let mut cmd = Command::new(&program);
    cmd.args(listing_args(backend))
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    no_window(&mut cmd);
    match cmd.output() {
        Ok(out) => Ok(parse_listing(
            backend,
            &String::from_utf8_lossy(&out.stdout),
        )),
        Err(e) => Err(format!("{} could not be run: {e}", program.display())),
    }
}

/// What this machine can do, probed on first ask and remembered for the life
/// of the process.
///
/// Cached for the reason `usertools` caches its PATH lookups: a voice
/// installed mid-session appears after a restart, which the cache makes true
/// by construction rather than intermittently — and here it also keeps a
/// quarter-second process off any path that might be asked twice.
pub fn report() -> &'static Report {
    static REPORT: OnceLock<Report> = OnceLock::new();
    REPORT.get_or_init(|| build_report(backend(), backend().map_or(Ok(Vec::new()), probe)))
}

/// The voices this machine offers, in the order the platform listed them.
pub fn installed_voices() -> &'static [Voice] {
    &report().voices
}

/// Why a string is not an installed voice, or `None`.
///
/// A membership check with the valid set printed, the ruling
/// `session_command::refuse_an_unknown_theme` makes — except that the set here
/// can be 184 rows on a stock Mac, so the refusal names a *sample* and points
/// at the listing for the rest. Takes the list rather than querying it so the
/// wording is testable on a machine with different voices.
pub fn refuse_an_unknown_voice(name: &str, installed: &[Voice]) -> Option<String> {
    if installed.iter().any(|v| v.name == name) {
        return None;
    }
    // Case is the likely typo and matching it loosely would make `/voice
    // samantha` work while the listing shows `Samantha` — the disagreement
    // `/theme` refuses. So it is still a refusal, but it names the fix.
    if let Some(v) = installed.iter().find(|v| v.name.eq_ignore_ascii_case(name)) {
        return Some(format!(
            "`{name}` is not a voice, so nothing was changed — but `{}` is, and names are matched \
             exactly.",
            v.name
        ));
    }
    let sample: Vec<&str> = installed
        .iter()
        .filter(|v| v.natural)
        .chain(installed.iter().filter(|v| v.is_english()))
        .map(|v| v.name.as_str())
        .take(4)
        .collect();
    // The sample is uncurated, and it has to be: neither platform exposes a
    // field separating a speech voice from a novelty one, so `Bad News` and
    // `Bahh` sort in beside `Albert`. A hardcoded list of the serious voices
    // would read better and would be wrong on the next OS release, which is the
    // worse failure — so the refusal also names the option that is always
    // right, and that is the system default.
    Some(if sample.is_empty() {
        format!(
            "`{name}` is not a voice on this machine, so nothing was changed. `/voice on` with no \
             name uses {}.",
            system_default_label()
        )
    } else {
        format!(
            "`{name}` is not a voice on this machine, so nothing was changed. It has {} and {} \
             more — /voice on its own lists them. `/voice on` with no name uses {}.",
            sample.join(", "),
            installed.len().saturating_sub(sample.len()),
            system_default_label()
        )
    })
}

/// What the unset voice is called in every line Emma prints, and where the
/// person changes it.
///
/// Platform-specific because the two places are different rooms: on macOS the
/// system default is the **only** seat a Siri voice can occupy, which is the
/// whole reason this module never substitutes an enhanced voice for it.
pub fn system_default_label() -> &'static str {
    match backend() {
        Some(Backend::Say) => {
            "the system default (Settings → Accessibility → Spoken Content, where a Siri voice \
             lives)"
        }
        Some(Backend::Sapi) => "the system default (Settings → Time & Language → Speech)",
        None => "the system default",
    }
}

// endregion: Platform

// region: The process
// ---------------------------------------------------------------------------
// Speaking
//
// One utterance at a time, process-wide. The state is global for the reason
// `palette::ACTIVE` is: `/voice` sets it from the command loop and the answer
// seam reads it, and threading a handle between those two would mean a
// parameter on every function between them.
// ---------------------------------------------------------------------------

/// Whether answers are read aloud, and with which voice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub on: bool,
    /// Bytes read aloud before the rest is left on screen. Seeded from
    /// `voice.spoken_limit`, defaulting to [`SPOKEN_LIMIT`].
    pub spoken_limit: usize,
    /// The voice by name, or `None` for the system default.
    ///
    /// `None` is not "no voice" — it is the deliberate default. See
    /// [`system_default_label`].
    pub voice: Option<String>,
}

impl Default for State {
    /// Voice off, system voice, and the built-in limit. Derived it would be a
    /// zero limit, which is a voice that is on and says nothing.
    fn default() -> Self {
        Self {
            on: false,
            spoken_limit: SPOKEN_LIMIT,
            voice: None,
        }
    }
}

impl State {
    /// How the voice in force is named in a receipt or a listing.
    pub fn voice_label(&self) -> String {
        match &self.voice {
            Some(v) => v.clone(),
            None => system_default_label().to_string(),
        }
    }
}

struct Speaker {
    state: Mutex<State>,
    /// The synthesiser this process started and has not yet reaped. Behind its
    /// own lock so a follow-up question can kill it without waiting on
    /// whatever `/voice` is doing to the state.
    current: Mutex<Option<Child>>,
}

fn speaker() -> &'static Speaker {
    static SPEAKER: OnceLock<Speaker> = OnceLock::new();
    SPEAKER.get_or_init(|| Speaker {
        state: Mutex::new(State::default()),
        current: Mutex::new(None),
    })
}

/// What is in force now.
pub fn state() -> State {
    speaker()
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Turn reading aloud on or off. Turning it off silences what is being said:
/// somebody typing `/voice off` wants the room quiet now, not at the end of
/// the sentence.
pub fn set_on(on: bool) {
    speaker().state.lock().unwrap_or_else(|e| e.into_inner()).on = on;
    if !on {
        silence();
    }
}

/// Choose a voice, or `None` for the system default. Not validated here —
/// [`refuse_an_unknown_voice`] is the gate, and it runs where it can print.
pub fn set_voice(voice: Option<String>) {
    speaker()
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .voice = voice;
}

/// Restore what was stored in the settings file, at startup.
///
/// A stored name this machine does not have is dropped to the system default
/// rather than kept. Keeping it would make every utterance select a voice that
/// is not there — on macOS a failed `say -v`, which is **silence**, so voice
/// would report itself on and produce no sound, the worst of the three
/// outcomes. A settings file synced from another machine therefore still
/// talks. (The Windows script also falls back internally, which makes this the
/// first of two gates rather than the only one; both are wanted, because only
/// this one can also correct the *stored* value.)
///
/// The check costs the one cached probe [`report`] does, and only when a name
/// is actually stored — so the common cases, voice off or voice on with the
/// system default, pay nothing for it.
pub fn restore(on: bool, voice: Option<String>, spoken_limit: Option<usize>) {
    let voice = match voice {
        Some(name) if !installed_voices().iter().any(|v| v.name == name) => None,
        other => other,
    };
    let mut state = speaker().state.lock().unwrap_or_else(|e| e.into_inner());
    state.on = on;
    state.voice = voice;
    // Zero would speak nothing at all while reporting the voice as on, which
    // is the one outcome worse than a cut that is too short.
    state.spoken_limit = spoken_limit.filter(|n| *n > 0).unwrap_or(SPOKEN_LIMIT);
}

/// Stop whatever is being said, and reap it.
pub fn silence() {
    let mut slot = speaker().current.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut child) = slot.take() {
        // Kill *then* wait: the kill is what stops the sound, and the wait is
        // what stops the zombie. Both children die immediately, so this does
        // not block the caller in any way a person notices.
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// A fully-decided utterance: the program, its argv, and what goes down its
/// stdin. Built element by element, and there is deliberately no constructor
/// taking a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Utterance {
    program: PathBuf,
    args: Vec<OsString>,
    stdin: String,
}

/// Decide what the synthesiser is handed. **The one place the answer text and
/// the voice name meet a child process**, which is why it is pure and has
/// tests asserting what is *not* in `args`.
///
/// macOS: the voice is an argv element (`-v`, name) and the text is stdin.
/// `say` with no text operand reads stdin, which closes the hole an argv would
/// leave — an answer beginning with `-` would otherwise be parsed as a flag.
///
/// Windows: **both** travel on stdin, first line the voice and the rest the
/// text, because the argv is a fixed script and putting a settings value into
/// it would be building a command line out of user input. The voice name has
/// its control characters stripped before it goes in: a name containing a
/// newline would otherwise move the boundary between the two fields and let
/// the first line of the *answer* be read as a voice — a small injection, into
/// a script rather than a shell, and closed at the only place it could open.
fn plan_utterance(
    backend: Backend,
    program: PathBuf,
    voice: Option<&str>,
    text: &str,
) -> Utterance {
    match backend {
        Backend::Say => Utterance {
            program,
            args: match voice {
                Some(v) => vec![OsString::from("-v"), OsString::from(v)],
                None => vec![],
            },
            stdin: text.to_string(),
        },
        Backend::Sapi => {
            let name: String = voice
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            Utterance {
                program,
                args: PS_FLAGS
                    .iter()
                    .map(OsString::from)
                    .chain([OsString::from("-Command"), OsString::from(SPEAK_SCRIPT)])
                    .collect(),
                stdin: format!("{name}\n{text}"),
            }
        }
    }
}

/// Read one finished answer aloud, if voice is on.
///
/// **A new utterance kills the previous one.** The owner asking a follow-up
/// before the first answer finished must not queue a second speech behind it;
/// the older answer is on screen and the newer one is what they are waiting
/// for. Not `killall say` — that would silence anything else on the machine
/// using the same binary, which is not this process's business.
///
/// **This never blocks the loop, and never waits on the child.** The spawn
/// returns as soon as the OS has the process; the text is written to a pipe
/// that is then closed, which the synthesiser reads to EOF; and a reaper
/// thread — not the drawing loop — collects the exit. A long answer and a slow
/// synthesiser therefore cost the loop nothing at all: the *speaking* takes as
/// long as it takes, in another process, and the next question kills it. The
/// one measured cost on the Windows path is latency before the first word,
/// because `powershell.exe` has to start: about 260ms on the owner's box.
///
/// A failure is silence and not a message. The platform was already reported
/// once, by `/voice` and the settings row, and a warning per answer would put
/// a row into the frame for something the user cannot fix mid-conversation.
pub fn speak(answer: &str) {
    let state = state();
    // Checked before anything asks the machine: with voice off — the default —
    // this function must not cost a probe, and `report()` is a process.
    if !state.on {
        return;
    }
    let Some(backend) = backend() else { return };
    if !report().ready() {
        return;
    }
    let Some(program) = program_for(backend) else {
        return;
    };
    let text = truncate(&to_speech(answer));
    if text.trim().is_empty() {
        return;
    }
    silence();
    let plan = plan_utterance(backend, program, state.voice.as_deref(), &text);
    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.args)
        .stdin(Stdio::piped())
        // Neither child prints anything on the happy path, but a future
        // version that warned would land on rows ratatui owns. Nulled for the
        // same reason `usertools` nulls a detached spawn.
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    no_window(&mut cmd);
    let Ok(mut child) = cmd.spawn() else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(plan.stdin.as_bytes());
        // Dropped here, closing the pipe — both scripts read to EOF and would
        // wait forever otherwise.
    }
    *speaker().current.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
    reap_when_it_finishes();
}

/// Keep a child off Emma's console.
///
/// `powershell.exe` is a console program, and a console program spawned from
/// the interactive frame attaches to the console the alternate screen is
/// drawn on. `CREATE_NO_WINDOW` gives it a console of its own with no window —
/// the same flag and the same argument as `usertools`' detached arm, whose
/// module doc carries the measurements. Stdout and stderr are nulled as well,
/// so even that console gets nothing.
#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Unix has no window to suppress: `say` draws nothing, and its output streams
/// are already nulled by the caller.
#[cfg(not(windows))]
fn no_window(_cmd: &mut Command) {}

/// The waiter thread, in the shape `Frame::launch_tool` established: never
/// wait on a child from the loop that draws.
///
/// It polls rather than owning the child, because the killer in [`silence`]
/// has to be able to take it. One thread that lives as long as the utterance,
/// asking a cheap question six times a second.
fn reap_when_it_finishes() {
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_millis(150));
        let mut slot = speaker().current.lock().unwrap_or_else(|e| e.into_inner());
        match slot.as_mut() {
            // Somebody else took it — `silence`, or a newer utterance. Either
            // way it is reaped and this thread has nothing left to do.
            None => return,
            Some(child) => match child.try_wait() {
                Ok(Some(_)) => {
                    *slot = None;
                    return;
                }
                Ok(None) => {}
                Err(_) => return,
            },
        }
    });
}

// endregion: The process

#[cfg(test)]
mod tests {
    use super::*;

    // ---- What gets spoken ----------------------------------------------

    #[test]
    fn a_fenced_block_becomes_two_words_and_not_its_contents() {
        let out = to_speech(
            "Here is the fix:\n\n```rust\nfn main() { println!(\"hi\"); }\n```\n\nThat is all.",
        );
        assert!(!out.contains("println"), "the fence was read aloud: {out}");
        assert!(!out.contains("```"), "a fence marker survived: {out}");
        assert!(out.contains("code omitted"), "the skip was silent: {out}");
        assert!(out.contains("Here is the fix"));
        assert!(out.contains("That is all"));
    }

    #[test]
    fn an_unclosed_fence_swallows_the_rest_rather_than_reading_it() {
        // A streamed answer can end mid-fence. Reading the tail aloud is the
        // failure this guards; ending the utterance early is the tolerable one.
        let out = to_speech("Try this:\n\n```\nrm -rf /\n");
        assert!(!out.contains("rm -rf"), "an unclosed fence leaked: {out}");
        assert!(out.contains("code omitted"));
    }

    #[test]
    fn inline_formatting_is_removed_but_the_words_inside_it_are_kept() {
        let out = to_speech("Set **`max_turns`** to *four*, not ~~three~~.");
        assert!(!out.contains('`'));
        assert!(!out.contains('*'));
        assert!(!out.contains('~'));
        assert!(
            out.contains("max_turns"),
            "the word inside was dropped: {out}"
        );
        assert!(out.contains("four"));
        assert!(out.contains("three"));
    }

    #[test]
    fn headings_bullets_and_links_read_as_their_words() {
        let out =
            to_speech("## The finding\n\n- see [the docs](https://example.com/a/b)\n- and #2\n");
        assert!(!out.contains('#'), "a heading marker survived: {out}");
        assert!(
            !out.contains("https://"),
            "a URL is being read aloud: {out}"
        );
        assert!(out.contains("The finding"));
        assert!(out.contains("the docs"));
    }

    /// **A bare URL, which the test above could not see.** That one asserts no
    /// `https://` survives a markdown link — and `unlink` alone satisfies it,
    /// so deleting the URL filter in [`speakable_line`] entirely left the
    /// suite green. A model writes bare URLs constantly, and each one is a
    /// minute of "aitch tee tee pee colon slash slash".
    #[test]
    fn a_bare_url_is_not_read_aloud_but_the_prose_around_it_is() {
        let out = to_speech("See https://example.com/a/b for the rest, then stop.");
        assert!(!out.contains("example.com"), "a bare URL survived: {out}");
        assert!(out.contains("See"));
        assert!(out.contains("for the rest, then stop."));
    }

    #[test]
    fn whitespace_collapses_to_single_spaces() {
        let out = to_speech("one\n\n\ntwo   three\t\tfour\n");
        assert_eq!(out, "one two three four");
    }

    #[test]
    fn an_answer_that_is_only_a_fence_still_says_something() {
        // The empty utterance is the bug: a spawn that makes no sound leaves
        // the user wondering whether voice is on at all.
        let out = to_speech("```\nls -la\n```");
        assert_eq!(out, "code omitted.");
    }

    #[test]
    fn a_short_answer_is_spoken_whole_with_no_note() {
        let short = "The run lost four rows to a broken grader.";
        assert_eq!(truncate_to(short, SPOKEN_LIMIT), short);
        assert!(!truncate_to(short, SPOKEN_LIMIT).contains("full answer on screen"));
    }

    /// Two ordinary paragraphs are spoken whole. At the branch's old 400 byte
    /// limit they were cut every time, which the owner heard as the voice
    /// tiring partway through rather than as a limit being reached.
    #[test]
    fn an_answer_of_a_few_paragraphs_is_spoken_whole_by_default() {
        let para = "The run lost four rows to a broken grader, and the answers are still on \
                    disk, so a regrade recovers them without any model time at all. ";
        let answer = para.repeat(6);
        assert!(
            answer.len() > 400,
            "fixture is not longer than the old limit"
        );
        assert_eq!(truncate_to(&answer, SPOKEN_LIMIT), answer);
    }

    #[test]
    fn a_long_answer_is_cut_at_a_sentence_boundary_under_the_limit() {
        let sentence = "This suite separates fifty seven percent of pairs. ";
        let long = sentence.repeat(20);
        // An explicit limit, because this test is about where the cut lands
        // and not about what the default happens to be today.
        const LIMIT: usize = 400;
        let out = truncate_to(&long, LIMIT);
        let spoken = out
            .strip_suffix(TRUNCATION_NOTE)
            .expect("no truncation note");
        assert!(
            spoken.trim_end().ends_with('.'),
            "the cut landed mid-sentence: {spoken:?}"
        );
        assert!(
            spoken.len() <= LIMIT,
            "{} bytes spoken, limit {LIMIT}",
            spoken.len()
        );
        assert!(spoken.starts_with("This suite separates"));
    }

    #[test]
    fn a_long_answer_with_no_sentence_boundary_is_still_cut() {
        // One 900-character run-on. There is no full stop to land on, and
        // refusing to cut would read the whole thing.
        let long = "word ".repeat(180);
        let out = truncate_to(&long, 400);
        assert!(out.ends_with(TRUNCATION_NOTE));
        assert!(out.len() < long.len());
    }

    #[test]
    fn the_cut_never_lands_inside_a_character() {
        // Multi-byte text is the case a naive `&s[..limit]` panics on, and an
        // answer containing an em dash or an accent is ordinary.
        //
        // **A sweep and not one limit, because one limit proved nothing.**
        // Written first as a single `truncate_to(&long, 400)`, it survived the
        // mutation that deletes the char-boundary walk entirely: byte 399 of
        // that fixture happens to fall between characters, so the naive slice
        // was legal and the test was a false receipt. Every limit across a
        // range guarantees the interior bytes of the accents and the em dash
        // are each landed on.
        let long = "Amélie a répondu — et puis rien. ".repeat(30);
        for limit in 100..=420 {
            let out = truncate_to(&long, limit);
            assert!(out.ends_with(TRUNCATION_NOTE), "limit {limit}: {out:?}");
        }
    }

    // ---- Listings, both platforms, on either host ----------------------

    #[test]
    fn a_say_voice_row_splits_from_the_right_because_names_contain_spaces() {
        let listing = "Samantha            en_US    # Hello! My name is Samantha.\n\
                       Bad News            en_US    # Hello! My name is Bad News.\n\
                       Eddy (English (US)) en_US    # Hello! My name is Eddy.\n\
                       Amélie              fr_CA    # Bonjour!\n";
        let v = parse_say_listing(listing);
        let names: Vec<&str> = v.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            ["Samantha", "Bad News", "Eddy (English (US))", "Amélie"]
        );
        assert_eq!(v[3].locale, "fr_CA");
        assert!(v[0].is_english());
        assert!(!v[3].is_english());
    }

    #[test]
    fn the_enhanced_and_premium_downloads_are_the_natural_ones() {
        let listing = "Samantha (Enhanced) en_US    # Hi.\n\
                       Ava (Premium)       en_US    # Hi.\n\
                       Samantha            en_US    # Hi.\n";
        let v = parse_say_listing(listing);
        assert!(v[0].natural);
        assert!(v[1].natural);
        assert!(!v[2].natural, "the compact voice was called natural");
    }

    #[test]
    fn a_blank_or_junk_line_is_not_a_voice_in_either_listing() {
        assert!(parse_say_listing("\n\n   \n").is_empty());
        assert!(parse_say_listing("# just a comment\n").is_empty());
        assert!(parse_sapi_listing("\n\n   \n").is_empty());
        // No tab: not a row this script wrote, so it is dropped rather than
        // guessed at.
        assert!(parse_sapi_listing("Microsoft David Desktop en-US\n").is_empty());
    }

    /// The exact bytes `LIST_SCRIPT` produced on the owner's box, 2026-08-26,
    /// captured with `od -c`. A fixture agrees with its author, so this one is
    /// a transcript rather than an invention.
    #[test]
    fn the_sapi_listing_parses_the_real_output_of_this_machine() {
        let listing = "Microsoft David Desktop\ten-US\nMicrosoft Zira Desktop\ten-US\n";
        let v = parse_sapi_listing(listing);
        assert_eq!(
            v.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
            ["Microsoft David Desktop", "Microsoft Zira Desktop"],
            "the tab split lost part of a name with spaces in it"
        );
        assert!(v.iter().all(|v| v.locale == "en-US"));
        assert!(v.iter().all(|v| v.is_english()));
        // SAPI has no Enhanced/Premium axis, and claiming one would be the
        // recommendation half of the branch's decision inventing a ranking.
        assert!(
            v.iter().all(|v| !v.natural),
            "a SAPI voice was called natural"
        );
    }

    #[test]
    fn a_listing_query_never_carries_a_shell() {
        // The whole Windows arm rests on the script being a constant. If
        // anyone ever interpolates into it, this stops being a fixed argv.
        let args = listing_args(Backend::Sapi);
        assert_eq!(args[0], OsString::from("-NoProfile"));
        assert_eq!(args[2], OsString::from("-Command"));
        assert_eq!(args[3], OsString::from(LIST_SCRIPT));
        assert_eq!(
            listing_args(Backend::Say),
            vec![OsString::from("-v"), OsString::from("?")]
        );
    }

    // ---- The report ----------------------------------------------------

    fn one_voice() -> Vec<Voice> {
        parse_sapi_listing("Microsoft Zira Desktop\ten-US\n")
    }

    #[test]
    fn no_platform_is_reported_as_unavailable_and_not_as_broken() {
        let r = build_report(None, Ok(one_voice()));
        assert!(!r.ready());
        assert_eq!(r.blocked, NO_PLATFORM);
        assert!(r.voices.is_empty(), "voices survived a missing platform");
    }

    /// **The gap a plain "is the binary there" check would leave.** A
    /// `powershell.exe` whose `Add-Type -AssemblyName System.Speech` fails
    /// exits zero with empty stdout; calling that available would report voice
    /// on and make no sound.
    #[test]
    fn a_backend_that_lists_no_voices_is_blocked_rather_than_ready() {
        let r = build_report(Some(Backend::Sapi), Ok(vec![]));
        assert!(!r.ready(), "an empty listing was called ready");
        assert!(r.blocked.contains("System.Speech"), "{}", r.blocked);
        assert!(
            r.blocked.contains("Settings"),
            "the fix was not named: {}",
            r.blocked
        );

        let r = build_report(Some(Backend::Say), Ok(vec![]));
        assert!(!r.ready());
        assert!(r.blocked.contains("say"), "{}", r.blocked);
    }

    #[test]
    fn a_probe_failure_becomes_the_sentence_the_settings_row_shows() {
        let r = build_report(Some(Backend::Sapi), Err("no powershell here".into()));
        assert!(!r.ready());
        assert_eq!(r.blocked, "no powershell here");
    }

    #[test]
    fn a_backend_with_voices_is_ready_and_says_nothing_about_being_blocked() {
        let r = build_report(Some(Backend::Sapi), Ok(one_voice()));
        assert!(r.ready());
        assert!(r.blocked.is_empty());
        assert_eq!(r.voices.len(), 1);
    }

    // ---- What a child is handed ----------------------------------------

    fn plan(backend: Backend, voice: Option<&str>, text: &str) -> Utterance {
        plan_utterance(backend, PathBuf::from("prog"), voice, text)
    }

    #[test]
    fn an_answer_never_reaches_a_child_as_an_argument() {
        // Arbitrary model output, including the two shapes that would matter:
        // a leading dash (a flag, if it were argv) and a quote.
        let nasty = "-v \"; Remove-Item C:\\ -Recurse\" and then some prose.";
        for backend in [Backend::Say, Backend::Sapi] {
            let u = plan(backend, None, nasty);
            for arg in &u.args {
                assert!(
                    !arg.to_string_lossy().contains("Remove-Item C:"),
                    "{backend:?} put answer text in argv: {arg:?}"
                );
            }
            assert!(
                u.stdin.contains(nasty),
                "{backend:?} did not send the text on stdin"
            );
        }
    }

    #[test]
    fn the_windows_argv_is_the_fixed_script_and_nothing_else() {
        let u = plan(Backend::Sapi, Some("Microsoft Zira Desktop"), "hello.");
        assert_eq!(
            u.args,
            vec![
                OsString::from("-NoProfile"),
                OsString::from("-NonInteractive"),
                OsString::from("-Command"),
                OsString::from(SPEAK_SCRIPT),
            ],
            "something other than the constant script reached the command line"
        );
        // Voice first line, text after — the contract SPEAK_SCRIPT parses.
        assert_eq!(u.stdin, "Microsoft Zira Desktop\nhello.");
        // And the system default is an empty first line, not an absent one:
        // the script splits on the first newline and would exit without it.
        assert_eq!(plan(Backend::Sapi, None, "hello.").stdin, "\nhello.");
    }

    /// A voice name is user input from `settings.json`, and on Windows it
    /// shares a stream with the answer. A newline in it would move the
    /// boundary and let the answer's first line be read as a voice name.
    #[test]
    fn a_voice_name_cannot_move_the_boundary_between_the_two_fields() {
        let u = plan(Backend::Sapi, Some("Zira\nrm -rf /"), "the real answer.");
        assert_eq!(
            u.stdin.lines().next(),
            Some("Zirarm -rf /"),
            "a newline in a voice name survived into the payload: {:?}",
            u.stdin
        );
        assert_eq!(u.stdin.lines().count(), 2, "the payload grew a line");
        assert!(u.stdin.ends_with("the real answer."));
    }

    #[test]
    fn the_mac_voice_is_an_argv_flag_and_the_default_passes_no_flag_at_all() {
        // `say -v ""` is not the system default, it is an error; the absence
        // of the flag is what selects it.
        assert_eq!(plan(Backend::Say, None, "hi.").args, Vec::<OsString>::new());
        assert_eq!(
            plan(Backend::Say, Some("Daniel"), "hi.").args,
            vec![OsString::from("-v"), OsString::from("Daniel")]
        );
    }

    // ---- Refusals and labels -------------------------------------------

    fn fleet() -> Vec<Voice> {
        parse_say_listing(
            "Samantha (Enhanced) en_US    # Hi.\n\
             Samantha            en_US    # Hi.\n\
             Daniel              en_GB    # Hi.\n\
             Anna                de_DE    # Hi.\n",
        )
    }

    #[test]
    fn an_installed_voice_is_accepted() {
        assert_eq!(refuse_an_unknown_voice("Daniel", &fleet()), None);
        assert_eq!(
            refuse_an_unknown_voice("Samantha (Enhanced)", &fleet()),
            None
        );
    }

    #[test]
    fn an_unknown_voice_is_refused_naming_real_ones() {
        let r = refuse_an_unknown_voice("Siri", &fleet()).expect("`Siri` was accepted");
        assert!(r.contains("nothing was changed"));
        // The option that is always right, named every time — an uncurated
        // sample can offer `Bahh` and the default cannot be wrong.
        assert!(
            r.contains("system default"),
            "the default was not offered: {r}"
        );
        assert!(
            r.contains("Samantha (Enhanced)"),
            "the refusal named no real voice: {r}"
        );
        assert!(
            r.contains("/voice"),
            "the refusal did not point anywhere: {r}"
        );
    }

    #[test]
    fn a_case_typo_is_still_refused_but_names_the_exact_spelling() {
        // Matching loosely would make `/voice daniel` work while the listing
        // shows `Daniel`, which is the disagreement `/theme` refuses.
        let r = refuse_an_unknown_voice("daniel", &fleet()).expect("`daniel` was accepted");
        assert!(
            r.contains("`Daniel` is"),
            "the exact spelling was not offered: {r}"
        );
    }

    #[test]
    fn the_unset_voice_is_named_as_the_system_default_and_says_where_to_change_it() {
        let label = State::default().voice_label();
        assert!(label.contains("system default"), "{label}");
        // Both platforms name the room. On macOS it is the Siri seat, which is
        // the reason this module never substitutes an enhanced voice for it.
        assert!(
            label.contains("Settings"),
            "the room was not named: {label}"
        );
        assert_eq!(
            State {
                on: true,
                voice: Some("Daniel".into()),
                ..Default::default()
            }
            .voice_label(),
            "Daniel"
        );
    }

    #[test]
    fn speaking_is_off_until_it_is_asked_for() {
        // The global starts silent. An assistant that began talking because a
        // module was linked in would be a defect nobody could turn off in time.
        assert!(!State::default().on);
    }

    // ---- The live machine ----------------------------------------------

    /// The process-wide [`State`] is one object and `cargo test` runs threads.
    /// Every test that writes it takes this first, so one test setting `on`
    /// cannot be observed by another that is asserting the room is quiet.
    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A limit of nought would be a voice that reports itself on and says
    /// nothing, so it is refused in favour of the default.
    #[test]
    fn a_zero_limit_is_refused_rather_than_silencing_the_voice() {
        let _guard = exclusive();
        restore(false, None, Some(0));
        assert_eq!(state().spoken_limit, SPOKEN_LIMIT);
        restore(false, None, Some(120));
        assert_eq!(state().spoken_limit, 120, "a real limit is honoured");
        restore(false, None, None);
        assert_eq!(
            state().spoken_limit,
            SPOKEN_LIMIT,
            "absent means the default"
        );
    }

    /// The synced-settings case. Keeping the name would make every utterance
    /// select a voice that is not there, and on macOS that is silence with
    /// voice reporting itself on.
    #[test]
    fn a_voice_this_machine_does_not_have_drops_to_the_system_default() {
        let _guard = exclusive();
        restore(
            true,
            Some("Ava (Premium) From Some Other Machine".into()),
            None,
        );
        let s = state();
        assert!(s.on, "the on/off choice is the user's and must survive");
        assert_eq!(s.voice, None, "an uninstalled voice was kept");
        // Put the global back: these tests share one process, and one left on
        // would make a later `speak` audible in a test run.
        restore(false, None, None);
    }

    /// **Off means no process, and this is the test that says so out loud.**
    /// `speaking_is_off_until_it_is_asked_for` only checks the default value
    /// of a struct; deleting the `if !state.on { return }` guard from [`speak`]
    /// left the whole suite green, and the defect it hides is an assistant
    /// that starts talking on a machine where nobody asked it to.
    ///
    /// It runs against the real machine, which is the point — a mutation here
    /// spawns a synthesiser, and the assertion catches it before the sleep
    /// this test does not have. On a box where voice is unavailable the spawn
    /// would fail anyway, so the assertion is only meaningful where
    /// `report().ready()`; it is asserted unconditionally regardless, because
    /// a slot filled on an unavailable machine would be a different bug.
    #[test]
    fn an_answer_starts_nothing_at_all_while_voice_is_off() {
        let _guard = exclusive();
        restore(false, None, None);
        speak("This must not be spoken, because voice is off.");
        assert!(
            speaker()
                .current
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none(),
            "voice is off and something was still started"
        );
    }

    /// This host's real answer, whatever it is, must be self-consistent —
    /// ready implies voices, blocked implies a sentence somebody can act on.
    /// It is the one assertion that runs the actual probe.
    #[test]
    fn this_machine_reports_a_coherent_answer_about_itself() {
        let r = report();
        if r.ready() {
            assert!(!r.voices.is_empty(), "ready with nothing to speak with");
            assert!(r.backend.is_some());
            assert!(
                r.voices.iter().all(|v| !v.name.is_empty()),
                "a nameless voice would be unselectable and unprintable"
            );
        } else {
            assert!(
                r.blocked.len() > 10,
                "an unavailable row must say what is missing, not just be false: {:?}",
                r.blocked
            );
        }
    }

    /// The whole point of the port: on Windows this is a real implementation
    /// and not an honest shrug. Compiled and run only where it can be true.
    #[test]
    #[cfg(windows)]
    fn windows_actually_has_a_synthesiser_behind_the_setting() {
        assert_eq!(backend(), Some(Backend::Sapi));
        let r = report();
        assert!(
            r.ready(),
            "Windows voice is not available on this box: {}",
            r.blocked
        );
        assert!(
            r.voices.iter().any(|v| v.is_english()),
            "no English voice: {:?}",
            r.voices
        );
    }

    #[test]
    #[ignore = "makes noise: run with --ignored, volume up"]
    fn it_actually_speaks_and_a_second_utterance_stops_the_first() {
        // Manual, by ear. Steps:
        //   1. cargo test -p emma --lib speech -- --ignored --nocapture
        //   2. Expect: a voice starts "This is the first answer…", is cut off
        //      part-way, and the second sentence is spoken to the end.
        //   3. Expect silence after the run — no orphaned child in the task
        //      list.
        assert!(report().ready(), "nothing to hear: {}", report().blocked);
        set_on(true);
        speak("This is the first answer, and you should not hear the end of this sentence at all.");
        std::thread::sleep(std::time::Duration::from_millis(2500));
        speak("This is the second answer, and it should be the one that finishes.");
        std::thread::sleep(std::time::Duration::from_secs(6));
        set_on(false);
        assert!(
            speaker()
                .current
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none(),
            "a synthesiser process was left behind"
        );
    }
}
