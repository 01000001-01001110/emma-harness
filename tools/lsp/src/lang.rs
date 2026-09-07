//! Which languages this crate speaks, and how each server is started.
//!
//! One table, `&'static`, and every other module reads it. The point of the
//! table is that adding an eighth language is an entry rather than a code path:
//! discovery, the handshake, readiness, the pool key and every refusal message
//! are already written against `Language` and know nothing about any particular
//! server.
//!
//! # Four of the seven are not executables
//!
//! This is the fact that shapes the type. `rust-analyzer` and `terraform-ls`
//! are binaries you exec. `ansible-language-server` and `bash-language-server`
//! are JavaScript, `Bicep.LangServer.dll` is a .NET assembly, and PowerShell
//! Editor Services is a `.ps1` bootstrapper. So a candidate carries a
//! [`Launcher`] as well as a name, and "the server is missing" and "the thing
//! that runs the server is missing" are two different refusals with two
//! different fixes.
//!
//! # Where the servers were found, and on which machine
//!
//! Two probes, and they disagree, which is the useful part. The macOS box this
//! table was first written on (2026-08-26) had nothing on `PATH` and four of
//! the seven inside VS Code extensions. The Windows box it was ported to
//! (2026-09-06) had rust-analyzer twice — as a rustup component and inside
//! `rust-lang.rust-analyzer-0.3.3033-win32-x64` — the bicep assembly with a
//! `dotnet` to run it, and `Start-EditorServices.ps1` with **no** `pwsh`, which
//! is the [`Launcher`] case in the wild rather than in a fixture.
//!
//! The extension route is worth far more than it looks: it is how a developer
//! box that has never heard of Emma still has working language servers on it,
//! and it was the only route to bicep on either machine.
//!
//! No version is hardcoded anywhere here. Extension directories are matched by
//! prefix and the highest name wins, exactly as the rust path already did.
//!
//! # Why three languages are off by default
//!
//! `ToolMeta.reaches_network` is a static, per-tool bit, and these tools
//! declare `false`. For rust that is true by construction: [`RUST_INIT`] passes
//! `--offline` to every cargo invocation. For `terraform-ls`, the bicep server
//! and `ansible-language-server` it is not something this repo can assert. They
//! fetch provider schemas, Azure type indexes and Galaxy metadata, and no off
//! switch for that was verified on either machine.
//!
//! So [`Language::network`] is true for those three, they are absent from
//! [`DEFAULT_ENABLED`], and the refusal for a disabled language says why. A
//! capability that is off is honest. A `reaches_network: false` that is wrong
//! is not.
//!
//! # An eighth language without a recompile
//!
//! The table above is the seven Emma knows about. A user adds theirs in
//! `~/.emma/settings.json` under `lsp.servers`, and [`plan_user_servers`] turns
//! that JSON into more [`Language`] entries — same type, same discovery, same
//! refusals, no code path of their own. See that function for what is
//! validated and what each refusal says, and [`table`] for the one rule that
//! decides precedence: **a key matching a built-in replaces it in place, and a
//! new key goes after the seven**, so a stranger's entry cannot take `.rs` off
//! rust-analyzer by being read first.

use std::path::Path;
use std::sync::OnceLock;

use serde_json::Value;

// region: The types
// ---------------------------------------------------------------------------
// The types
//
// A language, its candidates, and how a candidate is turned into a command
// line. Kept above the table so the table reads as data.
// ---------------------------------------------------------------------------

/// One language, its server candidates in preference order, and everything the
/// rest of the crate needs to talk to whichever one is found.
#[derive(Debug, PartialEq, Eq)]
pub struct Language {
    /// The key used in `settings.json`, in `EMMA_LSP_SERVER_<KEY>` and as half
    /// of the pool key. Lowercase ASCII.
    pub key: &'static str,
    /// What the refusals call it.
    pub label: &'static str,
    /// The LSP `languageId` sent on `textDocument/didOpen`.
    pub language_id: &'static str,
    /// Lowercased file extensions this language claims, without the dot.
    pub extensions: &'static [&'static str],
    pub candidates: &'static [Candidate],
    /// `initializationOptions`, as a JSON object. `"{}"` when the server needs
    /// none.
    pub init_options: &'static str,
    /// How to get the server, printed in the refusal when none was found.
    pub install: &'static str,
    /// The server may reach the network while answering. See the module doc.
    pub network: bool,
    /// A known hole in what this server can answer, carried into every result
    /// that came from it. Empty when there is nothing to declare.
    pub caveat: &'static str,
    /// This entry came out of somebody's `settings.json` rather than out of
    /// [`LANGUAGES`].
    ///
    /// Worth a field rather than a lookup because three different messages need
    /// it — the startup disclosure, the Settings screen and the refusal for a
    /// language that is off — and each of them is saying the same thing: Emma
    /// did not choose this program and has never run it.
    pub user_declared: bool,
}

/// One way to start one language's server.
#[derive(Debug, PartialEq, Eq)]
pub struct Candidate {
    pub launcher: Launcher,
    /// The name to look for on `PATH`. Empty when this candidate only ever
    /// exists inside an editor extension.
    pub bin: &'static str,
    /// Where an editor extension keeps this candidate, if one does.
    pub bundled: Option<Bundled>,
    /// Arguments that follow the entry point. `terraform-ls` needs `serve`;
    /// most stdio servers need `--stdio`.
    pub args: &'static [&'static str],
    pub identity: Identity,
}

/// An entry point that lives inside a VS Code extension directory.
#[derive(Debug, PartialEq, Eq)]
pub struct Bundled {
    /// The extension directory's prefix, without a version. Matched against
    /// `~/.vscode/extensions/<prefix>*`.
    pub prefix: &'static str,
    /// The entry point's path relative to the extension directory, in `/`
    /// segments.
    pub relative: &'static str,
}

/// What has to be run to run the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launcher {
    /// Exec the entry point itself.
    Direct,
    /// `node <entry> <args>`.
    Node,
    /// `dotnet <entry> <args>`.
    Dotnet,
    /// `pwsh -File <entry> <the parameter set below>`.
    PowerShell,
}

impl Launcher {
    /// The program that has to exist on `PATH` before this candidate can run,
    /// beyond the entry point itself.
    pub fn program(self) -> Option<&'static str> {
        match self {
            Self::Direct => None,
            Self::Node => Some("node"),
            Self::Dotnet => Some("dotnet"),
            Self::PowerShell => Some("pwsh"),
        }
    }

    /// How to install the launcher, for the refusal when it is absent.
    ///
    /// **Every arm names a download page first and a package manager second,
    /// and the package manager is the host's.** The fork this came from said
    /// `brew install node` unconditionally. A refusal that tells a Windows user
    /// to run `brew` is a refusal with no remedy, which is the class of message
    /// the whole crate exists to avoid: it reads like help and cannot be acted
    /// on. The URL works everywhere and is therefore what leads.
    pub fn install(self) -> &'static str {
        match self {
            Self::Direct => "",
            Self::Node => {
                if cfg!(target_os = "macos") {
                    "install Node.js from https://nodejs.org, or `brew install node`"
                } else if cfg!(windows) {
                    "install Node.js from https://nodejs.org, or `winget install OpenJS.NodeJS`"
                } else {
                    "install Node.js from https://nodejs.org, or use your distribution's package \
                     manager"
                }
            }
            Self::Dotnet => {
                if cfg!(target_os = "macos") {
                    "install the .NET runtime from https://dotnet.microsoft.com/download, or \
                     `brew install --cask dotnet-sdk`"
                } else if cfg!(windows) {
                    "install the .NET runtime from https://dotnet.microsoft.com/download, or \
                     `winget install Microsoft.DotNet.Runtime.8`"
                } else {
                    "install the .NET runtime from https://dotnet.microsoft.com/download"
                }
            }
            Self::PowerShell => {
                if cfg!(target_os = "macos") {
                    "install PowerShell 7 from https://github.com/PowerShell/PowerShell, or \
                     `brew install --cask powershell`"
                } else if cfg!(windows) {
                    "install PowerShell 7 from https://github.com/PowerShell/PowerShell, or \
                     `winget install Microsoft.PowerShell`. Windows PowerShell 5.1 \
                     (`powershell.exe`) is a different program and is not `pwsh`"
                } else {
                    "install PowerShell 7 from https://github.com/PowerShell/PowerShell"
                }
            }
        }
    }
}

/// How a candidate proves it is what its filename says.
///
/// The rust case is the reason this exists at all and it is not hypothetical:
/// `~/.cargo/bin/rust-analyzer` is a rustup proxy that exists whether or not
/// the component behind it does, is first on `PATH`, and exits **0** while
/// printing a complaint to stderr. Choosing by existence picks a binary that
/// will never speak LSP, and the failure arrives ninety seconds later as a
/// handshake that never answers. Reproduced on the Windows box on 2026-09-06,
/// before `rustup component add rust-analyzer`:
///
/// ```text
/// $ rust-analyzer --version
/// error: Unknown binary 'rust-analyzer.exe' in official toolchain
/// '1.94.1-x86_64-pc-windows-msvc'.
/// $ echo $?
/// 1
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    /// Run the entry point with one flag and read the first line of stdout.
    VersionLine { arg: &'static str, expect: Expect },
    /// The launcher plus the filename plus the directory it was found in are
    /// the identity.
    ///
    /// Used for every entry point that is not a binary, because booting node to
    /// ask a bundled `server.js` its version costs a process and most LSP
    /// servers do not implement the flag. Also used for the `PATH` binaries
    /// whose `--version` behaviour could not be verified on either machine,
    /// since probing with a flag a program may not accept turns a working
    /// server into a rejected one. That trade is the right way round:
    /// [`Self::VersionLine`] is for the case where a *wrong* candidate is known
    /// to be present.
    LauncherOnly,
}

/// What the first line of a `--version` has to look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// Starts with this string. rust-analyzer prints its own name.
    StartsWith(&'static str),
    /// A bare dotted version, which is what `terraform-ls --version` prints
    /// (measured: `0.39.0`).
    LooksLikeVersion,
}

impl Expect {
    pub fn matches(self, line: &str) -> bool {
        match self {
            Self::StartsWith(prefix) => line.starts_with(prefix),
            Self::LooksLikeVersion => {
                let head = line.split_whitespace().next().unwrap_or("");
                let head = head.strip_prefix('v').unwrap_or(head);
                let mut parts = head.split('.');
                let ok = |p: Option<&str>| {
                    p.is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
                };
                ok(parts.next()) && ok(parts.next())
            }
        }
    }

    pub fn describe(self) -> String {
        match self {
            Self::StartsWith(prefix) => format!("a line beginning {prefix:?}"),
            Self::LooksLikeVersion => "a version number".to_string(),
        }
    }
}

// endregion: The types

// region: The table
// ---------------------------------------------------------------------------
// The table
//
// Seven languages. Each entry is data, and the comment above it records what
// was verified on a real machine rather than what the documentation claims —
// and names which machine, because the two disagreed.
// ---------------------------------------------------------------------------

/// What rust-analyzer is told at `initialize`, and the reason `read_only: true`
/// is not a lie.
///
/// Three switches are off, and each of them is off because leaving it on would
/// make an *analysis* tool run the analysed project's code or write into its
/// build directory:
///
/// - `check.enable`: rust-analyzer's default behaviour runs `cargo check` on
///   save. That is a compiler writing megabytes into `target/`, started by a
///   tool the approval gate lets through without asking anybody.
/// - `cargo.buildScripts.enable`: `build.rs` is arbitrary code from the
///   repository under analysis. Emma will read a stranger's repository; it will
///   not execute its build scripts to answer "where is this used".
/// - `procMacro.enable`: proc macro expansion loads compiled macro libraries,
///   which only exist because build scripts and a build produced them.
///
/// `cargo.extraArgs: ["--offline"]` is the fourth, and it is what makes
/// `reaches_network: false` true by construction rather than by hope.
///
/// **The cost, stated because the tools state it to the model too.** Without
/// proc macros, items generated by `derive` and by attribute macros are not in
/// the index, and references *into* macro-generated code will be missed. That
/// is a real hole in exactly the direction this crate is most dangerous — a
/// confidently short answer — which is why it is also rust's [`Language::caveat`]
/// and reaches the model on every result.
pub const RUST_INIT: &str = r#"{
  "check": { "enable": false },
  "checkOnSave": false,
  "cargo": { "buildScripts": { "enable": false }, "extraArgs": ["--offline"] },
  "procMacro": { "enable": false }
}"#;

/// Every language, in a stable order.
pub static LANGUAGES: &[Language] = &[
    // macOS box, 2026-08-26: no rust-analyzer anywhere — not on `PATH`, not a
    // rustup component, no `rust-lang.rust-analyzer-*` extension.
    // Windows box, 2026-09-06: both routes present.
    // `rust-lang.rust-analyzer-0.3.3033-win32-x64/server/rust-analyzer.exe`,
    // and after `rustup component add rust-analyzer`, a `PATH` binary printing
    // `rust-analyzer 1.94.1 (e408947b 2026-03-25)`. Before that command the
    // same `PATH` entry was the rustup proxy described on [`Identity`].
    Language {
        key: "rust",
        label: "Rust",
        language_id: "rust",
        extensions: &["rs"],
        candidates: &[Candidate {
            launcher: Launcher::Direct,
            bin: "rust-analyzer",
            bundled: Some(Bundled {
                prefix: "rust-lang.rust-analyzer-",
                relative: "server/rust-analyzer",
            }),
            args: &[],
            identity: Identity::VersionLine {
                arg: "--version",
                expect: Expect::StartsWith("rust-analyzer"),
            },
        }],
        init_options: RUST_INIT,
        install: "`rustup component add rust-analyzer`",
        network: false,
        caveat: "Proc macros and build scripts are disabled, so items generated by `derive` \
                 and by attribute macros are not in the index and references into \
                 macro-generated code will be missed.",
        user_declared: false,
    },
    // Not installed on either box in any form. `bash-language-server` on `PATH`
    // is an npm shim; the VS Code extension ships the same server as a bundled
    // script, and neither was present.
    Language {
        key: "bash",
        label: "Bash",
        language_id: "shellscript",
        extensions: &["sh", "bash", "zsh", "ksh"],
        candidates: &[
            Candidate {
                launcher: Launcher::Direct,
                bin: "bash-language-server",
                bundled: None,
                args: &["start"],
                identity: Identity::LauncherOnly,
            },
            Candidate {
                launcher: Launcher::Node,
                bin: "",
                bundled: Some(Bundled {
                    prefix: "mads-hartmann.bash-ide-vscode-",
                    relative: "out/server.js",
                }),
                args: &["--stdio"],
                identity: Identity::LauncherOnly,
            },
        ],
        init_options: "{}",
        install: "`npm install -g bash-language-server`",
        network: false,
        caveat: "Shell analysis is lexical. It reports parse errors and unset-variable \
                 warnings; it does not know what a command does.",
        user_declared: false,
    },
    // Present and unrunnable on **both** boxes, for the same reason:
    // `ms-vscode.powershell-2025.4.0` ships `Start-EditorServices.ps1` and
    // neither machine has a `pwsh`. Windows has `powershell.exe`, which is
    // Windows PowerShell 5.1 and a different program. This is the live case for
    // [`Launcher`]: the server is there, the thing that runs it is not, and the
    // refusal must say which. The `-Stdio` parameter set below was read out of
    // that script rather than remembered. Expected, never started.
    Language {
        key: "powershell",
        label: "PowerShell",
        language_id: "powershell",
        extensions: &["ps1", "psm1", "psd1"],
        candidates: &[Candidate {
            launcher: Launcher::PowerShell,
            bin: "",
            bundled: Some(Bundled {
                prefix: "ms-vscode.powershell-",
                relative: "modules/PowerShellEditorServices/Start-EditorServices.ps1",
            }),
            args: &[],
            identity: Identity::LauncherOnly,
        }],
        install: "install the PowerShell extension for VS Code (`ms-vscode.powershell`), \
                  which bundles PowerShell Editor Services, and PowerShell 7 to run it",
        init_options: "{}",
        network: false,
        caveat: "PowerShell Editor Services has never been started by Emma, on any machine. \
                 It is wired from the parameter set in its own bootstrap script; treat a \
                 first answer from it as unproven.",
        user_declared: false,
    },
    // macOS box: `hashicorp.terraform-2.40.0-darwin-arm64/bin/terraform-ls`
    // printed `0.39.0`. Windows box: no terraform extension and no
    // `terraform-ls` on `PATH`. One language key for both dialects, because
    // terraform-ls serves OpenTofu too; `tofu-ls` is listed first for anyone
    // who has one.
    Language {
        key: "terraform",
        label: "Terraform and OpenTofu",
        language_id: "terraform",
        extensions: &["tf", "tfvars"],
        candidates: &[
            Candidate {
                launcher: Launcher::Direct,
                bin: "tofu-ls",
                bundled: None,
                args: &["serve"],
                identity: Identity::VersionLine {
                    arg: "--version",
                    expect: Expect::LooksLikeVersion,
                },
            },
            Candidate {
                launcher: Launcher::Direct,
                bin: "terraform-ls",
                bundled: Some(Bundled {
                    prefix: "hashicorp.terraform-",
                    relative: "bin/terraform-ls",
                }),
                args: &["serve"],
                identity: Identity::VersionLine {
                    arg: "--version",
                    expect: Expect::LooksLikeVersion,
                },
            },
        ],
        init_options: "{}",
        install: "download terraform-ls from \
                  https://github.com/hashicorp/terraform-ls/releases, or install the \
                  HashiCorp Terraform extension for VS Code, which bundles it",
        network: true,
        caveat: "terraform-ls answers about providers from schemas under `.terraform/`. \
                 In a directory that has never been initialised it knows the language and \
                 not the providers.",
        user_declared: false,
    },
    // macOS box: `Bicep.LangServer.dll` in the VS Code extension, the only
    // native binary beside it a Windows PE32+, and no `dotnet` — so unrunnable.
    // Windows box, 2026-09-06:
    // `ms-azuretools.vscode-bicep-0.46.1/bicepLanguageServer/Bicep.LangServer.dll`
    // **and** `dotnet` on `PATH`, which makes this the one non-rust language
    // the port could resolve for real.
    Language {
        key: "bicep",
        label: "Bicep",
        language_id: "bicep",
        extensions: &["bicep", "bicepparam"],
        candidates: &[
            Candidate {
                launcher: Launcher::Direct,
                bin: "bicep-langserver",
                bundled: None,
                args: &[],
                identity: Identity::LauncherOnly,
            },
            Candidate {
                launcher: Launcher::Dotnet,
                bin: "",
                bundled: Some(Bundled {
                    prefix: "ms-azuretools.vscode-bicep-",
                    relative: "bicepLanguageServer/Bicep.LangServer.dll",
                }),
                args: &[],
                identity: Identity::LauncherOnly,
            },
        ],
        init_options: "{}",
        install: "install the Bicep extension for VS Code (`ms-azuretools.vscode-bicep`), \
                  which bundles the language server, and the .NET runtime to run it",
        network: true,
        caveat: "The bicep server has been resolved but not driven to an answer by Emma.",
        user_declared: false,
    },
    // macOS box: `node .../ansible-language-server/dist/server.js` returned 0
    // rather than failing to load — verified to start, not verified to answer.
    // Windows box: no `redhat.ansible` extension and nothing on `PATH`.
    Language {
        key: "ansible",
        label: "Ansible",
        language_id: "ansible",
        extensions: &["yml", "yaml"],
        candidates: &[
            Candidate {
                launcher: Launcher::Direct,
                bin: "ansible-language-server",
                bundled: None,
                args: &["--stdio"],
                identity: Identity::LauncherOnly,
            },
            Candidate {
                launcher: Launcher::Node,
                bin: "",
                bundled: Some(Bundled {
                    prefix: "redhat.ansible-",
                    relative: "packages/ansible-language-server/dist/server.js",
                }),
                args: &["--stdio"],
                identity: Identity::LauncherOnly,
            },
        ],
        init_options: "{}",
        install: "`npm install -g @ansible/ansible-language-server`, or install the Ansible \
                  extension for VS Code (`redhat.ansible`), which bundles it",
        network: true,
        caveat: "ansible-language-server shells out to `ansible-lint` when it is installed. \
                 Without it, the answers are syntax and module-name checks only.",
        user_declared: false,
    },
    // Nothing installed on either box. Pylance is on both and is deliberately
    // not a candidate: its licence restricts it to Microsoft products. The four
    // here are the mainstream open servers, in the order most people would want
    // them.
    Language {
        key: "python",
        label: "Python",
        language_id: "python",
        extensions: &["py", "pyi"],
        candidates: &[
            Candidate {
                launcher: Launcher::Direct,
                bin: "basedpyright-langserver",
                bundled: None,
                args: &["--stdio"],
                identity: Identity::LauncherOnly,
            },
            Candidate {
                launcher: Launcher::Direct,
                bin: "pyright-langserver",
                bundled: None,
                args: &["--stdio"],
                identity: Identity::LauncherOnly,
            },
            Candidate {
                launcher: Launcher::Direct,
                bin: "pylsp",
                bundled: None,
                args: &[],
                identity: Identity::LauncherOnly,
            },
            Candidate {
                launcher: Launcher::Direct,
                bin: "jedi-language-server",
                bundled: None,
                args: &[],
                identity: Identity::LauncherOnly,
            },
        ],
        init_options: "{}",
        install: "`npm install -g pyright`, or `pipx install python-lsp-server`",
        network: false,
        caveat: "",
        user_declared: false,
    },
];

/// The languages Emma starts without being asked.
///
/// The three left out are the three whose servers may reach the network. See
/// the module doc.
pub const DEFAULT_ENABLED: &[&str] = &["rust", "bash", "powershell", "python"];

// endregion: The table

// region: Lookup
// ---------------------------------------------------------------------------
// Lookup
//
// Extension to language, plus the one case where an extension is not enough.
// Pure, so the whole of it is testable without a filesystem or a server.
// ---------------------------------------------------------------------------

/// The language with this key, or `None`.
pub fn by_key(key: &str) -> Option<&'static Language> {
    let key = key.trim().to_ascii_lowercase();
    table().iter().copied().find(|l| l.key == key)
}

/// Every key, for the refusal that has to list them.
pub fn keys() -> Vec<&'static str> {
    table().iter().map(|l| l.key).collect()
}

/// The directory segments that make a YAML file an Ansible file.
const ANSIBLE_DIRS: &[&str] = &[
    "playbooks",
    "roles",
    "tasks",
    "handlers",
    "group_vars",
    "host_vars",
    "molecule",
];

/// Whether a YAML file under `root` is Ansible's.
///
/// **Why this is not just the extension.** Ansible is the one language in the
/// table whose extension it does not own. Handing every `.yml` to
/// ansible-language-server means answering about `docker-compose.yml`, a GitHub
/// Actions workflow and a Kubernetes manifest with a tool that believes they
/// are playbooks, and the answer would look exactly like a real one. So the
/// claim needs corroboration from the tree: an `ansible.cfg` at the root, or a
/// directory segment that only an Ansible layout has.
///
/// It will miss a loose playbook in a bare directory. That is the right way to
/// be wrong: the refusal names the heuristic, and the fix is to say so with the
/// override.
pub fn looks_like_ansible(root: &Path, file: &Path) -> bool {
    if root.join("ansible.cfg").is_file() {
        return true;
    }
    let relative = file.strip_prefix(root).unwrap_or(file);
    relative.components().any(|c| {
        let segment = c.as_os_str().to_string_lossy().to_ascii_lowercase();
        ANSIBLE_DIRS.contains(&segment.as_str())
    })
}

/// The language for a file, or `None` when nothing here claims it.
///
/// `root` is needed only for the Ansible corroboration above.
pub fn for_path(root: &Path, file: &Path) -> Option<&'static Language> {
    let extension = file
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())?;
    let language = table()
        .iter()
        .copied()
        .find(|l| l.extensions.contains(&extension.as_str()))?;
    if language.key == "ansible" && !looks_like_ansible(root, file) {
        return None;
    }
    Some(language)
}

/// The `.yml` case, phrased so the reader learns the rule rather than the
/// verdict.
pub fn yaml_not_ansible(shown: &str) -> String {
    format!(
        "{shown} is YAML, and Emma only hands YAML to ansible-language-server when the tree \
         says it is Ansible: an `ansible.cfg` at the working directory, or one of these path \
         segments: {}. Answering about a compose file or a workflow with a playbook server \
         would produce a result that reads exactly like a real one.",
        ANSIBLE_DIRS.join(", ")
    )
}

// endregion: Lookup

// region: Servers declared in settings.json
// ---------------------------------------------------------------------------
// Servers declared in settings.json
//
// The eighth language, without a recompile. Everything here produces the same
// `Language` the table above holds, so nothing downstream — discovery, the
// pool, the seven tools, the refusals — has a second code path for it.
// ---------------------------------------------------------------------------

/// The effective table: the seven, with whatever `lsp.servers` declared.
///
/// A `OnceLock` because a `Language` is `&'static` everywhere in this crate —
/// the pool is keyed by one, `for_path` returns one, `Client` holds one for as
/// long as a server lives. A declared entry is therefore leaked, once, at
/// startup: bounded by the number of entries in one person's settings file,
/// and freed by the process exiting, which is the same lifetime the built-in
/// table has.
static TABLE: OnceLock<Vec<&'static Language>> = OnceLock::new();

/// Every language in force, built-ins and declared, in the order `for_path`
/// consults them.
///
/// Reading this before [`install_user_servers`] runs freezes the table at the
/// seven, which is why the installer reports that case rather than silently
/// dropping the entries: see [`Plan::refusals`].
pub fn table() -> &'static [&'static Language] {
    TABLE.get_or_init(|| LANGUAGES.iter().collect())
}

/// The fields one entry under `lsp.servers` may carry.
///
/// Named so the refusal for a misspelling can list them, which is the whole
/// reason an unknown field is refused rather than ignored: `"arguments"` for
/// `"args"` would otherwise start a server with none of the arguments it needs
/// and nothing anywhere would say so.
pub const SERVER_FIELDS: &[&str] = &[
    "label",
    "language_id",
    "extensions",
    "command",
    "args",
    "init_options",
    "install",
];

/// What a set of declarations comes to: a table, the entries that made it, and
/// a sentence for each that did not.
pub struct Plan {
    /// The effective table, ready for [`install_user_servers`].
    pub table: Vec<&'static Language>,
    /// The declared entries that were accepted, in the order they were given.
    pub declared: Vec<&'static Language>,
    /// One sentence per rejected entry, addressed to whoever wrote the file.
    /// A refusal here is never fatal: the other entries and all seven built-ins
    /// still work, for the same reason [`crate::Pool::unknown_keys`] keeps an
    /// unknown key rather than rejecting the file.
    pub refusals: Vec<String>,
}

/// Turn `lsp.servers` into languages, without touching the global table.
///
/// **Extend by default, override by key.** A key that names one of the seven
/// replaces that entry *entirely* — no field-wise merge, which is what
/// `crates/harness` refuses and for the same reason: a half-overridden entry is
/// a shape neither the writer nor the reader has ever seen. A key that names
/// nothing goes after the seven, so first-match-wins in [`for_path`] cannot let
/// a declared entry take `.rs` away from rust-analyzer.
///
/// **What is validated, and why each one is a refusal rather than a default.**
/// A missing `command` or `extensions` leaves nothing to run or nothing to run
/// it on. An unknown field is a misspelling of a known one. A `command` is one
/// program name or one absolute path and never a command line — `args` is a
/// separate array — which is `crate::server`'s and `usertools`'s existing
/// ruling: splitting on spaces is the first half of running text through a
/// shell, and a path with a space in it is not an argument boundary. And
/// `init_options` has to be an object here, because the alternative is that it
/// reaches the handshake and fails there, one process later, in a message about
/// JSON.
///
/// **A declared server is `network: true` and is never in [`DEFAULT_ENABLED`].**
/// Emma cannot assert `reaches_network: false` about a program it did not
/// choose, and these seven tools declare that bit statically. Turning one on
/// stays an explicit act, which is the same two-step the three network
/// languages already have.
pub fn plan_user_servers(entries: impl IntoIterator<Item = (String, Value)>) -> Plan {
    let mut table: Vec<&'static Language> = LANGUAGES.iter().collect();
    let mut declared = Vec::new();
    let mut refusals = Vec::new();

    for (raw_key, value) in entries {
        let key = raw_key.trim().to_ascii_lowercase();
        match build_user_language(&key, &value, &table) {
            Err(why) => refusals.push(why),
            Ok(language) => {
                match table.iter().position(|l| l.key == language.key) {
                    // Replaced in place: an override of `rust` keeps rust's
                    // position, because the person who wrote it meant to answer
                    // for `.rs` and moving it behind six other entries would
                    // make that depend on what else they had declared.
                    Some(at) => table[at] = language,
                    None => table.push(language),
                }
                declared.push(language);
            }
        }
    }

    Plan {
        table,
        declared,
        refusals,
    }
}

/// [`plan_user_servers`], installed as the table every other module reads.
///
/// Call it once, before anything asks a language question. Calling it after
/// something already has is reported rather than ignored: the entries would
/// have no effect at all, and a settings file that silently does nothing is the
/// failure this whole module is trying to avoid.
pub fn install_user_servers(entries: impl IntoIterator<Item = (String, Value)>) -> Plan {
    let mut plan = plan_user_servers(entries);
    if TABLE.set(plan.table.clone()).is_err() && !plan.declared.is_empty() {
        plan.refusals.push(format!(
            "the language table had already been read when {} declared server(s) from \
             settings.json were installed, so they are not in force. This is a bug in Emma's \
             startup order, not in your settings file.",
            plan.declared.len()
        ));
        plan.declared.clear();
    }
    plan
}

/// One entry, validated into a leaked [`Language`], or the sentence that says
/// why not.
fn build_user_language(
    key: &str,
    value: &Value,
    table: &[&'static Language],
) -> Result<&'static Language, String> {
    let at = format!("lsp.servers.{key:?}");
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "lsp.servers has a key Emma cannot use: {key:?}. A language key is lowercase \
             letters, digits, \"-\" or \"_\": it is half of the pool key and the whole of the \
             EMMA_LSP_SERVER_<KEY> override's name."
        ));
    }
    let Some(object) = value.as_object() else {
        return Err(format!(
            "{at} must be a JSON object with at least \"command\" and \"extensions\", and it is \
             {}. The server was not declared.",
            json_kind(value)
        ));
    };
    if let Some(unknown) = object.keys().find(|k| !SERVER_FIELDS.contains(&k.as_str())) {
        return Err(format!(
            "{at} has a field Emma does not know: {unknown:?}. The fields are: {}. The server \
             was not declared, because a misspelled field is arguments or options silently \
             dropped — and a server started with half of what you wrote answers, which is \
             worse than one that does not start.",
            SERVER_FIELDS.join(", ")
        ));
    }

    let command = match object.get("command") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            return Err(format!(
                "{at} has no \"command\". It is one program name to look up on PATH, or one \
                 absolute path — never a command line: arguments go in \"args\", because \
                 splitting a string on spaces is the first half of running it through a shell, \
                 and a path with a space in it is not an argument boundary."
            ))
        }
    };
    let args = string_list(object.get("args"), &at, "args")?;

    let extensions = string_list(object.get("extensions"), &at, "extensions")?;
    if extensions.is_empty() {
        return Err(format!(
            "{at} claims no extensions. \"extensions\" is the only thing that sends a file to \
             this server: [\"go\"] means Emma asks it about `.go` files."
        ));
    }
    let extensions: Vec<String> = extensions
        .iter()
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .collect();
    if extensions.iter().any(String::is_empty) {
        return Err(format!(
            "{at} has an empty string in \"extensions\". An extension is written without the \
             dot: \"go\", not \".go\" or \"\"."
        ));
    }
    // The collision check runs against the table as it stands, minus the entry
    // this one replaces — overriding `rust` must be allowed to keep `.rs`.
    for extension in &extensions {
        if let Some(owner) = table
            .iter()
            .find(|l| l.key != key && l.extensions.contains(&extension.as_str()))
        {
            return Err(format!(
                "{at} claims the extension {extension:?}, which {} already claims. The first \
                 language that claims an extension answers for it, so this entry would never \
                 be reached. Choose another extension, or give this entry the key {:?} to \
                 replace {} entirely.",
                owner.label, owner.key, owner.label
            ));
        }
    }

    let init_options = match object.get("init_options") {
        None | Some(Value::Null) => "{}".to_string(),
        Some(v) if v.is_object() => v.to_string(),
        Some(v) => {
            return Err(format!(
                "{at} has an \"init_options\" that is {}. It is sent to the server as the \
                 handshake's initializationOptions, which is an object: write {{}} for none.",
                json_kind(v)
            ))
        }
    };

    let label = match object.get("label") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => key.to_string(),
    };
    let language_id = match object.get("language_id") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => key.to_string(),
    };
    let install = match object.get("install") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => format!(
            "whatever installs `{command}` on this machine, or set \"command\" in \
             lsp.servers.{key:?} to an absolute path"
        ),
    };

    let candidates: &'static [Candidate] = Box::leak(Box::new([Candidate {
        launcher: Launcher::Direct,
        bin: leak(command),
        bundled: None,
        args: leak_list(args),
        // Never probed. A `--version` is a flag a stranger's program may not
        // accept, and rejecting a working server for answering it oddly is the
        // trade `Identity` already reasons about — `VersionLine` is for the
        // case where a *wrong* candidate is known to be on the machine, which
        // is a fact about rust-analyzer and rustup, not about this entry.
        identity: Identity::LauncherOnly,
    }]));

    Ok(Box::leak(Box::new(Language {
        key: leak(key.to_string()),
        label: leak(label.clone()),
        language_id: leak(language_id),
        extensions: leak_list(extensions),
        candidates,
        init_options: leak(init_options),
        install: leak(install),
        // See the function doc: not a claim about this server, an admission
        // that Emma has none to make.
        network: true,
        caveat: leak(format!(
            "{label} is declared in your settings.json. Emma did not choose this server, has \
             never driven it to an answer, and cannot promise it stays off the network — these \
             tools declare `reaches_network: false` and that bit was decided before this entry \
             existed. Treat a first answer from it as unproven.",
        )),
        user_declared: true,
    })))
}

/// `["a", "b"]` or nothing, and a refusal for anything else.
fn string_list(value: Option<&Value>, at: &str, field: &str) -> Result<Vec<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|i| {
                i.as_str().map(str::to_string).ok_or_else(|| {
                    format!(
                        "{at} has a {field:?} containing {}, and every entry has to be a string.",
                        json_kind(i)
                    )
                })
            })
            .collect(),
        Some(other) => Err(format!(
            "{at} has a {field:?} that is {}, and it has to be an array of strings.",
            json_kind(other)
        )),
    }
}

/// What a value is, in the words a settings file uses.
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// One declared string, for the life of the process. See [`TABLE`].
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn leak_list(items: Vec<String>) -> &'static [&'static str] {
    let leaked: Vec<&'static str> = items.into_iter().map(leak).collect();
    Box::leak(leaked.into_boxed_slice())
}

// endregion: Servers declared in settings.json
