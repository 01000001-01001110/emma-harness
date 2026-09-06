//! Seeing the screen.
//!
//! The owner's request was "so it can see the desktop", and the whole design
//! follows from the second half of that sentence rather than the first.
//! Capturing a screen is a two-line shell out; *delivering a picture to a
//! model* is the part that had to be built, and it is not in this crate. It is
//! [`emma_tool_api::ToolOutcome::images`], the image block in `emma_llm`, and
//! the one mapping in `agent.rs` that moves the first into the second without
//! knowing which tool filled it. This crate is the first caller of that path
//! and is deliberately not privileged on it.
//!
//! # Three things that are not pretended about
//!
//! **The provider may not be able to carry an image.** That is a fact about
//! where the bytes are going, so it arrives as [`ImageDelivery`], built once at
//! start-up by whoever chose the provider. When it says no, the result is prose
//! that names the path and the reason. The model is never told a picture is
//! attached when none is.
//!
//! **The model may not be able to read one.** Nothing here can tell:
//! `ornith:35b` and a Qwen VL model are the same string to this code and the
//! same wire shape to Ollama. So the honest sentence is written into the
//! result. The image was sent, a text-only model will not see it, and the file
//! is at this path either way. That is written out rather than replaced by a
//! capability check, which would be a guess dressed as a gate.
//!
//! **The platform may not have a screen this can reach.** There are two
//! implementations of [`Capture`] and neither is a fallback for the other. On a
//! third platform [`host_capture`] answers `None` and every call is refused in
//! words naming the platform. What is *not* here is the shape ruling 4 of the
//! port forbids: an arm that returns success on the platform nobody
//! implemented.
//!
//! # Why the Windows arm is not a PowerShell script
//!
//! It was, and the first real run on Windows 11 with stock Defender said this,
//! verbatim:
//!
//! ```text
//! This script contains malicious content and has been blocked by your
//! antivirus software.
//!     + FullyQualifiedErrorId : ScriptContainedMaliciousContent
//! ```
//!
//! Bisected: neither `CopyFromScreen` alone nor a JPEG save alone is flagged;
//! *capture, then resize, then encode* is, which is the shape of every
//! screenshot-stealing script AMSI has a signature for — and it is also the
//! shape this tool needs. A `.ps1` on disk run with `-File` is blocked
//! identically, so the delivery is not the issue and there is no version of
//! "write it to a file instead" that helps.
//!
//! The obvious next move — find a spelling the current signature misses — is
//! the one thing this crate must not do. That is antivirus evasion, it would
//! be written into a tool a human is asked to approve, and it would rot on the
//! next definition update into a feature that breaks in the field with a
//! frightening message. So the Windows arm does the work in process with GDI
//! and GDI+, which AMSI does not scan and has no reason to: there is no script.
//! It also costs no subprocess, no execution policy, and no quoting.
//!
//! # The failure that cannot be reported
//!
//! macOS gates screen capture per application. Two of the three failure shapes
//! are detectable, and the third is the ugly one: with permission denied in
//! some configurations `screencapture` exits zero and writes a valid image of
//! the desktop wallpaper with no windows in it. There is no way to tell that
//! from a genuinely tidy desktop, so it is documented in the tool description
//! as the thing to suspect rather than reported as a failure that might not
//! have happened.
//!
//! Windows has no per-application screen-recording gate, so that particular
//! trap does not exist there. It has a different one, and it is equally
//! undetectable: a process on a non-interactive window station — a service, a
//! scheduled task with "run whether user is logged on or not", a locked session
//! — blits a black rectangle of exactly the right size and reports success.
//!
//! # The seam
//!
//! [`Capture`] is the seam, and it is where it is for one reason, stated so
//! nobody removes it as ceremony: the tests must not capture a real screen.
//! Screen Recording permission is not available in CI, is not available to a
//! test runner on a developer's machine without a click, a Windows CI agent may
//! have no interactive desktop at all, and a suite that needs any of that is a
//! suite that gets ignored. A scripted `Capture` makes the failure shapes — a
//! zero-byte capture, a resize that did not happen, a refusal — ordinary test
//! cases.
//!
//! Under it, [`Commands`] keeps a second seam, [`Runner`], because its whole
//! behaviour is which arguments it hands to which binary and that is worth
//! asserting. [`Gdi`] has no such seam and cannot have one: it calls the
//! operating system directly, so the only test that can prove it works is one
//! that captures a real screen, and that test is `#[ignore]`d and named in the
//! report rather than pretended away.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use emma_tool_api::{OutcomeImage, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

#[cfg(windows)]
mod gdi;
#[cfg(windows)]
pub use gdi::Gdi;

// region: What the provider can carry
// ---------------------------------------------------------------------------
// What the provider can carry
//
// One value, decided once where the provider is chosen, so the tool never
// guesses and never has to learn a provider's name.
// ---------------------------------------------------------------------------

/// Whether a picture attached to a tool result reaches the model, and how.
///
/// Built by the binary from the provider it just resolved, because that is the
/// only place both facts are known and it keeps this crate from growing a match
/// on provider names that would go stale one provider later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageDelivery {
    /// The result block itself carries the picture. Anthropic's shape: an
    /// `image` block inside the `tool_result` content array.
    Blocks {
        /// Named in the result so a reader of the transcript knows which wire
        /// shape carried the bytes.
        provider: &'static str,
    },
    /// The picture rides on a message next to the result rather than inside it,
    /// and whether the model looks at it depends on the model. Ollama's shape,
    /// and the two chat-completions hosts'.
    Attached {
        /// See [`ImageDelivery::Blocks`].
        provider: &'static str,
        /// Stated back to the model, because the one thing that decides whether
        /// the picture is readable is which model is running and only the human
        /// reading the transcript can check it.
        model: String,
    },
    /// No path from a tool result to this provider's wire. The capture still
    /// happens and the file is still written; the result says why the picture
    /// is not attached.
    Unsupported {
        /// Said back to the model verbatim. Written where the provider was
        /// resolved, which is the only place that knows why.
        reason: String,
    },
}

impl ImageDelivery {
    /// The sentence appended to a successful capture. Present in all three
    /// cases, because "the picture is attached" and "the picture is not
    /// attached, here is why" are equally worth saying and only one of them is
    /// ever guessable from the rest of the result.
    fn note(&self) -> String {
        match self {
            Self::Blocks { provider } => format!(
                "The image is attached to this result and {provider} carries it inside the \
                 result block."
            ),
            Self::Attached { provider, model } => format!(
                "The image is attached to this result. {provider} carries it on the message \
                 beside the result, and whether it is read depends on the model: {model} sees \
                 it if it is a vision model and silently does not if it is text-only. Nothing \
                 here can tell which. The file above is readable either way."
            ),
            Self::Unsupported { reason } => format!(
                "The image itself is NOT attached to this result: {reason}. The capture \
                 succeeded and the file above is on disk."
            ),
        }
    }

    fn carries_images(&self) -> bool {
        !matches!(self, Self::Unsupported { .. })
    }
}

// endregion: What the provider can carry

// region: The command seam
// ---------------------------------------------------------------------------
// The command seam
//
// One trait and one real implementation, under the macOS backend only, so its
// argument shapes are asserted rather than assumed.
// ---------------------------------------------------------------------------

/// What one external command did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    /// `None` when the process was killed by a signal and never returned one.
    pub code: Option<i32>,
    /// Whatever the command said about itself, trimmed. Reported verbatim in
    /// failures: `screencapture` puts the interesting half of a permission
    /// problem here and nothing downstream can reconstruct it.
    pub stderr: String,
}

impl RunOutcome {
    fn succeeded(&self) -> bool {
        self.code == Some(0)
    }
}

/// Runs the external commands the macOS backend shells out to.
///
/// See the module doc for why the seam exists at all. This one is narrower
/// than [`Capture`]: it exists so a test can assert that `screencapture` is
/// invoked with `-x` and a display number and `sips` with the bound that was
/// asked for, which is the whole of what that backend does.
pub trait Runner: Send + Sync {
    /// Run `program` with `args` and say what happened. An `Err` is the spawn
    /// itself failing, which is a different answer from a non-zero exit.
    fn run(&self, program: &Path, args: &[String]) -> std::io::Result<RunOutcome>;
}

/// The real one.
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &Path, args: &[String]) -> std::io::Result<RunOutcome> {
        let out = Command::new(program).args(args).output()?;
        Ok(RunOutcome {
            code: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }
}

// endregion: The command seam

// region: The screen
// ---------------------------------------------------------------------------
// The screen
//
// One trait, two implementations, and the platform question asked in exactly
// one place.
// ---------------------------------------------------------------------------

/// Which operating-system facility took the picture.
///
/// Carried on [`Capture::backend`] rather than deduced from a `cfg`, so the
/// sentences that differ per platform — the hint after a failure, the option
/// one platform cannot honour — are chosen by the thing that actually ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// macOS. `/usr/sbin/screencapture` writes the PNG, `/usr/bin/sips` makes
    /// the bounded copy.
    Screencapture,
    /// Windows. `BitBlt` into a compatible bitmap, `StretchBlt` for the bounded
    /// copy, GDI+ to encode both. No subprocess — see the module doc.
    Gdi,
}

impl Backend {
    /// Which backend this machine's build uses, or `None` where there is not
    /// one. The descriptor half of [`host_capture`], separated so a test can
    /// assert the platform choice without constructing anything.
    pub fn host() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::Screencapture)
        } else if cfg!(windows) {
            Some(Self::Gdi)
        } else {
            None
        }
    }

    /// Arguments this backend was asked for and cannot honour, said in words.
    ///
    /// One entry today. GDI composites the desktop without the mouse pointer
    /// and there is no flag that adds it — drawing it means `GetCursorInfo`
    /// plus `DrawIcon` against a cursor that may have moved since the blit.
    /// Refusing the whole call over it would be worse than saying so: the model
    /// asked for a screenshot and a cursor, and it is getting the screenshot.
    fn unhonoured(self, shot: &Shot) -> Vec<String> {
        match self {
            Self::Screencapture => Vec::new(),
            Self::Gdi if shot.cursor => vec![
                "The mouse pointer is NOT drawn: this platform's capture composites the \
                 desktop without it and there is no option that adds it."
                    .to_string(),
            ],
            Self::Gdi => Vec::new(),
        }
    }

    /// The sentence every capture failure ends with, because on each platform
    /// the same cause is common enough that omitting it would waste a turn on
    /// the model guessing.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Screencapture => {
                "The usual cause is that Screen Recording permission has not been granted to \
                 the terminal application Emma is running in; grant it in System Settings > \
                 Privacy & Security > Screen Recording, then restart that application."
            }
            Self::Gdi => {
                "Windows has no per-application screen-recording permission, so the usual \
                 cause is that this process has no interactive desktop to read — a service, a \
                 scheduled task set to run whether the user is logged on or not, or a locked \
                 session. Run Emma from a signed-in desktop session."
            }
        }
    }
}

/// The one step that touches the screen.
///
/// `shoot` has three outcomes and they are three different things, which is
/// why it is not a `bool` and not a plain `Result<(), _>`:
///
/// - `Err` — nothing was captured. The call fails and the model is told why.
/// - `Ok(None)` — everything asked for is on disk.
/// - `Ok(Some(why))` — the full capture is on disk and the bounded copy is not.
///   The call still succeeds: the model learns the screen was captured, gets
///   the path, and is told exactly what it is not being shown.
pub trait Capture: Send + Sync {
    /// Which facility this is, for the sentences that differ per platform.
    fn backend(&self) -> Backend;

    /// Write the full-resolution PNG at `full` and, when `bounded` is `Some`,
    /// a bounded JPEG at that path with its longest edge at most that many
    /// pixels. See the trait doc for the three outcomes.
    fn shoot(
        &self,
        shot: &Shot,
        full: &Path,
        bounded: Option<(&Path, u32)>,
    ) -> Result<Option<String>, ToolError>;
}

/// The backend this machine's build uses, ready to run, or `None` where there
/// is not one.
///
/// The `None` is load-bearing and is the whole of ruling 4 in one place: a
/// platform with neither facility gets a refusal that names it, not an arm that
/// quietly succeeds at nothing.
pub fn host_capture() -> Option<Arc<dyn Capture>> {
    #[cfg(target_os = "macos")]
    {
        return Some(Arc::new(Commands::new(Arc::new(SystemRunner))));
    }
    #[cfg(windows)]
    {
        return Some(Arc::new(Gdi));
    }
    #[allow(unreachable_code)]
    None
}

/// Absolute paths, checked with `command -v` on a stock macOS 15 rather than
/// assumed. `PATH` is deliberately not consulted: this tool runs a privileged
/// capture, and resolving the binary through an environment the model's own
/// `Bash` calls can edit is how that becomes an execution primitive.
const SCREENCAPTURE: &str = "/usr/sbin/screencapture";
const SIPS: &str = "/usr/bin/sips";

/// The macOS backend: `screencapture` for the picture, `sips` for the copy.
pub struct Commands {
    runner: Arc<dyn Runner>,
}

impl Commands {
    /// With a runner of the caller's choosing. [`SystemRunner`] is the real
    /// one; the suite passes a scripted one.
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self { runner }
    }
}

impl Capture for Commands {
    fn backend(&self) -> Backend {
        Backend::Screencapture
    }

    fn shoot(
        &self,
        shot: &Shot,
        full: &Path,
        bounded: Option<(&Path, u32)>,
    ) -> Result<Option<String>, ToolError> {
        let mut argv = vec!["-x".to_string()];
        if shot.cursor {
            argv.push("-C".to_string());
        }
        match shot.region {
            Some((x, y, w, h)) => {
                argv.push("-R".to_string());
                argv.push(format!("{x},{y},{w},{h}"));
            }
            None => {
                argv.push("-D".to_string());
                argv.push(shot.display.to_string());
            }
        }
        argv.push(full.to_string_lossy().to_string());

        let run = self
            .runner
            .run(Path::new(SCREENCAPTURE), &argv)
            .map_err(|e| {
                ToolError::Unavailable(format!(
                    "I could not run {SCREENCAPTURE}: {e}. It ships with macOS, so its \
                     absence means this is not a normal macOS install."
                ))
            })?;
        if !run.succeeded() {
            return Err(ToolError::Failed(format!(
                "screencapture exited {} while capturing {}{}. {}",
                code_of(&run),
                shot.what(),
                suffix(&run.stderr),
                Backend::Screencapture.hint()
            )));
        }

        let Some((dest, max_dim)) = bounded else {
            return Ok(None);
        };
        let argv = vec![
            "-Z".to_string(),
            max_dim.to_string(),
            "-s".to_string(),
            "format".to_string(),
            "jpeg".to_string(),
            full.to_string_lossy().to_string(),
            "--out".to_string(),
            dest.to_string_lossy().to_string(),
        ];
        let run = match self.runner.run(Path::new(SIPS), &argv) {
            Ok(r) => r,
            Err(e) => return Ok(Some(format!("{SIPS} could not be run ({e})"))),
        };
        if !run.succeeded() {
            return Ok(Some(format!(
                "sips exited {} while resizing{}",
                code_of(&run),
                suffix(&run.stderr)
            )));
        }
        Ok(None)
    }
}

/// The bounded copy's dimensions: the same aspect ratio, longest edge at most
/// `max_dim`, and never smaller than one pixel in either direction.
///
/// Shared rather than per-backend so the arithmetic has one home and one test.
/// `sips` does its own on macOS; this is what the Windows arm hands `StretchBlt`
/// and what the test below pins.
pub(crate) fn bounded_dimensions(w: i32, h: i32, max_dim: u32) -> (i32, i32) {
    let longest = w.max(h);
    let max = max_dim as i32;
    if longest <= max || longest <= 0 {
        return (w.max(1), h.max(1));
    }
    // Rounded rather than truncated: a 1921-pixel-wide capture bounded to 1280
    // should not lose a pixel of height to integer division.
    let scale = f64::from(max) / f64::from(longest);
    let round = |n: i32| ((f64::from(n) * scale).round() as i32).max(1);
    (round(w), round(h))
}

// endregion: The screen

// region: Bounds
// ---------------------------------------------------------------------------
// Bounds
//
// What is allowed to reach the model, and what happens when the bounded copy
// still will not fit.
// ---------------------------------------------------------------------------

/// The longest edge of the copy sent to the model, when the call does not say.
///
/// A Retina desktop is 3456 x 2234 and a lossless capture of it is a several
/// megabyte PNG, which becomes a third again as base64. 1280 is a size at which
/// window layout, dialogs and headings are legible and body text mostly is not,
/// which matches what "see the desktop" is asked for.
const DEFAULT_MAX_DIMENSION: u32 = 1280;

/// The ceiling on `max_dimension`. Above this the token cost stops being worth
/// the detail on every provider that resizes server-side anyway.
const MAX_MAX_DIMENSION: u32 = 2000;

/// The last check before sending: base64 characters, not decoded bytes, because
/// that is what actually travels. Roughly 3 MB of encoded payload, which a
/// JPEG bounded to 2000px does not come close to unless something has gone
/// wrong with the resize.
const MAX_ENCODED_BYTES: usize = 3 * 1024 * 1024;

// endregion: Bounds

// region: The tool
// ---------------------------------------------------------------------------
// The tool
// ---------------------------------------------------------------------------

/// One capture's arguments, read once, so no backend re-parses the JSON.
pub struct Shot {
    /// `(x, y, w, h)` in screen coordinates, when a rectangle was asked for.
    pub region: Option<(i64, i64, i64, i64)>,
    /// 1-based, 1 being the primary display. Ignored when `region` is set.
    pub display: u64,
    /// Draw the mouse pointer. Honoured on macOS; see [`Backend::unhonoured`].
    pub cursor: bool,
    /// Longest edge of the copy sent to the model.
    pub max_dim: u32,
}

impl Shot {
    /// Arguments already through [`Screenshot::validate_args`], so every
    /// fallback here is a default rather than a tolerance.
    fn read(args: &Value) -> Self {
        let region = args.get("region").and_then(Value::as_object).map(|r| {
            let get = |k: &str| r.get(k).and_then(Value::as_i64).unwrap_or(0);
            (get("x"), get("y"), get("w"), get("h"))
        });
        Self {
            region,
            display: args.get("display").and_then(Value::as_u64).unwrap_or(1),
            cursor: args.get("cursor").and_then(Value::as_bool).unwrap_or(false),
            max_dim: args
                .get("max_dimension")
                .and_then(Value::as_u64)
                .map_or(DEFAULT_MAX_DIMENSION, |n| n as u32),
        }
    }

    /// What was captured, for the first line of the result and for a failure.
    pub fn what(&self) -> String {
        match self.region {
            Some((x, y, w, h)) => format!("the region {w}x{h} at ({x},{y})"),
            None => format!("display {}", self.display),
        }
    }
}

/// The tool. See the module doc for what it does not pretend about.
pub struct Screenshot {
    capture: Option<Arc<dyn Capture>>,
    delivery: ImageDelivery,
    /// Where captures are kept. One directory per session under the system
    /// temporary directory, so the files are bounded to one place a human can
    /// find and delete rather than scattered next to whatever the cwd was.
    root: PathBuf,
}

impl Screenshot {
    /// The one the binary builds: the host's backend and the system temporary
    /// directory.
    pub fn new(delivery: ImageDelivery) -> Self {
        Self::with_capture(delivery, host_capture(), std::env::temp_dir())
    }

    /// Every part chosen. `None` for the capture is the platform with no
    /// backend, which every test of the refusal uses.
    pub fn with_capture(
        delivery: ImageDelivery,
        capture: Option<Arc<dyn Capture>>,
        root: PathBuf,
    ) -> Self {
        Self {
            capture,
            delivery,
            root,
        }
    }

    fn dir_for(&self, ctx: &ToolCtx) -> PathBuf {
        self.root
            .join("emma-screenshots")
            .join(sanitise(&ctx.session_id))
    }
}

/// Session and turn ids reach a path here, so they are reduced to characters
/// that cannot be a separator, a parent reference, or a shell surprise. Both
/// are generated by Emma and neither has ever contained anything else; this is
/// the cheap half of not depending on that.
fn sanitise(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if cleaned.is_empty() {
        "session".to_string()
    } else {
        cleaned
    }
}

#[async_trait::async_trait]
impl Tool for Screenshot {
    fn name(&self) -> &'static str {
        "Screenshot"
    }

    fn description(&self) -> &str {
        include_str!("descriptions/screenshot.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "display": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Which display to capture, 1 for the main one."
                },
                "region": {
                    "type": "object",
                    "description": "Capture this rectangle instead of a whole display.",
                    "properties": {
                        "x": { "type": "integer" },
                        "y": { "type": "integer" },
                        "w": { "type": "integer", "minimum": 1 },
                        "h": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["x", "y", "w", "h"]
                },
                "cursor": {
                    "type": "boolean",
                    "description": "Draw the mouse pointer into the capture. macOS only; on \
                                    Windows the result says the pointer was not drawn."
                },
                "max_dimension": {
                    "type": "integer",
                    "minimum": 64,
                    "maximum": MAX_MAX_DIMENSION,
                    "description":
                        "Longest edge of the copy sent to the model. The file on disk keeps \
                         its full resolution regardless."
                }
            },
            "additionalProperties": false
        })
    }

    /// **Not read-only, and not exempt from approval.** It writes a file, which
    /// settles `read_only` on its own. The larger reason is the one the gate
    /// exists for: a screen capture takes whatever a human happens to have open,
    /// which is a wider read than any path this model can name, and treating it
    /// as ambient because it is convenient would be the one exemption that makes
    /// every other gate look arbitrary. `idempotent` is false because the answer
    /// is the screen at a moment, and two calls are two moments.
    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: false,
            reaches_network: false,
            idempotent: false,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if let Some(d) = args.get("display") {
            let n = d.as_u64().ok_or_else(|| {
                ToolError::BadArguments("`display` must be a whole number".into())
            })?;
            if n == 0 {
                return Err(ToolError::BadArguments(
                    "`display` counts from 1, where 1 is the main display".into(),
                ));
            }
        }
        if let Some(region) = args.get("region") {
            let obj = region.as_object().ok_or_else(|| {
                ToolError::BadArguments("`region` must be an object {x, y, w, h}".into())
            })?;
            for key in ["x", "y", "w", "h"] {
                let v = obj.get(key).and_then(Value::as_i64).ok_or_else(|| {
                    ToolError::BadArguments(format!("`region.{key}` must be a whole number"))
                })?;
                if matches!(key, "w" | "h") && v < 1 {
                    return Err(ToolError::BadArguments(format!(
                        "`region.{key}` must be at least 1 pixel"
                    )));
                }
            }
        }
        if let Some(m) = args.get("max_dimension") {
            let n = m.as_u64().ok_or_else(|| {
                ToolError::BadArguments("`max_dimension` must be a whole number".into())
            })?;
            if !(64..=u64::from(MAX_MAX_DIMENSION)).contains(&n) {
                return Err(ToolError::BadArguments(format!(
                    "`max_dimension` must be between 64 and {MAX_MAX_DIMENSION}"
                )));
            }
        }
        Ok(())
    }

    async fn invoke(
        &self,
        ctx: &ToolCtx,
        args: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.capture(ctx, &args))
    }
}

impl Screenshot {
    /// Split out of `invoke` so the whole thing is synchronous and testable
    /// without a runtime. Nothing here awaits: a capture is short and every one
    /// goes through [`Capture`].
    fn capture(&self, ctx: &ToolCtx, args: &Value) -> Result<ToolOutcome, ToolError> {
        // Compiles everywhere, refuses honestly, and names the platform it is
        // refusing on rather than saying "unsupported".
        let Some(capture) = &self.capture else {
            return Err(ToolError::Unavailable(format!(
                "I cannot capture the screen on {}: this tool is implemented with macOS's \
                 screencapture and Windows' GDI, and has no implementation for that platform.",
                std::env::consts::OS
            )));
        };
        self.validate_args(args)?;
        let shot = Shot::read(args);
        let what = shot.what();
        let backend = capture.backend();

        let dir = self.dir_for(ctx);
        std::fs::create_dir_all(&dir).map_err(|e| {
            ToolError::Failed(format!(
                "I could not create the capture directory {}: {e}",
                dir.display()
            ))
        })?;
        let stem = format!("{}-{}", sanitise(&ctx.turn_id), stamp());
        let full = dir.join(format!("{stem}.png"));
        let bounded = dir.join(format!("{stem}-{}.jpg", shot.max_dim));

        // Asked once, here, because it decides whether the backend is told to
        // make a bounded copy at all. A provider that cannot carry a picture
        // must not pay for one.
        let wants_image = self.delivery.carries_images();
        let ask = wants_image.then_some((bounded.as_path(), shot.max_dim));
        let shrink_failed = capture.shoot(&shot, &full, ask)?;

        // Zero bytes with a successful capture is the shape a denied permission
        // takes on some macOS versions, and it is also what a cancelled capture
        // leaves behind. Either way there is nothing to send, and reporting
        // success here would hand the model an empty picture.
        let size = std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
        if size == 0 {
            return Err(ToolError::Failed(format!(
                "the capture reported success but wrote no image for {what}. {}",
                backend.hint()
            )));
        }

        let mut lines = vec![
            format!("Captured {what}."),
            format!("Full resolution PNG, {size} bytes: {}", full.display()),
        ];
        lines.extend(backend.unhonoured(&shot));

        if !wants_image {
            lines.push(self.delivery.note());
            return Ok(finish(lines, None, &full));
        }

        // The capture worked and only the shrink did not, so this is a result
        // rather than an error: the model still learns the screen was captured,
        // still gets the path, and is told exactly what it is not being shown.
        if let Some(why) = shrink_failed {
            lines.push(format!(
                "The image itself is NOT attached: {why}. The PNG above is on disk."
            ));
            return Ok(finish(lines, None, &full));
        }

        match encoded(&bounded) {
            Ok(data) => {
                lines.push(format!(
                    "Sent to you as JPEG, longest edge at most {} pixels, {} bytes encoded. \
                     The PNG above is the unbounded original.",
                    shot.max_dim,
                    data.len()
                ));
                lines.push(self.delivery.note());
                let image = OutcomeImage {
                    media_type: "image/jpeg".to_string(),
                    data,
                    path: Some(full.to_string_lossy().to_string()),
                };
                Ok(finish(lines, Some(image), &full))
            }
            Err(why) => {
                lines.push(format!(
                    "The image itself is NOT attached: {why}. The PNG above is on disk."
                ));
                Ok(finish(lines, None, &full))
            }
        }
    }
}

/// The bounded copy as base64, or the reason there is not one.
///
/// The last gate before the wire, and it is a size gate rather than a
/// correctness one: both backends have already said they wrote the file, so
/// what is left is whether what they wrote is worth sending.
fn encoded(dest: &Path) -> Result<String, String> {
    let bytes = std::fs::read(dest).map_err(|e| {
        format!(
            "the resized copy at {} could not be read ({e})",
            dest.display()
        )
    })?;
    if bytes.is_empty() {
        return Err("the resized copy came back empty".to_string());
    }
    let data = BASE64.encode(&bytes);
    if data.len() > MAX_ENCODED_BYTES {
        return Err(format!(
            "the resized copy is {} encoded bytes, over the {MAX_ENCODED_BYTES} byte cap; \
             call again with a smaller max_dimension",
            data.len()
        ));
    }
    Ok(data)
}

fn finish(lines: Vec<String>, image: Option<OutcomeImage>, full: &Path) -> ToolOutcome {
    let outcome = ToolOutcome::new(lines.join("\n")).with_display(format!(
        "{} {}",
        if image.is_some() {
            "captured"
        } else {
            "captured, not attached:"
        },
        full.display()
    ));
    match image {
        Some(i) => outcome.with_image(i),
        None => outcome,
    }
}

fn code_of(run: &RunOutcome) -> String {
    match run.code {
        Some(c) => c.to_string(),
        None => "on a signal, with no status".to_string(),
    }
}

fn suffix(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

/// Milliseconds since the epoch, so two captures in one turn do not overwrite
/// each other. Not a clock anybody reads: uniqueness within a turn is the whole
/// requirement.
fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// endregion: The tool

#[cfg(test)]
mod tests {
    use super::bounded_dimensions;

    /// The arithmetic the Windows arm hands `StretchBlt`. Unit-tested here
    /// rather than through a capture because it is the one part of that
    /// backend a machine with no screen can check.
    #[test]
    fn a_bounded_copy_keeps_its_shape_and_never_collapses() {
        // Already inside the bound: untouched, not upscaled.
        assert_eq!(bounded_dimensions(800, 600, 1280), (800, 600));
        // Landscape and portrait both bound their longest edge.
        assert_eq!(bounded_dimensions(1920, 1080, 1280), (1280, 720));
        assert_eq!(bounded_dimensions(1080, 1920, 1280), (720, 1280));
        // A shape extreme enough to round the short edge to zero keeps a pixel:
        // GDI+ will not make a bitmap with a zero dimension and the failure is
        // an opaque status code rather than a sentence.
        assert_eq!(bounded_dimensions(4000, 3, 64), (64, 1));
        // Degenerate input from a caller that never validated: still a bitmap.
        assert_eq!(bounded_dimensions(0, 0, 1280), (1, 1));
    }
}
