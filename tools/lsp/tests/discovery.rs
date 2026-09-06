//! Which binary, for which language, and every refusal on the way there.
//!
//! All of it against an injected [`Env`], so none of it depends on what is
//! installed. That is not a convenience: the machine this suite was written on
//! had no language server of any kind on `PATH`, rust-analyzer included, so a
//! discovery suite that consulted the real environment would assert nothing at
//! all there and something different on every other box.
//!
//! The cases that earn their place are the ones where a plausible
//! implementation gets it wrong: a rustup proxy that exits 0 and is not a
//! language server, a bicep assembly with no `dotnet` to run it, and a `.yml`
//! that belongs to Docker rather than to Ansible.
//!
//! # Why every comparison goes through [`norm`]
//!
//! The fixtures are unix-spelled and the injected `windows` flag is what decides
//! behaviour, but `PathBuf::join` uses the **host's** separator. So on Windows
//! `PathBuf::from("/usr/bin").join("rust-analyzer")` prints
//! `/usr/bin\rust-analyzer`, and a test comparing that against a literal
//! `"/usr/bin/rust-analyzer"` fails for a reason that has nothing to do with the
//! thing it is checking. This suite had never run on Windows before the port and
//! that is exactly what happened. `PathBuf` equality is already separator-blind
//! on Windows; the string comparisons are the ones that need the helper, and
//! they all use it — including the `exists` fixture, which is a string set.
//!
//! This is the same class of bug `tools/fs/src/bash.rs` fixed in its own
//! resolution tests, and the same fix: the injected flag decides behaviour, the
//! host decides the spelling.

use std::path::{Path, PathBuf};

use emma_tools_lsp::lang::{self, Language};
use emma_tools_lsp::server::{self, Env, Probe, Source};

fn language(key: &str) -> &'static Language {
    lang::by_key(key).expect("a language in the table")
}

/// Separator- and case-blind, for every comparison a host separator could
/// otherwise decide. See the module doc.
fn norm(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

fn norm_path(path: &Path) -> String {
    norm(&path.to_string_lossy())
}

/// An environment with a fixed `PATH` and four editor extension directories.
fn env<'a>(exists: &'a dyn Fn(&Path) -> bool, probe: &'a dyn Fn(&Path, &str) -> Probe) -> Env<'a> {
    Env {
        windows: false,
        path_dirs: vec![PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin")],
        extension_dirs: vec![
            PathBuf::from("/home/u/.vscode/extensions/hashicorp.terraform-2.40.0"),
            PathBuf::from("/home/u/.vscode/extensions/ms-azuretools.vscode-bicep-0.46.1"),
            PathBuf::from("/home/u/.vscode/extensions/redhat.ansible-26.8.2"),
            PathBuf::from("/home/u/.vscode/extensions/redhat.ansible-26.6.0"),
        ],
        exists,
        probe,
        temp_dir: PathBuf::from("/tmp"),
    }
}

fn only(paths: &'static [&'static str]) -> impl Fn(&Path) -> bool {
    move |p: &Path| paths.iter().any(|f| norm(f) == norm_path(p))
}

fn prints(line: &'static str) -> impl Fn(&Path, &str) -> Probe {
    move |_: &Path, _: &str| Probe::Line(line.to_string())
}

/// Asserts a path-shaped string, whichever separator the host used to build it.
#[track_caller]
fn same(actual: &str, expected: &str) {
    assert_eq!(norm(actual), norm(expected));
}

// region: The happy paths
// ---------------------------------------------------------------------------

/// A binary on `PATH` that identifies itself is chosen, and the command line is
/// the binary itself.
#[test]
fn a_probed_binary_on_path_is_chosen() {
    let exists = only(&["/usr/local/bin/rust-analyzer"]);
    let probe = prints("rust-analyzer 1.2.3");
    let found = server::resolve_with(language("rust"), None, &env(&exists, &probe))
        .expect("rust-analyzer is right there");
    assert_eq!(found.program, PathBuf::from("/usr/local/bin/rust-analyzer"));
    assert!(found.args.is_empty());
    assert_eq!(found.source, Source::Path);
    assert_eq!(found.version, "rust-analyzer 1.2.3");
    assert_eq!(found.language.key, "rust");
}

/// `terraform-ls` prints a bare version with no name in it, so the expectation
/// for it is "looks like a version" rather than a prefix. Measured on the macOS
/// box: `0.39.0`.
#[test]
fn terraform_ls_is_accepted_on_a_bare_version_and_gets_its_serve_argument() {
    let exists = only(&["/home/u/.vscode/extensions/hashicorp.terraform-2.40.0/bin/terraform-ls"]);
    let probe = prints("0.39.0");
    let found = server::resolve_with(language("terraform"), None, &env(&exists, &probe))
        .expect("the extension carries one");
    assert_eq!(found.source, Source::VsCodeExtension);
    assert_eq!(found.args, vec!["serve".to_string()]);
    assert_eq!(found.version, "0.39.0");
}

/// A bundled JavaScript server is run by `node`, with the script as the first
/// argument, and the newest extension directory wins.
#[test]
fn a_bundled_script_is_launched_by_node_and_the_newest_extension_wins() {
    let exists = only(&[
        "/usr/bin/node",
        "/home/u/.vscode/extensions/redhat.ansible-26.8.2/packages/ansible-language-server/dist/server.js",
        "/home/u/.vscode/extensions/redhat.ansible-26.6.0/packages/ansible-language-server/dist/server.js",
    ]);
    let probe = prints("unused");
    let found = server::resolve_with(language("ansible"), None, &env(&exists, &probe))
        .expect("the extension carries one");
    assert_eq!(found.program, PathBuf::from("/usr/bin/node"));
    assert_eq!(found.args.len(), 2, "{:?}", found.args);
    same(
        &found.args[0],
        "/home/u/.vscode/extensions/redhat.ansible-26.8.2/packages/ansible-language-server/dist/server.js",
    );
    assert_eq!(found.args[1], "--stdio");
    // Nothing was probed, and the result says so rather than inventing a
    // version.
    assert_eq!(found.version, server::UNPROBED);
}

/// PowerShell Editor Services is a script, not a server, and the parameter set
/// it is started with is the one declared in the script itself.
#[test]
fn powershell_editor_services_is_started_through_pwsh_with_stdio() {
    let exists = only(&[
        "/usr/bin/pwsh",
        "/home/u/.vscode/extensions/ms-vscode.powershell-2025.4.0/modules/PowerShellEditorServices/Start-EditorServices.ps1",
    ]);
    let probe = prints("unused");
    let mut e = env(&exists, &probe);
    e.extension_dirs.push(PathBuf::from(
        "/home/u/.vscode/extensions/ms-vscode.powershell-2025.4.0",
    ));
    let found = server::resolve_with(language("powershell"), None, &e).expect("bundled");
    assert_eq!(found.program, PathBuf::from("/usr/bin/pwsh"));
    assert!(
        found.args.contains(&"-Stdio".to_string()),
        "{:?}",
        found.args
    );
    assert!(found.args.contains(&"-File".to_string()));
    // `BundledModulesPath` is the script's grandparent, which is the `modules`
    // directory. Getting this wrong is how the server starts and then cannot
    // find itself.
    let i = found
        .args
        .iter()
        .position(|a| a == "-BundledModulesPath")
        .expect("the parameter is passed");
    same(
        &found.args[i + 1],
        "/home/u/.vscode/extensions/ms-vscode.powershell-2025.4.0/modules",
    );
}

/// **The `windows: true` half, which nothing else here reaches.** The flag
/// changes exactly one thing — the names a candidate may be spelled with — and
/// it changes it in two places that are easy to fix separately and forget:
/// `PATH` lookup, and an extensionless entry point inside an extension. The
/// Windows box carries `rust-lang.rust-analyzer-0.3.3033-win32-x64`, whose
/// server is `rust-analyzer.exe` while the table spells the bare name.
///
/// **The fixture spells its Windows paths with forward slashes, and that is
/// not cosmetic.** `Path` is a platform type: on unix a backslash is an
/// ordinary character, so `file_name()` of a backslash-separated string is
/// the whole string, the extension-directory prefix match never fires, and
/// this test fails on macOS for a reason that has nothing to do with what it
/// is testing. It did, on 2026-09-06, the first time the suite ran there.
/// Forward slashes parse as separators on both platforms, so the test now
/// exercises `Env::windows` rather than the host it happens to run on, which
/// is the whole point of that flag being a field.
#[test]
fn on_windows_a_bare_name_is_also_tried_with_an_exe_suffix() {
    const EXT_DIR: &str =
        "C:/Users/a/.vscode/extensions/rust-lang.rust-analyzer-0.3.3033-win32-x64";
    let exists = only(&[
        "C:/Users/a/.vscode/extensions/rust-lang.rust-analyzer-0.3.3033-win32-x64/server/rust-analyzer.exe",
    ]);
    let probe = prints("rust-analyzer 1.94.1");
    let e = Env {
        windows: true,
        path_dirs: vec![PathBuf::from("C:/Windows/System32")],
        extension_dirs: vec![PathBuf::from(EXT_DIR)],
        exists: &exists,
        probe: &probe,
        temp_dir: PathBuf::from("C:/Temp"),
    };
    let found = server::resolve_with(language("rust"), None, &e).expect("the extension server");
    assert_eq!(found.source, Source::VsCodeExtension);
    same(
        &found.entry.to_string_lossy(),
        &format!("{EXT_DIR}/server/rust-analyzer.exe"),
    );

    // And the same environment with `windows: false` finds nothing, which is
    // what makes the assertion above about the flag rather than about the
    // fixture.
    let unix = Env {
        windows: false,
        path_dirs: vec![PathBuf::from("C:/Windows/System32")],
        extension_dirs: vec![PathBuf::from(EXT_DIR)],
        exists: &exists,
        probe: &probe,
        temp_dir: PathBuf::from("C:/Temp"),
    };
    assert!(
        server::resolve_with(language("rust"), None, &unix).is_err(),
        "without the windows flag the bare name must not match an .exe"
    );
}

// endregion: The happy paths

// region: The refusals
// ---------------------------------------------------------------------------

/// **The rustup proxy, which is the reason probing exists.** It is a real file,
/// it is first on `PATH`, and it exits 0. Chosen by existence it would produce
/// a handshake that never answers, ninety seconds later.
#[test]
fn a_binary_that_does_not_identify_itself_is_rejected_and_explained() {
    let exists = only(&["/usr/local/bin/rust-analyzer"]);
    let probe = |_: &Path, _: &str| Probe::Line("error: Unknown binary 'rust-analyzer'".into());
    let err = server::resolve_with(language("rust"), None, &env(&exists, &probe))
        .expect_err("a proxy is not a language server");
    let detail = err.detail();
    // The rejected candidate is named, with what it said. A refusal that only
    // said "not found" would be actively misleading to someone looking at a
    // `rust-analyzer` on their PATH.
    assert!(
        norm(detail).contains(&norm("/usr/local/bin/rust-analyzer")),
        "{detail}"
    );
    assert!(detail.contains("Unknown binary"), "{detail}");
    assert!(detail.contains("rustup component add"), "{detail}");
}

/// **A missing launcher is not a missing server.** The bicep assembly is right
/// there; what is absent is `dotnet`. Telling the user to install the extension
/// they already have would send them the wrong way.
#[test]
fn a_missing_launcher_is_named_rather_than_the_server() {
    let exists = only(&[
        "/home/u/.vscode/extensions/ms-azuretools.vscode-bicep-0.46.1/bicepLanguageServer/Bicep.LangServer.dll",
    ]);
    let probe = prints("unused");
    let err = server::resolve_with(language("bicep"), None, &env(&exists, &probe))
        .expect_err("no dotnet");
    let detail = err.detail();
    assert!(detail.contains("needs `dotnet` to run it"), "{detail}");
    assert!(detail.contains("Bicep.LangServer.dll"), "{detail}");
    assert!(detail.contains("dotnet.microsoft.com"), "{detail}");
}

/// **No refusal may send a user to a package manager their machine has not
/// got.** The remedy is a URL that works everywhere, and the package-manager
/// suggestion beside it is the host's own.
///
/// The fork this was ported from said `brew install node` on every platform. A
/// remedy that cannot be run is worse than no remedy: it reads like help, so it
/// is followed, and it fails in a way that looks like the user's fault.
#[test]
fn a_launcher_remedy_names_a_package_manager_this_host_actually_has() {
    let exists = only(&[
        "/home/u/.vscode/extensions/ms-azuretools.vscode-bicep-0.46.1/bicepLanguageServer/Bicep.LangServer.dll",
    ]);
    let probe = prints("unused");
    let detail = server::resolve_with(language("bicep"), None, &env(&exists, &probe))
        .expect_err("no dotnet")
        .detail()
        .to_string();
    assert!(
        detail.contains("https://dotnet.microsoft.com/download"),
        "the platform-independent remedy must always be there: {detail}"
    );
    if !cfg!(target_os = "macos") {
        assert!(
            !detail.contains("brew "),
            "a non-macOS host was told to run brew: {detail}"
        );
    }
    if cfg!(windows) {
        assert!(detail.contains("winget"), "{detail}");
    }
}

/// Nothing anywhere: the refusal names what was looked for, where, how to get
/// it, and the override.
#[test]
fn nothing_installed_is_a_refusal_that_can_be_acted_on() {
    let exists = only(&[]);
    let probe = prints("unused");
    let err = server::resolve_with(language("python"), None, &env(&exists, &probe))
        .expect_err("nothing is installed");
    let detail = err.detail();
    assert!(detail.contains("pyright-langserver"), "{detail}");
    assert!(detail.contains("/usr/local/bin"), "{detail}");
    assert!(detail.contains("npm install -g pyright"), "{detail}");
    assert!(detail.contains("EMMA_LSP_SERVER_PYTHON"), "{detail}");
    // And the promise, in the message, because this is the moment a fall back
    // to grep would be most tempting and least detectable.
    assert!(detail.contains("text search"), "{detail}");
}

/// An override that names nothing is refused rather than quietly ignored.
#[test]
fn an_override_that_names_nothing_never_falls_back() {
    let exists = only(&["/usr/local/bin/rust-analyzer"]);
    let probe = prints("rust-analyzer 1.2.3");
    let err = server::resolve_with(
        language("rust"),
        Some("/opt/nowhere/ra"),
        &env(&exists, &probe),
    )
    .expect_err("the override is not there");
    let detail = err.detail().to_string();
    assert!(detail.contains("/opt/nowhere/ra"), "{detail}");
    // The perfectly good server on PATH was not silently used instead.
    assert!(
        !norm(&detail).contains(&norm("/usr/local/bin/rust-analyzer")),
        "{detail}"
    );
}

/// Each language has its own override variable, and rust keeps the original
/// name so that everything written about `EMMA_LSP_SERVER` stays true.
#[test]
fn the_override_variable_is_per_language() {
    assert_eq!(
        server::override_env(language("rust")),
        "EMMA_LSP_SERVER_RUST"
    );
    assert_eq!(
        server::override_env(language("terraform")),
        "EMMA_LSP_SERVER_TERRAFORM"
    );
    assert_eq!(server::OVERRIDE_ENV, "EMMA_LSP_SERVER");
}

/// The refusal for a file no language claims lists the extensions that are
/// served, and still promises not to grep instead.
#[test]
fn a_file_no_language_claims_is_refused_with_the_list_and_the_promise() {
    let err = server::unsupported_language("docs/plan.md");
    assert_eq!(err.kind(), "tool_unavailable");
    let detail = err.detail();
    assert!(detail.contains("docs/plan.md"), "{detail}");
    assert!(detail.contains("rs, sh"), "{detail}");
    assert!(detail.contains("text search"), "{detail}");
}

// endregion: The refusals

// region: The table
// ---------------------------------------------------------------------------

/// The seven the owner asked for, all present, and no extension claimed twice.
///
/// The second half is the one that would bite: two languages claiming `.yml`
/// makes which server answers depend on table order, which is not a decision
/// anybody made.
#[test]
fn the_table_covers_the_seven_and_claims_nothing_twice() {
    let keys = lang::keys();
    for wanted in [
        "bash",
        "powershell",
        "terraform",
        "bicep",
        "ansible",
        "python",
        "rust",
    ] {
        assert!(keys.contains(&wanted), "{wanted} is missing from the table");
    }
    let mut seen: Vec<&str> = Vec::new();
    for l in lang::LANGUAGES {
        for e in l.extensions {
            assert!(!seen.contains(e), "{e} is claimed by two languages");
            seen.push(e);
        }
        // Every entry must be startable and every refusal actionable.
        assert!(!l.candidates.is_empty(), "{} has no candidates", l.key);
        assert!(!l.install.is_empty(), "{} has no install hint", l.key);
        serde_json::from_str::<serde_json::Value>(l.init_options)
            .unwrap_or_else(|e| panic!("{}'s init_options is not JSON: {e}", l.key));
    }
}

/// The default set leaves out exactly the languages whose servers may reach the
/// network, because these tools declare that they do not.
#[test]
fn the_default_set_is_the_languages_that_stay_local() {
    for l in lang::LANGUAGES {
        let on = lang::DEFAULT_ENABLED.contains(&l.key);
        assert_eq!(
            on, !l.network,
            "{} is {} by default and network is {}",
            l.key, on, l.network
        );
    }
}

/// **YAML is not Ansible until the tree says so.** Handing every `.yml` to a
/// playbook server means answering about compose files and workflows with a
/// tool that believes they are playbooks, and the answer would look real.
#[test]
fn yaml_is_ansible_only_with_corroboration() {
    let root = Path::new("/w");
    assert!(lang::for_path(root, Path::new("/w/docker-compose.yml")).is_none());
    assert!(lang::for_path(root, Path::new("/w/.github/workflows/ci.yml")).is_none());
    assert_eq!(
        lang::for_path(root, Path::new("/w/playbooks/site.yml")).map(|l| l.key),
        Some("ansible")
    );
    assert_eq!(
        lang::for_path(root, Path::new("/w/roles/web/tasks/main.yml")).map(|l| l.key),
        Some("ansible")
    );
    // And the other corroboration, which needs a real directory.
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(!lang::looks_like_ansible(
        dir.path(),
        &dir.path().join("compose.yml")
    ));
    std::fs::write(dir.path().join("ansible.cfg"), "[defaults]\n").expect("write");
    assert!(lang::looks_like_ansible(
        dir.path(),
        &dir.path().join("compose.yml")
    ));
}

/// Extensions map to the language a reader would expect, and nothing else does.
#[test]
fn extensions_map_to_the_language_they_look_like() {
    let root = Path::new("/w");
    for (relative, expected) in [
        ("a.rs", Some("rust")),
        ("a.sh", Some("bash")),
        ("a.bash", Some("bash")),
        ("a.ps1", Some("powershell")),
        ("a.psm1", Some("powershell")),
        ("a.tf", Some("terraform")),
        ("a.tfvars", Some("terraform")),
        ("a.bicep", Some("bicep")),
        ("a.bicepparam", Some("bicep")),
        ("a.py", Some("python")),
        ("a.pyi", Some("python")),
        ("a.md", None),
        ("a.json", None),
        ("Makefile", None),
    ] {
        assert_eq!(
            lang::for_path(root, &root.join(relative)).map(|l| l.key),
            expected,
            "{relative}"
        );
    }
}

// endregion: The table

// region: Presence, the no-spawn question
// ---------------------------------------------------------------------------
// `server::presence` answers a weaker question than `resolve_with` so that a
// screen painting on the input thread never spawns a process. These cases pin
// the difference, including the one where it is weaker than the truth.
// ---------------------------------------------------------------------------

/// A binary on `PATH` is `Found`, and nothing was probed to say so.
#[test]
fn presence_finds_a_binary_on_path_without_probing() {
    let exists = only(&["/usr/local/bin/rust-analyzer"]);
    let probe = |_: &Path, _: &str| -> Probe { panic!("presence must not probe") };
    let seen = server::presence_with(language("rust"), None, &env(&exists, &probe));
    assert_eq!(
        seen,
        server::Presence::Found {
            entry: PathBuf::from("/usr/local/bin/rust-analyzer"),
            source: Source::Path,
        }
    );
}

/// An entry point inside a VS Code extension counts, and says where it is from.
#[test]
fn presence_finds_a_bundled_entry_point() {
    let exists = only(&["/home/u/.vscode/extensions/hashicorp.terraform-2.40.0/bin/terraform-ls"]);
    let probe = |_: &Path, _: &str| -> Probe { panic!("presence must not probe") };
    let seen = server::presence_with(language("terraform"), None, &env(&exists, &probe));
    assert!(matches!(
        seen,
        server::Presence::Found {
            source: Source::VsCodeExtension,
            ..
        }
    ));
}

/// The bicep case: the assembly is there and `dotnet` is not, so the answer
/// names the launcher rather than the server.
#[test]
fn presence_names_the_missing_launcher() {
    let exists = only(&[
        "/home/u/.vscode/extensions/ms-azuretools.vscode-bicep-0.46.1/bicepLanguageServer/Bicep.LangServer.dll",
    ]);
    let probe = |_: &Path, _: &str| -> Probe { panic!("presence must not probe") };
    let seen = server::presence_with(language("bicep"), None, &env(&exists, &probe));
    assert!(
        matches!(&seen, server::Presence::NeedsLauncher { needs, .. } if *needs == "dotnet"),
        "expected a missing dotnet, got {seen:?}"
    );
}

/// A launcher that is present promotes the same entry point to `Found`.
#[test]
fn presence_with_the_launcher_present_is_found() {
    let exists = only(&[
        "/home/u/.vscode/extensions/redhat.ansible-26.8.2/packages/ansible-language-server/dist/server.js",
        "/usr/bin/node",
    ]);
    let probe = |_: &Path, _: &str| -> Probe { panic!("presence must not probe") };
    let seen = server::presence_with(language("ansible"), None, &env(&exists, &probe));
    assert!(
        matches!(seen, server::Presence::Found { .. }),
        "got {seen:?}"
    );
}

/// Nothing anywhere is `Absent`, for every language in the table.
#[test]
fn presence_of_nothing_is_absent_for_every_language() {
    let exists = only(&[]);
    let probe = |_: &Path, _: &str| -> Probe { panic!("presence must not probe") };
    for key in lang::keys() {
        assert_eq!(
            server::presence_with(language(key), None, &env(&exists, &probe)),
            server::Presence::Absent,
            "{key} should be absent in an empty environment"
        );
    }
}

/// The honest limit, pinned rather than hidden: the rustup proxy is a real file
/// and `presence` calls it `Found`, while `resolve_with` refuses it. Anything
/// built on `presence` may say a file is on disk and must never say a server
/// works.
#[test]
fn presence_is_weaker_than_resolve_for_the_rustup_proxy() {
    let exists = only(&["/usr/local/bin/rust-analyzer"]);
    let refuses = prints("error: Unknown binary 'rust-analyzer.exe'");
    let no_probe = |_: &Path, _: &str| -> Probe { panic!("presence must not probe") };
    assert!(server::resolve_with(language("rust"), None, &env(&exists, &refuses)).is_err());
    assert!(matches!(
        server::presence_with(language("rust"), None, &env(&exists, &no_probe)),
        server::Presence::Found { .. }
    ));
}

/// An override that names a real path is `Found` and says it came from the
/// override; one that names nothing is `Absent`, never a fallback.
#[test]
fn presence_honours_the_override_and_never_falls_back() {
    let exists = only(&["/opt/ra/rust-analyzer", "/usr/local/bin/rust-analyzer"]);
    let probe = |_: &Path, _: &str| -> Probe { panic!("presence must not probe") };
    assert_eq!(
        server::presence_with(
            language("rust"),
            Some("/opt/ra/rust-analyzer"),
            &env(&exists, &probe)
        ),
        server::Presence::Found {
            entry: PathBuf::from("/opt/ra/rust-analyzer"),
            source: Source::Override,
        }
    );
    assert_eq!(
        server::presence_with(
            language("rust"),
            Some("/opt/ra/absent"),
            &env(&exists, &probe)
        ),
        server::Presence::Absent
    );
}

// endregion: Presence, the no-spawn question
