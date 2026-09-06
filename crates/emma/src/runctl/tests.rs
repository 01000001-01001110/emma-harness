//! The verification tests are the point of this file.
//!
//! Not one of them sends a signal. [`Fake`] answers the two questions the real
//! implementation asks the operating system and records what it was asked to
//! send, so "would this have signalled the wrong process" is a value to assert
//! on rather than an experiment to run on the developer's machine.
//!
//! Three tests do touch the real machine, and they are marked `#[cfg(windows)]`
//! because they are the Windows implementations this port had to write from
//! nothing: naming a live process, refusing to name a dead one, and starting a
//! detached child. A fake cannot certify any of those — it would only prove the
//! fake agrees with itself.

use std::cell::RefCell;

use super::*;

/// A recording [`Control`]. `names` is the process table this test believes
/// in; `sent` is every signal that got past verification.
struct Fake {
    me: u32,
    names: Vec<(u32, &'static str)>,
    sent: RefCell<Vec<(u32, Action)>>,
}

impl Fake {
    fn new(names: &[(u32, &'static str)]) -> Self {
        Self {
            me: 4242,
            names: names.to_vec(),
            sent: RefCell::new(Vec::new()),
        }
    }
}

impl Control for Fake {
    fn me(&self) -> u32 {
        self.me
    }
    fn name_of(&self, pid: u32) -> Option<String> {
        self.names
            .iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, n)| n.to_string())
    }
    fn send(&self, pid: u32, action: Action) -> Result<(), Refusal> {
        self.sent.borrow_mut().push((pid, action));
        Ok(())
    }
}

fn target(session: &str, cwd: Option<&str>) -> Target {
    Target {
        session: session.to_string(),
        cwd: cwd.map(str::to_string),
        finished: false,
    }
}

// region: The pid inside an id

#[test]
fn a_session_id_carries_the_pid_that_wrote_it() {
    assert_eq!(pid_of("sess-1756000000000-9931"), Some(9931));
}

#[test]
fn an_id_that_is_not_ours_yields_no_pid() {
    for id in [
        "sess-none",
        "notes-1756000000000-9931",
        "sess-1756000000000",
        "sess-abc-9931",
        "sess-1756000000000-9931x",
        "sess-1756000000000-",
    ] {
        assert_eq!(pid_of(id), None, "{id} should carry no pid");
    }
}

#[test]
fn init_and_the_process_group_are_never_a_run() {
    // 0 means "every process in my group", which would stop the terminal that
    // is asking. 1 is init. Both parse as numbers and neither is a run.
    assert_eq!(pid_of("sess-1756000000000-0"), None);
    assert_eq!(pid_of("sess-1756000000000-1"), None);
}

// endregion: The pid inside an id

// region: Naming the binary

/// **The leaf name is not the same string on both platforms, and the guard is
/// only as good as the comparison.** Windows reports `emma.exe` from a
/// case-insensitive file system; Unix reports `emma` from a case-sensitive one.
/// A bare `==` would have made every Windows `verify` answer `NotEmma`, which
/// reads as "the pid was reused" — a wrong sentence, and one that would have
/// sent a reader looking for a recycled pid that never happened.
#[test]
fn the_binary_is_recognised_the_way_this_platform_names_it() {
    assert!(is_emma("emma"));
    assert!(!is_emma("postgres"));
    assert!(!is_emma("emmaline"));
    // Never a substring, and never a path: `name_of` returns a leaf.
    assert!(!is_emma("not-emma"));

    #[cfg(windows)]
    {
        assert!(is_emma("emma.exe"), "the Windows image name carries .exe");
        assert!(is_emma("EMMA.EXE"), "that file system is case-insensitive");
        assert!(!is_emma("emma.exe.bak"));
    }
    #[cfg(not(windows))]
    {
        // Exact on Unix on purpose: `EMMA` there is a different program, and
        // reaching into it would be this module acting outside what it checked.
        assert!(!is_emma("EMMA"));
        assert!(!is_emma("emma.exe"));
    }
}

// endregion: Naming the binary

// region: Verification

#[test]
fn a_live_emma_run_in_this_repo_verifies() {
    let c = Fake::new(&[(9931, "emma")]);
    let got = verify(
        &c,
        &target("sess-1756000000000-9931", Some("/repo")),
        "/repo",
    );
    assert_eq!(got, Ok(9931));
}

#[test]
fn a_recycled_pid_running_something_else_is_refused() {
    let c = Fake::new(&[(9931, "postgres")]);
    let got = verify(
        &c,
        &target("sess-1756000000000-9931", Some("/repo")),
        "/repo",
    );
    assert_eq!(
        got,
        Err(Refusal::NotEmma {
            pid: 9931,
            name: "postgres".into()
        })
    );
    assert!(c.sent.borrow().is_empty(), "a refusal must not signal");
}

#[test]
fn a_dead_pid_is_refused_rather_than_signalled_blind() {
    let c = Fake::new(&[]);
    assert_eq!(
        verify(
            &c,
            &target("sess-1756000000000-9931", Some("/repo")),
            "/repo"
        ),
        Err(Refusal::Gone(9931))
    );
}

#[test]
fn this_terminals_own_process_is_never_signalled() {
    let c = Fake::new(&[(4242, "emma")]);
    assert_eq!(
        verify(
            &c,
            &target("sess-1756000000000-4242", Some("/repo")),
            "/repo"
        ),
        Err(Refusal::Myself(4242))
    );
}

#[test]
fn a_run_from_another_repository_is_refused() {
    let c = Fake::new(&[(9931, "emma")]);
    assert_eq!(
        verify(
            &c,
            &target("sess-1756000000000-9931", Some("/elsewhere")),
            "/repo"
        ),
        Err(Refusal::Elsewhere {
            want: "/repo".into(),
            got: "/elsewhere".into()
        })
    );
}

#[test]
fn a_run_with_no_recorded_directory_is_refused() {
    let c = Fake::new(&[(9931, "emma")]);
    assert!(matches!(
        verify(&c, &target("sess-1756000000000-9931", None), "/repo"),
        Err(Refusal::Elsewhere { .. })
    ));
}

#[test]
fn a_finished_run_has_nothing_to_signal() {
    let c = Fake::new(&[(9931, "emma")]);
    let mut t = target("sess-1756000000000-9931", Some("/repo"));
    t.finished = true;
    assert_eq!(verify(&c, &t, "/repo"), Err(Refusal::Finished));
}

#[test]
fn control_sends_exactly_one_signal_and_only_after_verifying() {
    let c = Fake::new(&[(9931, "emma")]);
    let t = target("sess-1756000000000-9931", Some("/repo"));
    assert_eq!(control(&c, &t, "/repo", Action::Pause), Ok(9931));
    assert_eq!(control(&c, &t, "/repo", Action::Resume), Ok(9931));
    assert_eq!(
        *c.sent.borrow(),
        vec![(9931, Action::Pause), (9931, Action::Resume)]
    );
}

#[test]
fn every_refusal_path_leaves_the_process_table_untouched() {
    // One fake, every way in: the property that matters is that no path
    // reaches `send` except the verified one.
    let c = Fake::new(&[(9931, "nginx"), (4242, "emma")]);
    let cases = [
        target("sess-none", Some("/repo")),
        target("sess-1756000000000-9931", Some("/repo")),
        target("sess-1756000000000-4242", Some("/repo")),
        target("sess-1756000000000-9931", Some("/elsewhere")),
    ];
    for t in cases {
        assert!(
            control(&c, &t, "/repo", Action::Cancel).is_err(),
            "{t:?} should have been refused"
        );
    }
    assert!(c.sent.borrow().is_empty());
}

#[test]
fn the_three_actions_name_the_signals_they_send() {
    assert_eq!(Action::Pause.signal_name(), "SIGSTOP");
    assert_eq!(Action::Resume.signal_name(), "SIGCONT");
    // Never SIGKILL: an interrupted run writes its own ending, a killed one
    // leaves a log indistinguishable from a crash.
    assert_eq!(Action::Cancel.signal_name(), "SIGINT");
}

#[test]
fn a_target_is_read_out_of_a_run_row_rather_than_assembled_by_hand() {
    // The trap this closes: `RunRow::id` is `<session>#<n>` for a goal, so a
    // page that passed the id where the session belongs would take the pid out
    // of `…-9931#1` and get nothing. The row comes from the real reader rather
    // than a literal, so the two modules are shown to agree about a log rather
    // than asserted to.
    let dir = tempfile::tempdir().unwrap();
    let session = "sess-1756000000000-9931";
    let path = dir.path().join(format!("{session}.jsonl"));
    std::fs::write(
        &path,
        "{\"kind\":\"goal\",\"at_ms\":1,\"text\":\"hello\",\"cwd\":\"/repo\"}\n",
    )
    .unwrap();

    let feed = crate::harness_state::runs(dir.path(), 2).unwrap();
    let row = feed.runs.first().expect("one run");
    assert!(row.id.starts_with(session), "the id is not the session id");
    assert_ne!(row.id, row.session, "and this is the trap being closed");
    let t = Target::of(row);
    assert_eq!(pid_of(&t.session), Some(9931));
    assert_eq!(t.cwd.as_deref(), Some("/repo"));
    assert!(!t.finished, "no ending means the run is still open");

    std::fs::write(
        &path,
        "{\"kind\":\"goal\",\"at_ms\":1,\"text\":\"hello\",\"cwd\":\"/repo\"}\n\
         {\"kind\":\"goal_finished\",\"at_ms\":2,\"ending\":\"done\"}\n",
    )
    .unwrap();
    let feed = crate::harness_state::runs(dir.path(), 3).unwrap();
    let t = Target::of(feed.runs.first().expect("one run"));
    assert!(t.finished, "an ending is what finished means");
}

// endregion: Verification

// region: What this platform can and cannot do

/// **The page must be able to ask before it draws.**
///
/// A control that is present and refuses on every press is exactly the failure
/// this module was written to end — the Harness page used to advertise controls
/// against a scheduler that did not exist. `supported` is how the page prints
/// the sentence instead, so this test pins that the answer tracks the platform
/// rather than being a cheerful constant.
#[test]
fn the_three_controls_are_offered_only_where_they_exist() {
    for action in [Action::Pause, Action::Resume, Action::Cancel] {
        assert_eq!(
            supported(action),
            cfg!(unix),
            "{action:?} is a unix signal and nothing else"
        );
    }
}

/// The refusal a Windows reader actually sees, in words they can act on.
///
/// Asserted on the real [`Os`], not the fake: `Os::send` is the thing that
/// could have been written as a silent `Ok(())`, and pid 4 is `System`, which
/// this test never touches because the refusal is returned before anything is
/// sent.
#[cfg(windows)]
#[test]
fn windows_refuses_all_three_controls_and_says_why() {
    for action in [Action::Pause, Action::Resume, Action::Cancel] {
        let got = Os.send(4, action);
        assert_eq!(got, Err(Refusal::Unsupported(action)));
        let said = Refusal::Unsupported(action).to_string();
        assert!(
            said.contains("Windows"),
            "the sentence must name the platform: {said}"
        );
        assert!(
            !said.contains("SIGSTOP") || action == Action::Pause,
            "a Windows reader is not helped by a signal name: {said}"
        );
    }
    // Cancel is the one that is possible and deliberately not done, and the
    // reader is told that rather than left to look for the option.
    let cancel = Refusal::Unsupported(Action::Cancel).to_string();
    assert!(
        cancel.contains("TerminateProcess"),
        "Cancel's refusal must say what was refused and why: {cancel}"
    );
}

/// **`name_of` against the real operating system, which is the only way to
/// know it works.**
///
/// The fork returned `None` here on every non-Unix build, and a fake would
/// happily agree with a reimplementation that did the same. This asks Windows
/// about a process that certainly exists — this test binary — and about one
/// that certainly does not.
#[cfg(windows)]
#[test]
fn a_live_process_can_be_named_on_windows_and_a_dead_one_cannot() {
    let me = std::process::id();
    let name = Os.name_of(me).expect("this process is running");
    let expected = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        .expect("the test binary has a path");
    assert_eq!(name, expected, "the leaf of the image path, with its .exe");
    assert!(
        name.to_ascii_lowercase().ends_with(".exe"),
        "and it carries the extension `is_emma` exists to tolerate: {name}"
    );

    // A pid nothing can hold. Odd pids do not exist on Windows (pids are
    // multiples of four), so this cannot race a real process into existence.
    assert_eq!(Os.name_of(0xFFFF_FFFF), None);
    assert_eq!(Os.name_of(3), None);
}

// endregion: What this platform can and cannot do

// region: Recording

#[test]
fn a_control_action_is_appended_to_the_runs_own_log() {
    let dir = tempfile::tempdir().unwrap();
    let session = "sess-1756000000000-9931";
    std::fs::write(
        dir.path().join(format!("{session}.jsonl")),
        "{\"kind\":\"goal\",\"at_ms\":1}\n",
    )
    .unwrap();
    record(dir.path(), session, Action::Pause, 9931).unwrap();

    let raw = std::fs::read_to_string(dir.path().join(format!("{session}.jsonl"))).unwrap();
    assert!(
        !raw.contains('\r'),
        "the session log is LF-delimited on every platform: {raw:?}"
    );
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2, "the append must not rewrite the file");
    assert!(lines[0].contains("\"goal\""));
    let rec: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(rec["kind"], "harness_control");
    assert_eq!(rec["action"], "pause");
    assert_eq!(rec["signal"], "SIGSTOP");
    assert_eq!(rec["pid"], 9931);
    // The same clock every other record in this file is stamped with, so a
    // reader folding the log sorts them together rather than putting this one
    // at the epoch.
    let at = rec["at_ms"].as_u64().expect("a timestamp");
    assert!(
        at.abs_diff(crate::session::now_ms()) < 60_000,
        "at_ms is not this machine's clock: {at}"
    );
}

// endregion: Recording

// region: Archive and delete

/// A run nothing is holding open: the pid in the id names no process, so
/// `verify` refuses and the archive is safe. This is what every archive test
/// below except the live one is about.
fn dead() -> Fake {
    Fake::new(&[])
}

/// The target the three archive tests share. `finished: false` deliberately:
/// an unfinished run whose process is gone is exactly the case archiving must
/// still allow, and pinning it here keeps the liveness check honest rather
/// than letting `Refusal::Finished` do the work.
fn archivable(session: &str) -> Target {
    target(session, Some("/repo"))
}

#[test]
fn archiving_moves_the_file_out_of_the_directory_the_feeds_read() {
    let dir = tempfile::tempdir().unwrap();
    let session = "sess-1756000000000-9931";
    let file = dir.path().join(format!("{session}.jsonl"));
    std::fs::write(&file, "{\"kind\":\"goal\"}\n").unwrap();

    let to = archive(&dead(), dir.path(), &archivable(session), "/repo").unwrap();
    assert!(
        !file.exists(),
        "the original must be gone from the top level"
    );
    assert!(to.exists(), "the archived copy must be there");
    assert_eq!(to.parent().unwrap().file_name().unwrap(), ARCHIVE_DIR);
    // The bytes are the transcript; archiving must not touch them.
    assert_eq!(
        std::fs::read_to_string(&to).unwrap(),
        "{\"kind\":\"goal\"}\n"
    );
}

/// **Archiving a live run would split its transcript in two.**
///
/// The run holds the jsonl open for append. A rename does not stop it writing:
/// the handle follows the inode, so everything after the rename lands in a file
/// with no name and the archived copy is missing exactly that tail. `record` on
/// this module documents the same hazard and appends rather than renames; this
/// is the operation that has no such way out.
///
/// The `Fake` process table is what makes "live" a value rather than an
/// experiment: pid 9931 is an `emma`, in the same directory, with no ending
/// written, which is every condition `verify` requires.
#[test]
fn archiving_a_live_run_is_refused_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let session = "sess-1756000000000-9931";
    let file = dir.path().join(format!("{session}.jsonl"));
    std::fs::write(&file, "{\"kind\":\"goal\"}\n").unwrap();

    let live = Fake::new(&[(9931, "emma")]);
    let err = archive(&live, dir.path(), &archivable(session), "/repo")
        .unwrap_err()
        .to_string();
    assert!(err.contains("still running"), "{err}");
    assert!(err.contains("9931"), "the notice must name the pid: {err}");
    assert!(
        file.exists(),
        "the refusal must leave the transcript where the live process is writing it"
    );
    assert!(
        !dir.path().join(ARCHIVE_DIR).exists(),
        "a refused archive must not even make the directory"
    );

    // And the same run, once its process is gone, archives as it always did.
    // Same target, same file: only liveness changed.
    archive(&dead(), dir.path(), &archivable(session), "/repo").unwrap();
    assert!(!file.exists());
}

/// **The Windows hazard the fork could not see, because on the fork's Windows
/// build nothing could be named at all.**
///
/// `verify` refuses a run in another directory, and the Unix argument reads
/// that refusal as "not signallable, therefore not live". It is not: the
/// process may be running and writing. On Unix the consequence is bounded — the
/// page lists only rows whose `cwd` matched — but on Windows a rename succeeds
/// over the writer's own handle (`FILE_SHARE_DELETE`), silently, so the
/// stricter question is asked there: can we *show* nothing is running?
#[cfg(windows)]
#[test]
fn windows_refuses_to_archive_a_run_it_cannot_show_has_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let session = "sess-1756000000000-9931";
    let file = dir.path().join(format!("{session}.jsonl"));
    std::fs::write(&file, "{\"kind\":\"goal\"}\n").unwrap();

    // Alive, unfinished, and in another directory — so `verify` refuses with
    // `Elsewhere` and the Unix rule would have archived it.
    let live = Fake::new(&[(9931, "emma.exe")]);
    let elsewhere = target(session, Some("/elsewhere"));
    let err = archive(&live, dir.path(), &elsewhere, "/repo")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("cannot be shown to have stopped"),
        "the refusal must say what it could not establish: {err}"
    );
    assert!(file.exists(), "the transcript must be left where it is");

    // The guard is evidence, not pessimism: the same unfinished run in the same
    // other directory archives once its pid names nothing.
    archive(&dead(), dir.path(), &elsewhere, "/repo").unwrap();
    assert!(!file.exists());
}

#[test]
fn an_archived_session_leaves_the_feed() {
    let dir = tempfile::tempdir().unwrap();
    let session = "sess-1756000000000-9931";
    std::fs::write(
        dir.path().join(format!("{session}.jsonl")),
        "{\"kind\":\"goal\",\"at_ms\":1,\"text\":\"hello\",\"cwd\":\"/repo\"}\n",
    )
    .unwrap();
    assert_eq!(
        crate::harness_state::runs(dir.path(), 2)
            .unwrap()
            .runs
            .len(),
        1
    );
    archive(&dead(), dir.path(), &archivable(session), "/repo").unwrap();
    let feed = crate::harness_state::runs(dir.path(), 2).unwrap();
    assert!(
        feed.runs.is_empty(),
        "archived runs must not still be listed"
    );
    assert!(feed.unreadable.is_empty(), "and must not read as damage");
}

#[test]
fn archiving_twice_says_so_rather_than_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    let session = "sess-1756000000000-9931";
    let file = dir.path().join(format!("{session}.jsonl"));
    std::fs::write(&file, "first\n").unwrap();
    archive(&dead(), dir.path(), &archivable(session), "/repo").unwrap();
    std::fs::write(&file, "second\n").unwrap();

    let err = archive(&dead(), dir.path(), &archivable(session), "/repo")
        .unwrap_err()
        .to_string();
    assert!(err.contains("already archived"), "{err}");
    let kept = std::fs::read_to_string(
        dir.path()
            .join(ARCHIVE_DIR)
            .join(format!("{session}.jsonl")),
    )
    .unwrap();
    assert_eq!(kept, "first\n", "the archived transcript must survive");
}

#[test]
fn deleting_removes_the_file_and_a_missing_one_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let session = "sess-1756000000000-9931";
    let file = dir.path().join(format!("{session}.jsonl"));
    std::fs::write(&file, "{}\n").unwrap();
    delete(dir.path(), session).unwrap();
    assert!(!file.exists());
    assert!(delete(dir.path(), session).is_err());
}

#[test]
fn a_session_name_cannot_escape_the_directory() {
    // The ids come from file names this program wrote, but the two file
    // operations here are the destructive ones and a name with a separator in
    // it must not resolve outside the session directory. Both separators are
    // refused on both platforms: a `/` is a separator on Windows too, and a
    // `\` inside a name is not a character any session id has.
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("keep.jsonl");
    std::fs::write(&outside, "keep\n").unwrap();
    let sub = dir.path().join("sessions");
    std::fs::create_dir_all(&sub).unwrap();
    for escape in ["../keep", "..\\keep", "..", "", "sub/keep"] {
        assert!(
            delete(&sub, escape).is_err(),
            "`{escape}` must not name a file"
        );
    }
    assert!(outside.exists(), "a traversal must not have deleted it");
}

// endregion: Archive and delete

// region: Launching

#[test]
fn a_launch_is_the_headless_flag_the_goal_and_the_repo() {
    let l = launch_for(
        PathBuf::from("/usr/local/bin/emma"),
        "add a test",
        Path::new("/repo"),
    );
    assert_eq!(l.program, PathBuf::from("/usr/local/bin/emma"));
    // The goal is one argument, not shell text: a goal containing quotes or
    // semicolons is a goal, never a command.
    assert_eq!(l.args, vec!["-p".to_string(), "add a test".to_string()]);
    assert_eq!(l.cwd, PathBuf::from("/repo"));
}

#[test]
fn a_goal_with_shell_metacharacters_stays_one_argument() {
    let l = launch_for(
        PathBuf::from("emma"),
        "rm -rf / ; echo $HOME",
        Path::new("/repo"),
    );
    assert_eq!(l.args.len(), 2);
    assert_eq!(l.args[1], "rm -rf / ; echo $HOME");
}

/// **The detached spawn, run for real, because the flags are the whole
/// behaviour and a unit test of a `Command` builder proves nothing.**
///
/// `cmd.exe` rather than `emma`: this suite never runs the binary under test.
/// The child writes a file and exits, so "it actually ran, with no console and
/// with all three streams null" is a fact on disk rather than a claim.
#[cfg(windows)]
#[test]
fn a_detached_child_really_starts_on_windows() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran.txt");
    let launch = Launch {
        program: PathBuf::from("cmd.exe"),
        // `copy nul <file>` rather than an `echo … >` redirection: Rust quotes
        // each argument, and a single argument carrying its own quotes is
        // re-quoted into something `cmd /C` unwraps by rules nobody should have
        // to reason about. Three plain arguments have no such rule.
        args: vec![
            "/C".into(),
            "copy".into(),
            "nul".into(),
            marker.display().to_string(),
        ],
        cwd: dir.path().to_path_buf(),
    };
    let pid = spawn(&launch).expect("a detached child must start");
    assert_ne!(pid, 0);
    assert_ne!(pid, std::process::id(), "that is this process, not a child");

    // Bounded wait: the child is detached, so there is no handle to join on —
    // which is exactly the property under test.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !marker.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        marker.exists(),
        "the detached child never ran; DETACHED_PROCESS or the null stdio is wrong"
    );
}

// endregion: Launching

// region: Next-run policy

fn allow_of(file: &Path) -> Vec<String> {
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    doc["permissions"]["allow"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn deny_of(file: &Path) -> Vec<String> {
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    doc["permissions"]["deny"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn safe_pre_approves_only_the_reading_tools() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.local.json");
    write_policy(&file, Policy::Safe).unwrap();
    let allow = allow_of(&file);
    assert!(allow.contains(&"Read".to_string()));
    for writer in ["Write", "Edit", "Bash", "KillShell", "Delegate", "WebFetch"] {
        assert!(
            !allow.contains(&writer.to_string()),
            "{writer} must not be pre-approved by Safe"
        );
    }
    assert!(deny_of(&file).is_empty());
}

#[test]
fn manual_pre_approves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.local.json");
    write_policy(&file, Policy::Safe).unwrap();
    write_policy(&file, Policy::Manual).unwrap();
    assert!(
        allow_of(&file).is_empty(),
        "Safe's grants must be withdrawn"
    );
    assert!(deny_of(&file).is_empty());
}

#[test]
fn deny_all_names_every_tool_because_the_rule_syntax_forbids_a_star() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.local.json");
    write_policy(&file, Policy::DenyAll).unwrap();
    let deny = deny_of(&file);
    for tool in ALL_TOOLS {
        assert!(deny.contains(&tool.to_string()), "{tool} missing from deny");
    }
    // And each one must be a rule this build can actually parse, or the deny
    // list is a protection nobody has.
    for rule in &deny {
        assert!(
            crate::permissions::Rule::parse(rule).is_ok(),
            "{rule} does not parse as a permission rule"
        );
    }
}

#[test]
fn writing_a_policy_keeps_every_other_key_in_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.local.json");
    std::fs::write(
        &file,
        r#"{"hooks":{"PreToolUse":[1]},"permissions":{"allow":["WebFetch(domain:docs.rs)","Bash"],"ask":["Write"]}}"#,
    )
    .unwrap();
    write_policy(&file, Policy::Safe).unwrap();

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(doc["hooks"]["PreToolUse"][0], 1, "another key was lost");
    let allow = allow_of(&file);
    assert!(
        allow.contains(&"WebFetch(domain:docs.rs)".to_string()),
        "a hand-written specifier grant must survive: {allow:?}"
    );
    assert!(
        !allow.contains(&"Bash".to_string()),
        "the bare grant this page manages must be replaced"
    );
    assert_eq!(
        doc["permissions"]["ask"][0], "Write",
        "`ask` is not ours to touch"
    );
}

#[test]
fn a_settings_file_that_is_not_json_is_not_written_to() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.local.json");
    std::fs::write(&file, "{ not json").unwrap();
    assert!(write_policy(&file, Policy::Safe).is_err());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "{ not json",
        "a file we could not parse must be left exactly as it was"
    );
}

#[test]
fn the_policy_written_is_the_policy_read_back_by_the_rule_engine() {
    // The round trip that matters: what this writes has to be what
    // `permissions::Rules` decides with, not merely valid JSON.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.local.json");
    write_policy(&file, Policy::DenyAll).unwrap();
    let deny = deny_of(&file);
    let entries: Vec<emma_harness::PermissionEntry> = deny
        .iter()
        .map(|r| emma_harness::PermissionEntry {
            rule: r.clone(),
            kind: emma_harness::PermissionKind::Deny,
            source: file.clone(),
        })
        .collect();
    let (rules, problems) = crate::permissions::Rules::parse(&entries);
    assert!(problems.is_empty(), "{problems:?}");
    for tool in ["Bash", "BashOutput", "KillShell"] {
        assert_eq!(
            rules.for_tool(tool),
            Some(crate::permissions::Decision::Deny),
            "{tool} is registered and Deny All must reach it"
        );
    }
}

/// Every registered tool, built the way `main.rs` builds it.
///
/// Nothing is *detected*: `web_tools` registers the browser tools only when it
/// finds Chrome, and a test whose coverage depended on the machine it ran on
/// would pass on CI while the list rotted. The constructors underneath are
/// called directly instead, so every name this build can register is present
/// whatever is installed.
fn every_registered_name() -> Vec<&'static str> {
    use emma_tool_api::Tool;
    use std::sync::Arc;

    let mut registered: Vec<&'static str> = Vec::new();
    let mut take = |tools: Vec<Arc<dyn Tool>>| {
        registered.extend(tools.iter().map(|t| t.name()));
    };

    let (fs, _tracker) = emma_tools_fs::fs_tools();
    take(fs);
    take(emma_tools_tasks::task_tools());
    let (lsp, _pool) = emma_tools_lsp::lsp_tools();
    take(lsp);
    take(vec![
        Arc::new(emma_tools_web::fetch::WebFetch::new()) as Arc<dyn Tool>,
        Arc::new(emma_tools_web::search::WebSearch::new()) as Arc<dyn Tool>,
    ]);
    let (browser, _browser_pool) = emma_tools_web::browser::browser_tools(None);
    take(browser);
    // The two `main.rs` registers conditionally. `Skill` needs a harness that
    // resolved skills and `Delegate` is built from a live provider, so both are
    // named rather than constructed; naming them here is what keeps them inside
    // the assertion instead of outside it. `Skill`'s own `NAME` is private to
    // that module, so the string is repeated — the tools' own name tests are
    // what hold it.
    registered.push("Skill");
    registered.push(crate::delegate::NAME);
    registered
}

/// **The list must break loudly when a tool is added.**
///
/// `ALL_TOOLS` is a second, hand-kept copy of the tool surface, and `Deny All`
/// spells out every name in it because Emma refuses a `*` in the tool slot of a
/// rule. A hand-kept copy drifts, and this one arrived drifted: the macOS fork
/// this was ported from listed `Diagnostics`, `NotebookEdit` and `Screenshot`,
/// which this build does not have, and omitted `BashOutput` and `KillShell`,
/// which it does — so a `Deny All` written from the fork's list would have left
/// the two background-execution tools callable, with nothing to show it.
///
/// The assertion is one-directional on purpose. A registered name missing from
/// `ALL_TOOLS` is the defect: it is a tool the user believed they had denied.
/// A name in `ALL_TOOLS` that nothing registers is harmless, because denying a
/// tool that does not exist denies nothing, and requiring the reverse would
/// make deleting a tool a two-file change for no safety.
#[test]
fn every_registered_tool_is_in_the_deny_all_list() {
    let registered = every_registered_name();
    let missing: Vec<&&str> = registered
        .iter()
        .filter(|name| !ALL_TOOLS.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "these tools are registered and would survive Deny All: {missing:?}"
    );
    assert!(
        registered.len() >= 20,
        "the registry came back nearly empty, so this test would pass for the wrong reason: \
         {registered:?}"
    );
}

/// **`Safe` is a grant, so every name in it has to be one the tools themselves
/// call harmless.**
///
/// Two bits, both read off the registry rather than remembered here:
/// `read_only`, which is "can this change local state", and `reaches_network`,
/// which is the separate axis `tools/web` records — a tool can be read-only and
/// still be the channel a prompt-injected page exfiltrates through, and
/// pre-approving that is not what `Safe` means to anybody who chose it.
#[test]
fn every_read_only_name_really_is_read_only() {
    use emma_tool_api::Tool;
    use std::sync::Arc;

    let mut metas: Vec<(&'static str, bool, bool)> = Vec::new();
    let mut take = |tools: Vec<Arc<dyn Tool>>| {
        for t in tools {
            let m = t.meta();
            metas.push((t.name(), m.read_only, m.reaches_network));
        }
    };
    let (fs, _tracker) = emma_tools_fs::fs_tools();
    take(fs);
    take(emma_tools_tasks::task_tools());
    let (lsp, _pool) = emma_tools_lsp::lsp_tools();
    take(lsp);

    for name in READ_ONLY_TOOLS {
        let (_, read_only, network) = metas
            .iter()
            .find(|(n, _, _)| *n == name)
            .copied()
            .unwrap_or_else(|| panic!("{name} is pre-approved by Safe and is not registered"));
        assert!(read_only, "{name} is pre-approved by Safe and can write");
        assert!(
            !network,
            "{name} is pre-approved by Safe and reaches the network"
        );
    }
    // And Safe is a subset of what Deny All knows about, or the two presets
    // disagree about what the tool surface is.
    for name in READ_ONLY_TOOLS {
        assert!(ALL_TOOLS.contains(&name), "{name} is not in ALL_TOOLS");
    }
}

// endregion: Next-run policy

/// The summary names a preset only when the file matches one exactly. Every
/// other file gets what is actually in it, because a summary that answered
/// "Safe" for a hand-edited file would read as a receipt for a write nobody
/// made.
#[test]
fn the_summary_names_a_preset_only_when_the_file_is_one() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.local.json");

    // Absent: not an error, and the sentence says what that means.
    let missing = policy_summary(&file);
    assert!(missing.contains("does not exist"), "{missing}");
    assert!(missing.contains("every gated call asks"), "{missing}");

    write_policy(&file, Policy::Safe).unwrap();
    let safe = policy_summary(&file);
    assert!(safe.starts_with("Safe:"), "{safe}");
    assert!(safe.contains("Read"), "{safe}");
    assert!(safe.contains("denied: nothing"), "{safe}");

    write_policy(&file, Policy::DenyAll).unwrap();
    let deny = policy_summary(&file);
    assert!(deny.starts_with("Deny All:"), "{deny}");
    assert!(deny.contains("pre-approved: nothing"), "{deny}");

    write_policy(&file, Policy::Manual).unwrap();
    let manual = policy_summary(&file);
    assert!(manual.starts_with("Manual:"), "{manual}");
}

/// A file somebody edited by hand is reported, not classified, and the rules
/// carrying a specifier are counted apart: `write_policy` leaves those alone,
/// so a summary that folded them into its own total would be claiming
/// authorship of somebody else's decision.
#[test]
fn a_hand_edited_file_is_described_rather_than_given_a_presets_name() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.local.json");
    write_policy(&file, Policy::Safe).unwrap();
    // One bare grant no preset produces, and one the operator wrote.
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let allow = doc
        .pointer_mut("/permissions/allow")
        .unwrap()
        .as_array_mut()
        .unwrap();
    allow.push(serde_json::json!("Bash"));
    allow.push(serde_json::json!("WebFetch(domain:docs.rs)"));
    std::fs::write(&file, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    let out = policy_summary(&file);
    assert!(out.starts_with("No preset describes this file"), "{out}");
    assert!(out.contains("Bash"), "{out}");
    assert!(
        out.contains("1 rule(s) name a specifier"),
        "the operator's own rule is counted apart: {out}"
    );
    assert!(
        !out.contains("WebFetch(domain:docs.rs)"),
        "a specifier is counted, not listed as a tool: {out}"
    );

    // Damaged: said, and nothing is claimed about it.
    std::fs::write(&file, "{not json").unwrap();
    let broken = policy_summary(&file);
    assert!(broken.contains("not valid JSON"), "{broken}");
    assert!(
        !broken.contains("pre-approved"),
        "nothing may be claimed about a file that did not parse: {broken}"
    );
}
