//! Servers declared in `settings.json`, and the refusal for each way of
//! declaring one wrongly.
//!
//! **Two halves, and they are kept apart on purpose.** `lang::plan_user_servers`
//! is pure: it takes JSON and gives back a table and a list of sentences,
//! touching no global. Everything about validation and precedence is tested
//! through it, many times over, in one process.
//!
//! `lang::install_user_servers` sets the process-wide table, which can happen
//! **once**. So exactly one entry is installed here — [`installed`] — and every
//! test that needs `by_key`, `for_path` or a running server goes through it,
//! including the certification against a real rust-analyzer at the bottom.
//! Tests in one binary run concurrently, which is why no test in this file
//! reads the table without calling that function first: the alternative is a
//! test that installs after another has already frozen it, failing on
//! scheduling.

mod support;

use std::sync::OnceLock;

use emma_tool_api::Tool;
use emma_tools_lsp::lang::{self, Language, Plan};
use emma_tools_lsp::server::{self, Presence};
use emma_tools_lsp::{DocumentSymbols, Pool};
use serde_json::{json, Value};
use std::sync::Arc;
use support::Sandbox;

// region: The validator, without touching the table
// ---------------------------------------------------------------------------
// The validator, without touching the table
// ---------------------------------------------------------------------------

/// One entry, planned. The helper exists so each case reads as "this JSON, that
/// sentence".
fn plan_one(key: &str, value: Value) -> Plan {
    lang::plan_user_servers([(key.to_string(), value)])
}

fn only_refusal(key: &str, value: Value) -> String {
    let plan = plan_one(key, value);
    assert!(
        plan.declared.is_empty(),
        "the entry was accepted: {:?}",
        plan.declared.iter().map(|l| l.key).collect::<Vec<_>>()
    );
    assert_eq!(plan.refusals.len(), 1, "{:?}", plan.refusals);
    // …and the table is untouched: one bad entry never costs the built-ins.
    assert_eq!(plan.table.len(), lang::LANGUAGES.len());
    plan.refusals[0].clone()
}

/// The shape from the documentation, accepted: a new key, after the seven, with
/// its own extension.
///
/// **If this breaks:** `lsp.servers` does nothing, which is the whole feature.
#[test]
fn a_declared_server_becomes_a_language_after_the_built_in_seven() {
    let plan = plan_one(
        "go",
        json!({
            "label": "Go",
            "language_id": "go",
            "extensions": ["go"],
            "command": "gopls",
            "args": [],
            "init_options": {},
            "install": "go install golang.org/x/tools/gopls@latest",
        }),
    );
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    assert_eq!(plan.declared.len(), 1);
    let go = plan.declared[0];
    assert_eq!(go.key, "go");
    assert_eq!(go.label, "Go");
    assert_eq!(go.language_id, "go");
    assert_eq!(go.extensions, ["go"]);
    assert_eq!(go.init_options, "{}");
    assert!(go.user_declared);

    // Appended, never prepended. First-match-wins in `for_path` is why: an
    // entry ahead of the table could take `.rs` from rust-analyzer by being
    // read first.
    assert_eq!(plan.table.len(), lang::LANGUAGES.len() + 1);
    assert_eq!(plan.table.last().expect("appended").key, "go");
    assert_eq!(plan.table[0].key, lang::LANGUAGES[0].key);

    // One candidate, direct, unprobed, with the command as its name and the
    // arguments kept separate.
    let candidate = &go.candidates[0];
    assert_eq!(candidate.bin, "gopls");
    assert_eq!(candidate.launcher, lang::Launcher::Direct);
    assert_eq!(candidate.identity, lang::Identity::LauncherOnly);
}

/// Emma cannot assert `reaches_network: false` about a stranger's program, so a
/// declared server carries the opposite bit and is never in the default set.
///
/// **If this breaks:** a settings file turns on a program Emma has never run,
/// without anybody saying so, under seven tools that declare they touch no
/// network.
#[test]
fn a_declared_server_is_network_true_and_never_on_by_default() {
    let plan = plan_one("go", json!({ "extensions": ["go"], "command": "gopls" }));
    let go = plan.declared[0];
    assert!(go.network, "a declared server claims the network");
    assert!(
        !lang::DEFAULT_ENABLED.contains(&go.key),
        "a declared key must never reach the default enabled set"
    );
    // And the refusal for it says which of the two reasons applies.
    let refusal = server::disabled_language(go).to_string();
    assert!(refusal.contains("lsp.servers"), "{refusal}");
    assert!(refusal.contains("did not choose"), "{refusal}");
    assert!(refusal.contains("lsp.enabled"), "{refusal}");
}

/// Every result carries it, the way the built-in caveats do.
#[test]
fn a_declared_server_carries_a_caveat_naming_it_as_yours() {
    let plan = plan_one(
        "go",
        json!({ "label": "Go", "extensions": ["go"], "command": "gopls" }),
    );
    let caveat = plan.declared[0].caveat;
    assert!(caveat.contains("settings.json"), "{caveat}");
    assert!(caveat.contains("never driven it to an answer"), "{caveat}");
    assert!(caveat.contains("reaches_network"), "{caveat}");
}

/// A key that names a built-in replaces it **in place**, entirely.
///
/// **If this breaks:** either an override is a field-wise merge — a shape
/// nobody has ever seen, which `crates/harness` refuses for the same reason —
/// or it moves behind six other entries and stops answering for `.rs`.
#[test]
fn a_key_that_names_a_built_in_replaces_it_in_place_and_entirely() {
    let plan = plan_one(
        "rust",
        json!({ "extensions": ["rs"], "command": "my-analyzer" }),
    );
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    assert_eq!(
        plan.table.len(),
        lang::LANGUAGES.len(),
        "replaced, not added"
    );
    let rust = plan.table[0];
    assert_eq!(rust.key, "rust");
    assert_eq!(rust.candidates[0].bin, "my-analyzer");
    // Entirely: rust's own initialization options, install hint and caveat do
    // not survive into an entry that did not ask for them. The `--offline`
    // switch in particular is what made `reaches_network: false` true, and it
    // is not true of somebody else's binary.
    assert_eq!(rust.init_options, "{}");
    assert!(rust.network);
    assert!(!rust.caveat.contains("Proc macros"), "{}", rust.caveat);
}

/// The one case an override has to allow: taking `.rs` from the entry it
/// replaces.
#[test]
fn an_override_may_keep_the_extension_of_the_entry_it_replaces() {
    let plan = plan_one("rust", json!({ "extensions": ["rs"], "command": "x" }));
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
}

// region: The five refusals
// ---------------------------------------------------------------------------
// The five refusals
//
// Each is a sentence somebody reads at startup with their settings file open,
// so each is asserted for the fact that makes it actionable rather than for its
// exact wording.
// ---------------------------------------------------------------------------

/// A `command` that is not on `PATH` is **not** refused here. It is
/// `server::nothing_found` at call time, which is the refusal that already
/// names where it looked, and the startup line says so without spawning.
///
/// **If this breaks:** either a typo'd command is a silent nothing, or Emma
/// grew a second vocabulary for "no server".
#[test]
fn a_command_that_is_not_on_path_is_the_existing_refusal_and_a_startup_line() {
    let plan = plan_one(
        "go",
        json!({ "label": "Go", "extensions": ["go"], "command": "no-such-server-anywhere" }),
    );
    assert!(plan.refusals.is_empty(), "declaring it is not the error");
    let go = plan.declared[0];

    let Err(refusal) = server::resolve(go) else {
        panic!("a server that does not exist resolved");
    };
    let refusal = refusal.to_string();
    assert!(
        refusal.contains("no Go language server is available"),
        "{refusal}"
    );
    assert!(refusal.contains("no-such-server-anywhere"), "{refusal}");
    // The install hint Emma writes when the entry does not carry one names the
    // file and the key, because "install it" is not advice a stranger's server
    // can be given.
    assert!(refusal.contains("lsp.servers.\"go\""), "{refusal}");

    // And the startup disclosure, which is a `stat` and never a spawn.
    let line = server::declared_line(go, true);
    assert!(line.contains("no-such-server-anywhere"), "{line}");
    assert!(line.contains(".go"), "{line}");
    assert!(line.contains("refused"), "{line}");
}

/// **If this breaks:** an entry whose extension is already spoken for is
/// declared, never reached by `for_path`, and looks like a server that answers
/// nothing.
#[test]
fn an_extension_another_language_claims_is_refused_by_name() {
    let refusal = only_refusal("mine", json!({ "extensions": ["rs"], "command": "x" }));
    assert!(refusal.contains("\"rs\""), "{refusal}");
    assert!(refusal.contains("Rust"), "{refusal}");
    assert!(refusal.contains("would never be reached"), "{refusal}");
    // And it says what to do instead, which is the override.
    assert!(refusal.contains("\"rust\""), "{refusal}");
}

/// **If this breaks:** the value reaches the handshake, and `client.rs` panics
/// or fails one process later with a message about JSON.
#[test]
fn init_options_that_are_not_an_object_are_refused_before_a_process_exists() {
    let refusal = only_refusal(
        "go",
        json!({ "extensions": ["go"], "command": "gopls", "init_options": "{}" }),
    );
    assert!(refusal.contains("init_options"), "{refusal}");
    assert!(refusal.contains("a string"), "{refusal}");
    assert!(refusal.contains("initializationOptions"), "{refusal}");

    // An array is the same refusal, and an absent one is not a refusal at all.
    let refusal = only_refusal(
        "go",
        json!({ "extensions": ["go"], "command": "gopls", "init_options": [] }),
    );
    assert!(refusal.contains("an array"), "{refusal}");
    let plan = plan_one("go", json!({ "extensions": ["go"], "command": "gopls" }));
    assert_eq!(plan.declared[0].init_options, "{}");
}

/// A key that is neither a known language nor a declared one is
/// `Pool::unknown_keys` — kept and reported, never rejected. The rule was
/// already written for `lsp.enabled` and this change does not add a second one.
///
/// **If this breaks:** a settings file written by a newer build disables the
/// languages this one does know.
#[test]
fn a_key_that_is_neither_known_nor_declared_is_kept_and_reported() {
    // `unknown_keys` asks the table, so this test is one of the ones that has
    // to let the single installation happen first. See the module doc.
    let _ = installed();
    let pool = Pool::with_enabled(["rust".to_string(), "nosuchlanguage".to_string()]);
    assert_eq!(pool.unknown_keys(), ["nosuchlanguage"]);
}

/// **If this breaks:** `"arguments"` for `"args"` starts a server with none of
/// them and nothing anywhere says so.
#[test]
fn a_field_emma_does_not_know_is_refused_rather_than_ignored() {
    let refusal = only_refusal(
        "go",
        json!({ "extensions": ["go"], "command": "gopls", "arguments": ["--stdio"] }),
    );
    assert!(refusal.contains("\"arguments\""), "{refusal}");
    for field in lang::SERVER_FIELDS {
        assert!(refusal.contains(field), "{field} unlisted: {refusal}");
    }
}

/// `command` is one program or one path, and the arguments are their own array
/// — `usertools`'s ruling, and the reason it is not word-split here either.
///
/// **If this breaks:** a path with a space in it becomes two arguments, which
/// is the first half of running text through a shell.
#[test]
fn a_command_is_never_word_split_and_a_missing_one_is_refused() {
    let plan = plan_one(
        "go",
        json!({ "extensions": ["go"], "command": "C:\\Program Files\\gopls\\gopls.exe",
                "args": ["-mode", "stdio"] }),
    );
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    let candidate = &plan.declared[0].candidates[0];
    assert_eq!(candidate.bin, "C:\\Program Files\\gopls\\gopls.exe");
    assert_eq!(candidate.args, ["-mode", "stdio"]);

    let refusal = only_refusal("go", json!({ "extensions": ["go"] }));
    assert!(refusal.contains("no \"command\""), "{refusal}");
    assert!(refusal.contains("never a command line"), "{refusal}");
    assert!(refusal.contains("\"args\""), "{refusal}");
}

/// A `command` that is a path is that path, and no `PATH` directory is joined
/// to it.
///
/// **The `PATH` here is empty, and that is the point.** On Windows
/// `dir.join("C:\\x\\y.exe")` returns the absolute path, so a declared absolute
/// path resolves *by accident* even without the branch that handles it — as
/// long as there is at least one directory on `PATH` to join it onto. This test
/// removes the accident: with nothing on `PATH`, only a search that treats the
/// command as a location finds it.
///
/// **If this breaks:** a server named by absolute path is not found on a
/// machine with an unusual `PATH`, and the refusal names directories the user
/// never mentioned.
#[test]
fn a_command_that_is_a_path_is_found_with_nothing_on_path_at_all() {
    let plan = plan_one(
        "go",
        json!({ "extensions": ["go"], "command": "/opt/servers/gopls" }),
    );
    let go = plan.declared[0];
    let exists = |p: &std::path::Path| p == std::path::Path::new("/opt/servers/gopls");
    let probe = |_: &std::path::Path, _: &str| server::Probe::Not("never probed".into());
    let env = server::Env {
        windows: false,
        path_dirs: Vec::new(),
        extension_dirs: Vec::new(),
        exists: &exists,
        probe: &probe,
        temp_dir: std::path::PathBuf::from("/tmp"),
    };
    let found = server::resolve_with(go, None, &env).expect("the declared path is the server");
    assert_eq!(found.entry, std::path::PathBuf::from("/opt/servers/gopls"));
    // And the banner says where it came from. "found via PATH" would be a lie
    // about a program that was never looked up anywhere.
    assert_eq!(found.source, server::Source::Declared);
    assert!(
        found.banner().contains("the command in settings.json"),
        "{}",
        found.banner()
    );
}

/// The remaining shapes, each with the fact that makes its sentence usable.
#[test]
fn the_other_malformed_entries_each_say_which_part_is_wrong() {
    let refusal = only_refusal("go", json!("gopls"));
    assert!(refusal.contains("must be a JSON object"), "{refusal}");

    let refusal = only_refusal("go", json!({ "command": "gopls" }));
    assert!(refusal.contains("no extensions"), "{refusal}");

    let refusal = only_refusal("go", json!({ "command": "gopls", "extensions": ["go", 7] }));
    assert!(refusal.contains("a number"), "{refusal}");

    let refusal = only_refusal(
        "go",
        json!({ "command": "gopls", "extensions": ["go"], "args": "--stdio" }),
    );
    assert!(refusal.contains("array of strings"), "{refusal}");

    let refusal = only_refusal(
        "Go Lang",
        json!({ "command": "gopls", "extensions": ["go"] }),
    );
    assert!(refusal.contains("lowercase"), "{refusal}");
    assert!(refusal.contains("EMMA_LSP_SERVER"), "{refusal}");
}

/// A dot on an extension is the mistake everybody makes once, and it is a
/// tidy-up rather than a refusal — unlike an empty string, which claims
/// nothing.
#[test]
fn extensions_are_normalised_and_an_empty_one_is_refused() {
    let plan = plan_one(
        "go",
        json!({ "command": "gopls", "extensions": [".GO", "mod"] }),
    );
    assert_eq!(plan.declared[0].extensions, ["go", "mod"]);
    let refusal = only_refusal("go", json!({ "command": "gopls", "extensions": [""] }));
    assert!(refusal.contains("without the dot"), "{refusal}");
}

/// One bad entry costs that entry and nothing else.
#[test]
fn a_refused_entry_does_not_take_the_others_with_it() {
    let plan = lang::plan_user_servers([
        ("bad".to_string(), json!("not an object")),
        (
            "good".to_string(),
            json!({ "command": "x", "extensions": ["gd"] }),
        ),
    ]);
    assert_eq!(plan.refusals.len(), 1);
    assert_eq!(plan.declared.len(), 1);
    assert_eq!(plan.table.len(), lang::LANGUAGES.len() + 1);
}

// endregion: The five refusals

/// The `expect` in `client::handshake` was reachable from a configuration file
/// the moment `lsp.servers` existed. It is unreachable again — the validator
/// above refuses a non-object before anything is leaked — and it is still not a
/// panic, because the argument for panicking was that only this repository
/// could put a bad value there.
///
/// **If this breaks:** a malformed entry, arriving by any future route, takes
/// the whole process down instead of failing one tool call.
#[tokio::test(flavor = "multi_thread")]
async fn init_options_that_are_not_json_fail_the_handshake_rather_than_the_process() {
    // Leaked by hand rather than planned: this is the shape the validator
    // exists to make impossible, so there is no way to ask for it politely.
    let language: &'static Language = Box::leak(Box::new(Language {
        key: "broken",
        label: "Broken",
        language_id: "broken",
        extensions: &["brk"],
        candidates: &[],
        init_options: "{ not json",
        install: "",
        network: true,
        caveat: "",
        user_declared: true,
    }));
    let server = emma_tools_lsp::Server {
        program: std::path::PathBuf::from("/fake/server"),
        args: Vec::new(),
        entry: std::path::PathBuf::from("/fake/server"),
        version: "0.0.0-fake".into(),
        source: server::Source::Declared,
        language,
    };
    // No peer on the other end: the handshake must fail before it writes a
    // byte, which is the second half of the guarantee.
    let (ours, _theirs) = tokio::io::duplex(1024);
    let (read, write) = tokio::io::split(ours);
    let Err(error) =
        emma_tools_lsp::Client::connect(server, std::path::Path::new("."), read, write).await
    else {
        panic!("a language with unparseable init options started a server");
    };
    let error = error.to_string();
    assert!(error.contains("Broken"), "{error}");
    assert!(error.contains("not valid JSON"), "{error}");
}

// region: The one installation, and the real server
// ---------------------------------------------------------------------------
// The one installation, and the real server
//
// `install_user_servers` sets a `OnceLock`. Everything below shares this single
// call, which is also the certification: the declared entry points at whatever
// rust-analyzer this machine has, by absolute path, under a key and an
// extension that are not rust's.
// ---------------------------------------------------------------------------

/// The key and the extension used for the certification. Neither is rust's, so
/// an answer about `.rsx` can only have come through the declared entry.
const CERT_KEY: &str = "rustish";
const CERT_EXT: &str = "rsx";

/// The absolute path of this machine's rust-analyzer, or `None`.
fn rust_analyzer_path() -> Option<String> {
    let rust = lang::LANGUAGES
        .iter()
        .find(|l| l.key == "rust")
        .expect("rust is in the built-in table");
    match server::resolve(rust) {
        Ok(found) => Some(found.entry.display().to_string()),
        Err(e) => {
            eprintln!("SKIPPED: no rust-analyzer on this machine — {e}");
            None
        }
    }
}

/// Install the declared entry, once per process, and hand back the language it
/// became. `None` when there is no rust-analyzer to point it at — in which case
/// nothing is installed and the table stays the built-in seven.
fn installed() -> Option<&'static Language> {
    static ONCE: OnceLock<Option<&'static Language>> = OnceLock::new();
    *ONCE.get_or_init(|| {
        let command = rust_analyzer_path()?;
        let plan = lang::install_user_servers([(
            CERT_KEY.to_string(),
            json!({
                "label": "Rustish",
                // rust-analyzer is told the document is rust. The extension is
                // Emma's routing key and the languageId is the server's, and
                // the two are allowed to differ — which is the point of having
                // both fields.
                "language_id": "rust",
                "extensions": [CERT_EXT],
                "command": command,
                "init_options": serde_json::from_str::<Value>(lang::RUST_INIT)
                    .expect("RUST_INIT is valid JSON"),
            }),
        )]);
        assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
        Some(plan.declared[0])
    })
}

/// The table every other module reads is the one with the declared entry in it.
///
/// **If this breaks:** the entry validates and is then invisible to `for_path`,
/// `by_key` and every refusal — which is exactly the "settings file that
/// silently does nothing" this was built to avoid.
#[test]
fn the_installed_table_is_what_by_key_and_for_path_answer_from() {
    let Some(language) = installed() else { return };
    assert_eq!(lang::by_key(CERT_KEY).map(|l| l.key), Some(CERT_KEY));
    assert!(lang::keys().contains(&CERT_KEY));

    let root = std::path::Path::new("/project");
    let found = lang::for_path(root, &root.join(format!("a.{CERT_EXT}"))).expect("routed");
    assert_eq!(found.key, language.key);
    // …and rust still owns `.rs`.
    assert_eq!(
        lang::for_path(root, &root.join("a.rs")).map(|l| l.key),
        Some("rust")
    );

    // The refusal for a file nothing claims lists the declared extension, so it
    // never tells somebody their own entry does not exist.
    let refusal = server::unsupported_language("a.zzz").to_string();
    assert!(refusal.contains(CERT_EXT), "{refusal}");

    // And the disclosure names the resolved command rather than the spelling in
    // the file.
    let line = server::declared_line(language, true);
    assert!(line.contains("rust-analyzer"), "{line}");
    assert!(line.contains("may start"), "{line}");
    assert!(
        matches!(server::presence(language), Presence::Found { .. }),
        "an absolute-path command must resolve without a PATH lookup"
    );
}

/// **The certification.** A file with an extension no built-in language claims,
/// answered by a server named in settings.json under a key that is not rust's.
///
/// The fixture is a real crate so rust-analyzer has a project to load; the file
/// asked about is the one with the declared extension. If the answer names the
/// symbols in it, every link in the chain — the settings shape, the validator,
/// the leaked `Language`, the absolute-path candidate, the pool key, the
/// handshake with the declared `initializationOptions`, `didOpen` with the
/// declared `languageId` — worked against a program Emma did not choose.
#[tokio::test(flavor = "multi_thread")]
async fn a_declared_server_answers_about_its_own_extension_for_real() {
    let Some(language) = installed() else { return };
    let sandbox = Sandbox::new();
    sandbox.write(
        "Cargo.toml",
        "[package]\nname = \"declared-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    sandbox.write("src/lib.rs", "pub fn anchor() {}\n");
    sandbox.write(
        &format!("src/thing.{CERT_EXT}"),
        "pub struct Declared {\n    pub name: String,\n}\n\n\
         pub fn describe(d: &Declared) -> String {\n    d.name.clone()\n}\n",
    );

    let pool = Arc::new(Pool::with_enabled([CERT_KEY.to_string()]));
    let client = pool
        .client(&sandbox.canonical(), language)
        .await
        .expect("the declared server started");
    eprintln!("--- server ---\n{}", client.server().banner());

    let out = DocumentSymbols::new(pool)
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": format!("src/thing.{CERT_EXT}") }),
        )
        .await
        .expect("no fault")
        .expect("no tool error");
    eprintln!("--- DocumentSymbols on .{CERT_EXT} ---\n{}", out.content);

    for expected in ["Declared", "describe", "Rustish"] {
        assert!(
            out.content.contains(expected),
            "{expected}: {}",
            out.content
        );
    }
    // The caveat travels with the answer, which is the whole reason it exists.
    assert!(
        out.content.contains("declared in your settings.json"),
        "{}",
        out.content
    );
}

// endregion: The one installation, and the real server
