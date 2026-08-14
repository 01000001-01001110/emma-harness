//! The override, end to end, against whatever shells this box actually has.
//!
//! Its own test binary, and one test function, on purpose: every assertion here
//! turns on `EMMA_SHELL`, which is process-global state. Split across several
//! `#[test]`s in one binary they would run in parallel and set it out from
//! under each other — the kind of flake that is blamed on the code for weeks.
//!
//! The resolution *order* is unit-tested in `bash.rs` against a fake
//! filesystem, where every branch is reachable on every platform. What is left
//! for here is the part no fake can answer: that the shell chosen can be
//! spawned, that the arguments handed to it are the ones it accepts, and that
//! the banner in the result describes the process that really ran.

mod support;

use emma_tools_fs::ShellKind;
use serde_json::json;
use support::Sandbox;

/// Sets `EMMA_SHELL` for the duration and puts it back, including on panic —
/// a leaked override would silently retarget every later assertion.
struct Override;

impl Override {
    fn set(value: Option<&str>) -> Self {
        match value {
            Some(v) => std::env::set_var("EMMA_SHELL", v),
            None => std::env::remove_var("EMMA_SHELL"),
        }
        Self
    }
}

impl Drop for Override {
    fn drop(&mut self) {
        std::env::remove_var("EMMA_SHELL");
    }
}

#[tokio::test]
async fn the_override_selects_refuses_and_is_reported() {
    let sandbox = Sandbox::new();

    // region: A named shell that is not there
    // The refusal that matters most. Falling back here would run a shell the
    // user did not ask for, and the wrong answer would arrive looking exactly
    // like the command's own output.
    {
        let _guard = Override::set(Some("/nowhere/definitely-not-a-shell"));
        let error = sandbox.err("Bash", json!({ "command": "echo hi" })).await;
        assert_eq!(error.kind(), "tool_unavailable", "{error}");
        assert!(error.detail().contains("definitely-not-a-shell"), "{error}");
        assert!(error.detail().contains("EMMA_SHELL"), "{error}");
    }
    // endregion: A named shell that is not there

    // region: WSL, asked for by name
    // Refused on every platform, so this assertion does not depend on WSL
    // being installed. The reasoning is in the message because a bare "no"
    // sends people straight to the workaround that reopens the hole.
    {
        let _guard = Override::set(Some("wsl"));
        let error = sandbox.err("Bash", json!({ "command": "echo hi" })).await;
        assert_eq!(error.kind(), "tool_unavailable", "{error}");
        assert!(error.detail().contains("WSL"), "{error}");
        assert!(error.detail().contains("namespace"), "{error}");
    }
    // endregion: WSL, asked for by name

    // region: PowerShell, when this box has one
    // The live half of the opt-in argument: PowerShell is reachable, it is
    // never reached by accident, and the arguments are the ones it accepts.
    // `-c` is a `pwsh` abbreviation that Windows PowerShell 5.1 does not have,
    // and getting it wrong yields a usage message that reads like output.
    if let Some(ps) = powershell_path() {
        let _guard = Override::set(Some(&ps));
        let shell = emma_tools_fs::resolve_shell().expect("powershell resolves");
        assert_eq!(shell.kind, ShellKind::PowerShell, "{shell}");

        let outcome = sandbox
            .ok("Bash", json!({ "command": "Write-Output marker-ps" }))
            .await;
        let first = outcome.content.lines().next().unwrap_or_default();
        assert!(first.starts_with("shell: powershell"), "{outcome:?}");
        assert!(outcome.content.contains("marker-ps"), "{outcome:?}");
        // The model is told what it has and adapts; nothing here rewrites the
        // command for it. This is the proof that the shell really was
        // PowerShell rather than a POSIX shell that happened to echo.
        let probe = sandbox
            .ok("Bash", json!({ "command": "$PSVersionTable.PSEdition" }))
            .await;
        assert!(
            probe.content.contains("Desktop") || probe.content.contains("Core"),
            "not actually PowerShell: {probe:?}"
        );
    } else {
        eprintln!("skipped: no PowerShell on this box");
    }
    // endregion: PowerShell, when this box has one

    // region: A second POSIX shell, when this box has one
    // The unix half of the same live claim, and it exists because without it
    // the whole "an override selects a different shell and it really runs"
    // guarantee was asserted on Windows only: `powershell_path` returns `None`
    // off Windows by construction, so on macOS and Linux the region above is
    // skipped and nothing left in this file proves the override *changes*
    // anything — the default-shell region below would pass with `EMMA_SHELL`
    // ignored entirely.
    //
    // A shell other than `/bin/sh`, so "it selected what I asked for" and "it
    // fell back to the default" cannot look alike; and the probe reads the
    // shell's own version variable, because a POSIX shell echoing a marker
    // proves only that *some* POSIX shell ran.
    if let Some((path, probe, marker)) = second_posix_shell() {
        let _guard = Override::set(Some(&path));
        let shell = emma_tools_fs::resolve_shell().expect("the named shell resolves");
        assert_eq!(shell.kind, ShellKind::Posix, "{shell}");
        assert_eq!(shell.path, std::path::Path::new(&path), "{shell}");

        let outcome = sandbox.ok("Bash", json!({ "command": probe })).await;
        assert_eq!(
            outcome.content.lines().next().unwrap_or_default(),
            shell.banner(),
            "{outcome:?}"
        );
        assert!(
            !outcome.content.contains("not-that-shell"),
            "{marker} was unset, so the shell that ran was not {path}: {outcome:?}"
        );
    } else {
        eprintln!("skipped: no second POSIX shell on this box");
    }
    // endregion: A second POSIX shell, when this box has one

    // region: Back to the default
    {
        let _guard = Override::set(None);
        let shell = emma_tools_fs::resolve_shell().expect("a default shell");
        assert_eq!(shell.kind, ShellKind::Posix, "{shell}");
        let outcome = sandbox.ok("Bash", json!({ "command": "echo hi" })).await;
        assert_eq!(
            outcome.content.lines().next().unwrap_or_default(),
            shell.banner(),
            "{outcome:?}"
        );
    }
    // endregion: Back to the default
}

/// A POSIX shell that is **not** the default `/bin/sh`, with a command that
/// makes it identify itself and the name of the variable that does so.
///
/// `zsh` first because it is the one macOS ships and the one whose version
/// variable a `sh` cannot fake; `bash` second for Linux. `None` on Windows,
/// where `/bin/sh` is not the default anyway and the PowerShell region above
/// carries this claim.
fn second_posix_shell() -> Option<(String, String, &'static str)> {
    for (path, marker) in [("/bin/zsh", "ZSH_VERSION"), ("/bin/bash", "BASH_VERSION")] {
        if std::path::Path::new(path).is_file() {
            return Some((
                path.to_string(),
                format!("echo \"${{{marker}:-not-that-shell}}\""),
                marker,
            ));
        }
    }
    None
}

/// `None` where the platform has no PowerShell, so the block above skips
/// loudly rather than being quietly dropped from the suite.
fn powershell_path() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let root = std::env::var("SYSTEMROOT").ok()?;
    let p = format!(r"{root}\System32\WindowsPowerShell\v1.0\powershell.exe");
    std::path::Path::new(&p).is_file().then_some(p)
}
