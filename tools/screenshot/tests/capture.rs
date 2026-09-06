//! What this suite is arranged to avoid, first: it never captures a screen,
//! never asks for Screen Recording permission, and never needs a desktop
//! session. It reaches the tool's own decisions through a scripted [`Capture`]
//! and the macOS backend's command shapes through a scripted [`Runner`], which
//! is also the only way to reach the failure shapes that matter, since a
//! non-zero exit and a zero-byte capture cannot be provoked on a working
//! machine.
//!
//! What it therefore does **not** reach, said plainly rather than left to be
//! discovered: none of `src/gdi.rs`'s Win32 calls. That backend has no seam
//! under it — it *is* the operating system — so the only test that can prove it
//! works is `a_real_windows_capture_writes_a_png_with_real_dimensions` at the
//! bottom of this file, which needs a signed-in desktop and is `#[ignore]`d.
//! The one part of it a screenless machine can check is the resize arithmetic,
//! and that is a unit test in the crate.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolOutcome};
use emma_tools_screenshot::{
    Backend, Capture, Commands, ImageDelivery, RunOutcome, Runner, Screenshot, Shot,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// Two scripted seams
// ---------------------------------------------------------------------------

/// What a scripted capture does when it is asked to shoot.
enum Scripted {
    /// Write these bytes as the full PNG, and `jpeg` as the bounded copy when
    /// one was asked for.
    Wrote { png: Vec<u8>, jpeg: Option<Vec<u8>> },
    /// The capture worked and the bounded copy did not. See `Capture::shoot`.
    Shrank(String),
    /// Nothing was captured.
    Refused(ToolError),
}

/// A [`Capture`] that does what it was told and records what it was asked.
struct Fake {
    backend: Backend,
    answer: Scripted,
    asked: Mutex<Vec<(String, Option<u32>)>>,
}

impl Fake {
    fn new(backend: Backend, answer: Scripted) -> Arc<Self> {
        Arc::new(Self {
            backend,
            answer,
            asked: Mutex::new(Vec::new()),
        })
    }

    /// The ordinary success: a 4096-byte PNG and a JPEG whose bytes the test
    /// can recognise on the other side of the base64.
    fn good() -> Arc<Self> {
        Self::new(
            Backend::Screencapture,
            Scripted::Wrote {
                png: vec![0u8; 4096],
                jpeg: Some(b"jpeg-bytes".to_vec()),
            },
        )
    }

    /// What the tool asked for: the description of the shot, and the bound if a
    /// bounded copy was wanted.
    fn asked(&self) -> Vec<(String, Option<u32>)> {
        self.asked.lock().unwrap().clone()
    }
}

impl Capture for Fake {
    fn backend(&self) -> Backend {
        self.backend
    }

    fn shoot(
        &self,
        shot: &Shot,
        full: &Path,
        bounded: Option<(&Path, u32)>,
    ) -> Result<Option<String>, ToolError> {
        self.asked
            .lock()
            .unwrap()
            .push((shot.what(), bounded.map(|(_, m)| m)));
        match &self.answer {
            Scripted::Wrote { png, jpeg } => {
                std::fs::write(full, png).expect("the fixture PNG");
                if let (Some((dest, _)), Some(bytes)) = (bounded, jpeg) {
                    std::fs::write(dest, bytes).expect("the fixture JPEG");
                }
                Ok(None)
            }
            Scripted::Shrank(why) => {
                std::fs::write(full, vec![9u8; 32]).expect("the fixture PNG");
                Ok(Some(why.clone()))
            }
            Scripted::Refused(e) => Err(match e {
                ToolError::Unavailable(s) => ToolError::Unavailable(s.clone()),
                ToolError::Failed(s) => ToolError::Failed(s.clone()),
                ToolError::BadArguments(s) => ToolError::BadArguments(s.clone()),
                // `ToolError` is `#[non_exhaustive]`, so a variant added later
                // lands here rather than breaking this file. Degrading it to
                // `Failed` keeps the text and loses only the taxonomy, which
                // is the right way round for a test helper.
                other => ToolError::Failed(other.detail().to_string()),
            }),
        }
    }
}

/// One scripted command's answer: the status it exits with, what it says on
/// stderr, and the bytes it leaves at its output path — `None` to leave
/// nothing, which is how a missing file is expressed.
type Answer = (Option<i32>, String, Option<Vec<u8>>);

/// One scripted answer per external command, in order. Under [`Commands`] only.
struct Script {
    answers: Mutex<Vec<Answer>>,
    calls: Mutex<Vec<(String, Vec<String>)>>,
    /// Set to fail the spawn itself rather than the command.
    spawn_error: bool,
}

impl Script {
    fn new(answers: Vec<Answer>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers),
            calls: Mutex::new(Vec::new()),
            spawn_error: false,
        })
    }

    /// Both commands succeed and write plausible bytes.
    fn working() -> Arc<Self> {
        Self::new(vec![
            (Some(0), String::new(), Some(vec![0u8; 4096])),
            (Some(0), String::new(), Some(b"jpeg-bytes".to_vec())),
        ])
    }

    fn unspawnable() -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            spawn_error: true,
        })
    }

    fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Runner for Script {
    fn run(&self, program: &Path, args: &[String]) -> std::io::Result<RunOutcome> {
        self.calls
            .lock()
            .unwrap()
            .push((program.to_string_lossy().to_string(), args.to_vec()));
        if self.spawn_error {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no such file",
            ));
        }
        let mut answers = self.answers.lock().unwrap();
        let (code, stderr, bytes) = if answers.is_empty() {
            (Some(0), String::new(), Some(b"fixture".to_vec()))
        } else {
            answers.remove(0)
        };
        if let Some(bytes) = bytes {
            // Both commands put their output path last, which is the one thing
            // this helper is allowed to assume about `Commands` and which the
            // argv tests below pin independently.
            let dest = args.last().expect("a command with an output path");
            std::fs::write(dest, bytes)?;
        }
        Ok(RunOutcome { code, stderr })
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn ctx(dir: &Path) -> ToolCtx {
    ToolCtx {
        cwd: dir.to_path_buf(),
        session_id: "sess-1".into(),
        turn_id: "turn-1".into(),
        background: Default::default(),
    }
}

fn blocks() -> ImageDelivery {
    ImageDelivery::Blocks {
        provider: "anthropic",
    }
}

fn tool(capture: Arc<dyn Capture>, delivery: ImageDelivery, root: PathBuf) -> Screenshot {
    Screenshot::with_capture(delivery, Some(capture), root)
}

fn commands(script: Arc<Script>, root: PathBuf) -> Screenshot {
    Screenshot::with_capture(blocks(), Some(Arc::new(Commands::new(script))), root)
}

async fn invoke(
    tool: &Screenshot,
    dir: &Path,
    args: serde_json::Value,
) -> Result<ToolOutcome, ToolError> {
    tool.invoke(&ctx(dir), args).await.expect("no fault")
}

// ---------------------------------------------------------------------------
// The tool's own decisions, through a scripted capture
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_successful_capture_attaches_the_bounded_copy_and_names_the_full_file() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = Fake::good();
    let t = tool(fake.clone(), blocks(), tmp.path().to_path_buf());
    let out = invoke(&t, tmp.path(), json!({})).await.expect("captured");

    assert_eq!(out.images.len(), 1, "the picture must be attached");
    assert_eq!(out.images[0].media_type, "image/jpeg");
    assert_eq!(
        out.images[0].data,
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"jpeg-bytes")
    );
    // The path recorded on the image is the full-resolution PNG, not the copy
    // that was sent: that is what the session log stores and what a human opens.
    let path = out.images[0].path.as_deref().expect("a path");
    assert!(path.ends_with(".png"), "recorded {path}");
    assert!(out.content.contains(path));
    assert!(out.content.contains("4096 bytes"));
    assert!(out.content.contains("1280 pixels"), "{}", out.content);
    assert_eq!(fake.asked(), vec![("display 1".to_string(), Some(1280))]);
}

#[tokio::test]
async fn a_region_and_a_bound_reach_the_backend_and_the_result_names_them() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = Fake::good();
    let t = tool(fake.clone(), blocks(), tmp.path().to_path_buf());
    let out = invoke(
        &t,
        tmp.path(),
        json!({ "region": {"x": 10, "y": 20, "w": 30, "h": 40}, "max_dimension": 640 }),
    )
    .await
    .expect("captured");

    assert_eq!(
        fake.asked(),
        vec![("the region 30x40 at (10,20)".to_string(), Some(640))]
    );
    assert!(out.content.contains("the region 30x40 at (10,20)"));
    assert!(out.content.contains("640 pixels"), "{}", out.content);
}

#[tokio::test]
async fn a_provider_that_cannot_carry_images_gets_the_path_and_the_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = Fake::good();
    let t = tool(
        fake.clone(),
        ImageDelivery::Unsupported {
            reason: "the frobnitz provider has no image path wired up in this build".into(),
        },
        tmp.path().to_path_buf(),
    );
    let out = invoke(&t, tmp.path(), json!({})).await.expect("captured");

    assert!(out.images.is_empty(), "nothing may be attached");
    assert!(out.content.contains("NOT attached"), "{}", out.content);
    assert!(out.content.contains("frobnitz"));
    assert!(out.content.contains(".png"));
    // The bounded copy is not even asked for, because there is nowhere to send
    // it. On macOS that saves a whole second process; on Windows a resize and a
    // JPEG encode.
    assert_eq!(fake.asked(), vec![("display 1".to_string(), None)]);
}

#[tokio::test]
async fn ollama_delivery_says_the_model_may_be_text_only() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tool(
        Fake::good(),
        ImageDelivery::Attached {
            provider: "ollama",
            model: "ornith:35b".into(),
        },
        tmp.path().to_path_buf(),
    );
    let out = invoke(&t, tmp.path(), json!({})).await.expect("captured");

    assert_eq!(out.images.len(), 1, "the bytes are sent either way");
    assert!(out.content.contains("ornith:35b"), "{}", out.content);
    assert!(out.content.contains("text-only"));
}

#[tokio::test]
async fn a_zero_byte_file_is_a_failure_rather_than_an_empty_picture() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = Fake::new(
        Backend::Screencapture,
        Scripted::Wrote {
            png: Vec::new(),
            jpeg: None,
        },
    );
    let t = tool(fake, blocks(), tmp.path().to_path_buf());
    let err = invoke(&t, tmp.path(), json!({}))
        .await
        .expect_err("refused");

    assert_eq!(err.kind(), "tool_failed");
    assert!(err.detail().contains("wrote no image"), "{}", err.detail());
    assert!(
        err.detail().contains("Screen Recording"),
        "{}",
        err.detail()
    );
}

#[tokio::test]
async fn a_failed_shrink_keeps_the_capture_and_says_what_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = Fake::new(
        Backend::Gdi,
        Scripted::Shrank("StretchBlt failed: 8".to_string()),
    );
    let t = tool(fake, blocks(), tmp.path().to_path_buf());
    let out = invoke(&t, tmp.path(), json!({})).await.expect("captured");

    assert!(out.images.is_empty());
    assert!(out.content.contains("NOT attached"), "{}", out.content);
    assert!(out.content.contains("StretchBlt failed: 8"));
    assert!(out.content.contains(".png"), "the capture is still named");
}

#[tokio::test]
async fn a_refused_capture_is_the_backends_own_words() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = Fake::new(
        Backend::Gdi,
        Scripted::Refused(ToolError::Unavailable(
            "Windows reported no displays".into(),
        )),
    );
    let t = tool(fake, blocks(), tmp.path().to_path_buf());
    let err = invoke(&t, tmp.path(), json!({}))
        .await
        .expect_err("refused");

    assert_eq!(err.kind(), "tool_unavailable");
    assert_eq!(err.detail(), "Windows reported no displays");
}

#[tokio::test]
async fn the_cursor_is_silently_honoured_on_macos_and_said_to_be_missing_on_windows() {
    // The ruling-4 shape in miniature: one platform honours the argument, the
    // other cannot, and the one that cannot says so in words rather than
    // accepting the flag and quietly ignoring it.
    for (backend, says) in [(Backend::Screencapture, false), (Backend::Gdi, true)] {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::new(
            backend,
            Scripted::Wrote {
                png: vec![0u8; 64],
                jpeg: Some(b"j".to_vec()),
            },
        );
        let t = tool(fake, blocks(), tmp.path().to_path_buf());
        let out = invoke(&t, tmp.path(), json!({ "cursor": true }))
            .await
            .expect("captured");

        assert_eq!(
            out.content.contains("mouse pointer is NOT drawn"),
            says,
            "{backend:?}: {}",
            out.content
        );
        // Still a picture either way: the capture is not refused over an option
        // one platform cannot honour.
        assert_eq!(out.images.len(), 1, "{backend:?}");
    }
}

#[tokio::test]
async fn captures_land_in_one_directory_per_session() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tool(Fake::good(), blocks(), tmp.path().to_path_buf());
    let out = invoke(&t, tmp.path(), json!({})).await.expect("captured");

    let expected = tmp.path().join("emma-screenshots").join("sess-1");
    assert!(
        out.images[0]
            .path
            .as_deref()
            .unwrap()
            .starts_with(expected.to_string_lossy().as_ref()),
        "{:?} is not under {}",
        out.images[0].path,
        expected.display()
    );
}

// ---------------------------------------------------------------------------
// What the macOS backend actually spells
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_macos_backend_runs_screencapture_then_sips() {
    let tmp = tempfile::tempdir().unwrap();
    let script = Script::working();
    let t = commands(script.clone(), tmp.path().to_path_buf());
    invoke(&t, tmp.path(), json!({})).await.expect("captured");

    let calls = script.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].0, "/usr/sbin/screencapture");
    assert!(calls[0].1.contains(&"-x".to_string()), "{:?}", calls[0].1);
    assert!(calls[0].1.contains(&"-D".to_string()));
    assert!(
        !calls[0].1.contains(&"-C".to_string()),
        "no cursor by default"
    );
    assert_eq!(calls[1].0, "/usr/bin/sips");
    assert!(calls[1].1.contains(&"1280".to_string()));
    assert!(calls[1].1.contains(&"jpeg".to_string()));
}

#[tokio::test]
async fn the_macos_backend_passes_the_rectangle_and_the_cursor_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let script = Script::working();
    let t = commands(script.clone(), tmp.path().to_path_buf());
    invoke(
        &t,
        tmp.path(),
        json!({ "region": {"x": 10, "y": 20, "w": 30, "h": 40}, "cursor": true }),
    )
    .await
    .expect("captured");

    let argv = &script.calls()[0].1;
    assert!(argv.contains(&"-C".to_string()));
    assert!(argv.contains(&"-R".to_string()));
    assert!(argv.contains(&"10,20,30,40".to_string()), "{argv:?}");
}

#[tokio::test]
async fn the_macos_backend_skips_sips_when_no_picture_is_wanted() {
    let tmp = tempfile::tempdir().unwrap();
    let script = Script::working();
    let t = Screenshot::with_capture(
        ImageDelivery::Unsupported {
            reason: "nowhere to send it".into(),
        },
        Some(Arc::new(Commands::new(script.clone()))),
        tmp.path().to_path_buf(),
    );
    invoke(&t, tmp.path(), json!({})).await.expect("captured");

    assert_eq!(script.calls().len(), 1, "{:?}", script.calls());
}

#[tokio::test]
async fn a_nonzero_screencapture_fails_and_names_the_permission() {
    let tmp = tempfile::tempdir().unwrap();
    let script = Script::new(vec![(Some(1), "could not create image".into(), None)]);
    let t = commands(script, tmp.path().to_path_buf());
    let err = invoke(&t, tmp.path(), json!({}))
        .await
        .expect_err("refused");

    assert_eq!(err.kind(), "tool_failed");
    assert!(err.detail().contains("exited 1"), "{}", err.detail());
    assert!(err.detail().contains("could not create image"));
    assert!(err.detail().contains("Screen Recording"));
}

#[tokio::test]
async fn an_unspawnable_screencapture_is_unavailable_rather_than_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let t = commands(Script::unspawnable(), tmp.path().to_path_buf());
    let err = invoke(&t, tmp.path(), json!({}))
        .await
        .expect_err("refused");

    assert_eq!(err.kind(), "tool_unavailable");
    assert!(err.detail().contains("/usr/sbin/screencapture"));
}

#[tokio::test]
async fn a_nonzero_sips_loses_the_picture_and_keeps_the_capture() {
    let tmp = tempfile::tempdir().unwrap();
    let script = Script::new(vec![
        (Some(0), String::new(), Some(vec![9u8; 32])),
        (Some(4), "sips: no such file".into(), None),
    ]);
    let t = commands(script, tmp.path().to_path_buf());
    let out = invoke(&t, tmp.path(), json!({})).await.expect("captured");

    assert!(out.images.is_empty());
    assert!(out.content.contains("NOT attached"), "{}", out.content);
    assert!(out.content.contains("sips exited 4"));
    assert!(out.content.contains("no such file"));
}

// ---------------------------------------------------------------------------
// The platform question, asked once
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_build_with_no_backend_refuses_and_names_the_platform() {
    let tmp = tempfile::tempdir().unwrap();
    let t = Screenshot::with_capture(blocks(), None, tmp.path().to_path_buf());
    let err = invoke(&t, tmp.path(), json!({}))
        .await
        .expect_err("refused");

    assert_eq!(err.kind(), "tool_unavailable");
    assert!(err.detail().contains(std::env::consts::OS));
}

/// Ruling 4, as an assertion rather than a promise: this machine's build must
/// pick the arm written for this machine. Without it a `cfg` typo makes
/// `host_capture` answer `None` on Windows and every capture is refused with a
/// sentence about macOS — which compiles, passes every scripted test above, and
/// is the exact failure the ruling names.
#[test]
fn the_host_picks_the_arm_written_for_it() {
    let expected = if cfg!(target_os = "macos") {
        Some(Backend::Screencapture)
    } else if cfg!(windows) {
        Some(Backend::Gdi)
    } else {
        None
    };
    assert_eq!(Backend::host(), expected);
    assert_eq!(
        emma_tools_screenshot::host_capture().map(|c| c.backend()),
        expected,
        "the descriptor and the thing that runs must agree"
    );
}

#[test]
fn the_arguments_are_checked_before_anything_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = Fake::good();
    let t = tool(fake.clone(), blocks(), tmp.path().to_path_buf());

    for bad in [
        json!({ "display": 0 }),
        json!({ "display": "main" }),
        json!({ "region": {"x": 0, "y": 0, "w": 0, "h": 10} }),
        json!({ "region": [0, 0, 10, 10] }),
        json!({ "max_dimension": 4000 }),
        json!({ "max_dimension": 8 }),
    ] {
        let err = t.validate_args(&bad).expect_err("rejected");
        assert_eq!(err.kind(), "bad_arguments", "for {bad}");
    }
    assert!(fake.asked().is_empty(), "validation must not capture");
}

#[test]
fn the_declaration_matches_what_it_does() {
    let t = tool(Fake::good(), blocks(), std::env::temp_dir());
    let meta = t.meta();
    // It writes a file, so it is not read-only, and it is therefore not exempt
    // from the approval gate. It sends nothing off this machine.
    assert!(!meta.read_only);
    assert!(!meta.reaches_network);
    assert!(!meta.idempotent);
    assert!(t.network_target(&json!({})).is_none());
}

// ---------------------------------------------------------------------------
// The one thing no seam can prove
// ---------------------------------------------------------------------------

/// A real capture, through the real backend, on the real screen.
///
/// `#[ignore]`d, and that is not a hedge: it needs a signed-in interactive
/// desktop, which a CI agent and a locked workstation do not have, and a test
/// that fails there would be turned off by the second person who saw it. Run it
/// with `--ignored` on a machine somebody is logged into. Everything above this
/// line is a scripted seam agreeing with its author; this is the only test in
/// the file that could have caught — and did catch, on its first run — a
/// Windows backend that does not survive contact with the operating system.
///
/// **And it is still not enough on its own, which is the more useful lesson.**
/// Every assertion below passed on a bounded copy that was solid black, because
/// a black JPEG has a signature, has dimensions and is smaller than the PNG.
/// What found that bug was opening the file. So the test can be told to keep
/// the copy — `EMMA_SHOT_KEEP=some.jpg cargo test ... -- --ignored` — and the
/// size floor further down is the cheapest available stand-in for a human
/// looking, with the two measurements that set it written beside it.
#[cfg(windows)]
#[tokio::test]
#[ignore = "captures the real screen; needs a signed-in desktop session"]
async fn a_real_windows_capture_writes_a_png_with_real_dimensions() {
    let tmp = tempfile::tempdir().unwrap();
    let t = Screenshot::with_capture(
        blocks(),
        emma_tools_screenshot::host_capture(),
        tmp.path().to_path_buf(),
    );
    let out = invoke(&t, tmp.path(), json!({})).await.expect("captured");
    println!("{}", out.content);

    let path = out.images[0].path.as_deref().expect("a path");
    let bytes = std::fs::read(path).expect("the PNG on disk");
    // The eight-byte signature, then the IHDR width and height, big-endian.
    // Read from the file rather than trusted from the tool: a `Save` that wrote
    // some other format under a `.png` name would pass every other assertion.
    assert_eq!(
        &bytes[..8],
        b"\x89PNG\r\n\x1a\n",
        "not a PNG: {:?}",
        &bytes[..8]
    );
    let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    println!("real capture: {w}x{h} PNG, {} bytes", bytes.len());
    assert!(w > 0 && h > 0, "{w}x{h}");

    // The bounded copy is a JPEG, is bounded, and is not the whole desktop
    // again: a resize that silently did nothing would still be a valid JPEG.
    let jpeg = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &out.images[0].data,
    )
    .expect("base64");
    assert_eq!(&jpeg[..2], b"\xff\xd8", "not a JPEG");
    assert!(jpeg.len() < bytes.len(), "the copy is not smaller");
    // A smoke test for "the blit actually read the desktop", not a proof. The
    // two numbers behind it, on a 1920x1080 primary display: the black copy the
    // double-`SelectObject` bug produced was 15,027 bytes, and the real desktop
    // beside it was 159,551. A genuinely blank screen would trip this, which is
    // the right direction to fail in — loudly, rather than sending black.
    assert!(
        jpeg.len() > 20_000,
        "{} bytes is the size of a uniform image; look at it",
        jpeg.len()
    );
    println!("bounded JPEG: {} bytes", jpeg.len());
    // See the doc above: the assertions cannot see what is in the picture.
    if let Ok(keep) = std::env::var("EMMA_SHOT_KEEP") {
        std::fs::write(&keep, &jpeg).expect("kept");
        println!("kept at {keep}");
    }
}
