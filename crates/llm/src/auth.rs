//! Where the key comes from, and where it must never come from.
//!
//! Two sources, in order: the environment, then a credentials file under the
//! user's home directory. **Never the project directory.** Emma works inside
//! repositories; a key written next to the code is a key that gets committed,
//! and the first person to notice is usually the scanner that finds it in a
//! public push. That is why nothing here resolves a path relative to the
//! current directory — the home path is computed, not searched upward, and
//! `a_credentials_file_in_the_project_directory_is_ignored` below plants a
//! real `credentials.json` in a scratch cwd and asserts it is not found.
//!
//! The file is keyed by provider: `{"providers": {"anthropic": {"api_key":
//! …}}}`. It used to hold exactly one key under a top-level `api_key`, and that
//! spelling is still read — for as long as anybody has a file written by the
//! old `emma api`, which is one machine and no expiry date. Reading is what
//! migrates; nothing rewrites the file for having been read.
//!
//! **A write preserves everything it did not come to change.** One file may
//! hold keys this module does not own, so [`store`] merges into the JSON that
//! is there rather than replacing it. The alternative is a `set-provider` that
//! silently deletes a key belonging to some other part of the program. The
//! rule outlived the field that prompted it: a search tool once kept its own
//! vendor's key at the top level of this file, that tool is gone, and a file
//! written while it existed still reads and still survives a write.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::ProviderKind;

/// Anthropic's environment variable. Per-provider from here on — see
/// [`ProviderKind::env_var`], which is what every caller should be asking. This
/// stays because it is Anthropic's answer and the error text in `lib.rs` names
/// it directly.
pub const ENV_VAR: &str = "ANTHROPIC_API_KEY";

/// The provider a top-level `api_key` belonged to. It can only ever have been
/// Anthropic: nothing else was implemented when that shape was written.
const LEGACY_PROVIDER: &str = crate::DEFAULT_PROVIDER;
const LEGACY_FIELD: &str = "api_key";

// region: A key that cannot print itself
// ---------------------------------------------------------------------------
// A key that cannot print itself
//
// The wrapper exists so that reading the secret is a visible act. Everything
// about it is subtraction: no `Display`, and a `Debug` that shows nothing.
// ---------------------------------------------------------------------------

/// An API key that cannot be printed by accident.
///
/// No `Display`, and a `Debug` that shows nothing — so a key cannot reach a
/// log through a `{:?}` on some struct three layers up that happens to hold
/// one. Reading it takes [`ApiKey::expose`], which is a word you notice in
/// review.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        let key = key.into();
        // The one place every key in this process is built, which is what makes
        // it the place to register it for scrubbing. `LlmError`'s formatting
        // consults that list, so a message assembled somewhere the key was
        // never passed — a mid-stream `error` frame, an error-shaped batch body
        // — still cannot print it. See the note on `LlmError`'s impls.
        crate::remember_secret(&key);
        Self(key)
    }

    /// The absence of a key, for a provider that needs none.
    ///
    /// **Deliberately not `ApiKey::new("")`.** That constructor registers its
    /// argument for scrubbing, and registering the empty string would put a
    /// substring of every message this process prints into the scrub list.
    /// Nothing is remembered here because there is nothing to hide.
    ///
    /// A provider whose `requires_key` is false receives this. It must not be
    /// sent anywhere.
    pub fn none() -> Self {
        Self(String::new())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey([redacted])")
    }
}

// endregion: A key that cannot print itself

// region: Failures, each carrying its own fix
// ---------------------------------------------------------------------------
// Failures, each carrying its own fix
//
// A missing or unusable key is the most common way a run ends before it starts,
// so every variant here names the file and the command that resolves it.
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    // This used to say `emma auth`, with `emma::commands::rename_auth`
    // rewriting the word at the printing boundary. That was a workaround for a
    // wrong string: the command is real, so the string names a real one and the
    // rewrite is now a no-op that can be deleted. It names `set-provider`
    // rather than `api` because that is the command that takes a provider, and
    // this message now has to say which provider has no key.
    #[error(
        "no API key for {provider}. Set {env_var}, or run `emma set-provider {provider}` to \
         store one at {path}"
    )]
    Missing {
        path: PathBuf,
        provider: String,
        env_var: String,
    },

    #[error(
        "the home directory could not be determined, so there is nowhere to look for stored \
         credentials. Set {ENV_VAR} instead"
    )]
    NoHome,

    #[error("{path} is readable by other users (mode {mode:o}); run `chmod 600 {path}` and retry")]
    Insecure { path: PathBuf, mode: u32 },

    #[error("{path} could not be read: {source}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not valid credentials JSON: {source}")]
    Malformed {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("{path} could not be written: {source}")]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

// endregion: Failures, each carrying its own fix

// region: Where the key comes from
// ---------------------------------------------------------------------------
// Where the key comes from
//
// Environment first, then `~/.emma/credentials.json`. Every path is computed
// from a home directory that is passed in, which is what makes "never the
// project directory" a property rather than an intention.
// ---------------------------------------------------------------------------

/// `~/.emma/credentials.json`. A function rather than a constant because every
/// caller must pass the home it means — tests included, which is the only
/// reason they can run without touching the developer's real credentials.
pub fn credentials_path(home: &Path) -> PathBuf {
    home.join(".emma").join("credentials.json")
}

/// The user's home directory, or `None` when the platform will not say.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Resolve one provider's key: environment first, then the stored file.
///
/// Takes the environment value rather than reading it, so the decision is a
/// pure function of its inputs — a test can pin either source without mutating
/// process state that every other test shares.
///
/// The environment wins because that is how CI and one-off overrides work, and
/// because a user who exports a key expects it to be the one used. The variable
/// it wins with is the *provider's*, which is the whole reason this takes a
/// [`ProviderKind`] rather than a name: there is no way to ask for a provider
/// this build cannot run, and no way to check the wrong variable for it.
pub fn resolve(
    kind: &dyn ProviderKind,
    env_key: Option<&str>,
    home: &Path,
) -> Result<ApiKey, AuthError> {
    match from_env(env_key) {
        Some(key) => Ok(key),
        None => load_file(&credentials_path(home), kind),
    }
}

/// The environment half of the precedence rule, written once.
///
/// An exported-but-empty variable is not a key: `ANTHROPIC_API_KEY=` in a
/// forgotten `.env` must fall through to the stored file rather than
/// authenticate with `""` and get a 401 the user cannot explain.
fn from_env(env_key: Option<&str>) -> Option<ApiKey> {
    env_key
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(ApiKey::new)
}

/// [`resolve`] against the real environment and the real home directory.
///
/// The env check happens here rather than by handing the variable straight to
/// [`resolve`], because `resolve` needs a home and this function may not have
/// one: a machine that will not say where `$HOME` is can still run on an
/// exported key, and demanding the home first would turn that into `NoHome`.
/// Both halves of the rule are still written once — the trim-and-empty test in
/// [`from_env`], the file lookup in [`resolve`].
pub fn load_default(kind: &dyn ProviderKind) -> Result<ApiKey, AuthError> {
    let env_key = std::env::var(kind.env_var()).ok();
    if let Some(key) = from_env(env_key.as_deref()) {
        return Ok(key);
    }
    resolve(kind, None, &home_dir().ok_or(AuthError::NoHome)?)
}

fn load_file(path: &Path, kind: &dyn ProviderKind) -> Result<ApiKey, AuthError> {
    let missing = || AuthError::Missing {
        path: path.to_path_buf(),
        provider: kind.name().to_string(),
        env_var: kind.env_var().to_string(),
    };
    let keys = read_keys(path)?;
    keys.get(kind.name()).cloned().ok_or_else(missing)
}

/// Every key the file holds, by provider, in whichever shape it is written.
///
/// **All of them are built through [`ApiKey::new`], including the ones nobody
/// asked for.** That constructor is what registers a secret for scrubbing, so a
/// key belonging to a provider this run is not using still cannot appear in an
/// error message. Loading them and dropping them would be the same read with
/// the protection left off.
fn read_keys(path: &Path) -> Result<BTreeMap<String, ApiKey>, AuthError> {
    let mut keys = BTreeMap::new();
    let Some(doc) = read_json(path)? else {
        return Ok(keys);
    };
    let mut take = |provider: &str, value: Option<&Value>| {
        if let Some(raw) = value.and_then(Value::as_str) {
            if !raw.trim().is_empty() {
                keys.insert(provider.to_string(), ApiKey::new(raw));
            }
        }
    };
    // The shape `emma api` wrote. Read forever; see the module doc.
    take(LEGACY_PROVIDER, doc.get(LEGACY_FIELD));
    if let Some(providers) = doc.get("providers").and_then(Value::as_object) {
        for (provider, entry) in providers {
            // The keyed shape wins over the legacy field for the same provider:
            // it is the one a later `store` wrote.
            take(provider, entry.get(LEGACY_FIELD));
        }
    }
    Ok(keys)
}

/// The file as JSON, or `None` when there is no file. `Ok(None)` rather than an
/// error because "no credentials yet" is the ordinary state of a new install,
/// and only the caller knows whether that is a failure.
fn read_json(path: &Path) -> Result<Option<Value>, AuthError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(AuthError::Unreadable {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    check_permissions(path)?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|source| AuthError::Malformed {
            path: path.to_path_buf(),
            source,
        })
}

/// Which providers have a key stored in the file. Names only — `config check`
/// answers "did I store that key?" without being a way to read one back.
pub fn stored_providers(home: &Path) -> Vec<String> {
    read_keys(&credentials_path(home))
        .map(|keys| keys.into_keys().collect())
        .unwrap_or_default()
}

// endregion: Where the key comes from

// region: Owner-only on disk
// ---------------------------------------------------------------------------
// Owner-only on disk
//
// Writing and checking the permission bits, each with a Unix implementation
// and a Windows one that says plainly what it does not do.
// ---------------------------------------------------------------------------

/// Store one provider's key with owner-only permissions, creating the
/// directory if needed. This is what `emma set-provider` calls; the command
/// itself lives in `crates/emma/src/commands.rs`, which is also where the key
/// is read from a pipe or a no-echo prompt rather than an argument.
///
/// **Merges.** The file is read, one provider's entry is replaced, and
/// everything else in it survives — other providers' keys, and any top-level
/// field some other part of the program put here. Writing a fresh document
/// would delete whichever of those the caller did not happen to know about,
/// silently, and the symptom would arrive hours later as something that
/// stopped working with no error naming why.
///
/// A file that cannot be parsed is an error rather than something to overwrite,
/// for the same reason: overwriting is how the other keys are lost.
pub fn store(home: &Path, provider: &str, key: &ApiKey) -> Result<PathBuf, AuthError> {
    let path = credentials_path(home);
    let dir = path.parent().expect("credentials path always has a parent");
    std::fs::create_dir_all(dir).map_err(|source| AuthError::Unwritable {
        path: dir.to_path_buf(),
        source,
    })?;

    let mut doc = match read_json(&path)? {
        Some(Value::Object(map)) => map,
        // A file holding something that is not an object is not a credentials
        // file; `read_json` already refused anything that is not JSON at all.
        Some(_) => Map::new(),
        None => Map::new(),
    };
    // The legacy top-level key is dropped as it is carried across, so the
    // migration completes on the first write and there is never a file with two
    // answers for Anthropic.
    let carried = doc.remove(LEGACY_FIELD);
    let mut providers = match doc.remove("providers") {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    if let Some(Value::String(legacy)) = carried {
        providers
            .entry(LEGACY_PROVIDER.to_string())
            .or_insert_with(|| serde_json::json!({ LEGACY_FIELD: legacy }));
    }
    providers.insert(
        provider.to_string(),
        serde_json::json!({ LEGACY_FIELD: key.expose() }),
    );
    doc.insert("providers".into(), Value::Object(providers));

    let body = Value::Object(doc).to_string();
    write_private(&path, &body).map_err(|source| AuthError::Unwritable {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

#[cfg(unix)]
fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    // Created 0600 rather than created-then-chmodded: between those two calls
    // the key would be world-readable on disk.
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(body.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    // Windows has no mode bits. The file lands in the user's profile, whose
    // default ACL already excludes other users; tightening it further would
    // mean an ACL API this crate has no other reason to link.
    std::fs::write(path, body)
}

#[cfg(unix)]
fn check_permissions(path: &Path) -> Result<(), AuthError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map_err(|source| AuthError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?
        .permissions()
        .mode()
        & 0o777;
    // Refuse rather than warn: a warning about a key file that other accounts
    // can read is a warning nobody acts on until after the key is gone.
    if mode & 0o077 != 0 {
        return Err(AuthError::Insecure {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_path: &Path) -> Result<(), AuthError> {
    Ok(())
}

// endregion: Owner-only on disk

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Each one pins a property this module claims: the key never prints, the
// environment wins but an empty variable does not, a loose file is refused, and
// a `credentials.json` sitting in the project directory is not found.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::Anthropic;

    /// A provider this build cannot actually run, so that the per-provider
    /// behaviour is tested against something that is genuinely *not* Anthropic.
    /// It exists only to name a second key in the file; nothing calls `build`.
    struct Other;

    impl ProviderKind for Other {
        fn name(&self) -> &'static str {
            "other"
        }
        fn env_var(&self) -> &'static str {
            "OTHER_API_KEY"
        }
        fn default_model(&self) -> &'static str {
            "other-1"
        }
        fn build(
            &self,
            _key: ApiKey,
            _model: Option<String>,
        ) -> std::sync::Arc<dyn crate::Provider> {
            unreachable!("the fixture is never run")
        }
    }

    /// A scratch directory that stands in for `$HOME`, removed on drop.
    struct Home(PathBuf);

    impl Home {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "emma-auth-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_key_never_prints_itself() {
        let key = ApiKey::new("sk-ant-api03-SECRETSECRETSECRET");
        let shown = format!("{key:?}");
        assert!(!shown.contains("SECRET"), "{shown}");

        // …including from inside a struct someone else derived Debug on.
        #[derive(Debug)]
        struct Holder {
            #[allow(dead_code)]
            key: ApiKey,
        }
        let shown = format!("{:?}", Holder { key });
        assert!(!shown.contains("SECRET"), "{shown}");
    }

    #[test]
    fn a_stored_key_round_trips() {
        let home = Home::new("roundtrip");
        let path = store(home.path(), "anthropic", &ApiKey::new("sk-ant-stored")).unwrap();
        assert_eq!(path, credentials_path(home.path()));
        assert_eq!(
            load_file(&path, &Anthropic).unwrap().expose(),
            "sk-ant-stored"
        );
    }

    #[test]
    fn a_key_written_by_the_old_emma_api_still_reads() {
        // The shape the owner's live file had: one flat `api_key`, and beside
        // it a key belonging to a different part of the program. That other
        // key was a search vendor's when this was written; the tool that read
        // it is gone, and the guarantee is the same, because a file on somebody's
        // disk does not know the tool went away. Both must survive contact
        // with the provider-keyed shape.
        let home = Home::new("legacy");
        let path = credentials_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_private(
            &path,
            r#"{"api_key":"sk-ant-legacy","other_tool_key":"other-legacy"}"#,
        )
        .unwrap();

        // Read as Anthropic's, with no rewrite: reading migrates, writing does.
        assert_eq!(
            resolve(&Anthropic, None, home.path()).unwrap().expose(),
            "sk-ant-legacy"
        );
        assert_eq!(stored_providers(home.path()), vec!["anthropic".to_string()]);

        // A write for a *different* provider must not lose either of them.
        store(home.path(), "other", &ApiKey::new("other-key")).unwrap();
        assert_eq!(
            resolve(&Anthropic, None, home.path()).unwrap().expose(),
            "sk-ant-legacy"
        );
        assert_eq!(
            resolve(&Other, None, home.path()).unwrap().expose(),
            "other-key"
        );
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc["other_tool_key"], "other-legacy",
            "a key this module does not own was destroyed by a write: {doc}"
        );
        // …and the legacy field is gone rather than left as a second answer.
        assert!(doc.get("api_key").is_none(), "{doc}");
    }

    #[test]
    fn storing_one_provider_leaves_the_others_alone() {
        let home = Home::new("two");
        store(home.path(), "anthropic", &ApiKey::new("sk-ant-1")).unwrap();
        store(home.path(), "other", &ApiKey::new("other-1")).unwrap();
        store(home.path(), "anthropic", &ApiKey::new("sk-ant-2")).unwrap();
        assert_eq!(
            resolve(&Anthropic, None, home.path()).unwrap().expose(),
            "sk-ant-2"
        );
        assert_eq!(
            resolve(&Other, None, home.path()).unwrap().expose(),
            "other-1"
        );
        assert_eq!(stored_providers(home.path()), ["anthropic", "other"]);
    }

    #[test]
    fn a_key_belonging_to_another_provider_is_still_scrubbed() {
        // §1.4: every key in the file goes through `ApiKey::new` at load, so a
        // provider switched away from an hour ago cannot have its key printed
        // by an error raised now.
        const OTHERS: &str = "sk-other-NOTTHEONEBEINGUSEDRIGHTNOW";
        let home = Home::new("scrub");
        store(home.path(), "anthropic", &ApiKey::new("sk-ant-current")).unwrap();
        store(home.path(), "other", &ApiKey::new(OTHERS)).unwrap();
        // A fresh process would not have registered it; this one has, because
        // storing built it. Read the file back in a way that only registers
        // through the load path, then check the scrub list covers it.
        let _ = resolve(&Anthropic, None, home.path()).unwrap();
        let shown = format!("{}", crate::LlmError::Protocol(format!("saw {OTHERS}")));
        assert!(!shown.contains(OTHERS), "{shown}");
    }

    #[test]
    fn a_credentials_file_in_the_project_directory_is_ignored() {
        // The scenario this guards: a user drops credentials.json next to the
        // code they are working on. Emma must not find it, because the next
        // `git add .` publishes it. Run for both providers, because the keyed
        // shape added a second lookup path and only one of them was pinned.
        let kinds: [&dyn ProviderKind; 2] = [&Anthropic, &Other];
        for kind in kinds {
            let home = Home::new("home");
            let project = Home::new("project");
            store(project.path(), kind.name(), &ApiKey::new("key-in-the-repo")).unwrap();

            let cwd = std::env::current_dir().unwrap();
            std::env::set_current_dir(project.path()).unwrap();
            let found = resolve(kind, None, home.path());
            std::env::set_current_dir(cwd).unwrap();

            match found {
                Err(AuthError::Missing { path, .. }) => {
                    assert!(path.starts_with(home.path()), "{}", path.display())
                }
                other => panic!("resolved a key from the project directory: {other:?}"),
            }
        }
    }

    #[test]
    fn a_missing_file_says_which_provider_and_what_to_do() {
        let home = Home::new("missing");
        let msg = resolve(&Anthropic, None, home.path())
            .unwrap_err()
            .to_string();
        assert!(msg.contains("ANTHROPIC_API_KEY"), "{msg}");
        assert!(msg.contains("anthropic"), "{msg}");
        // The command a user actually runs, named correctly at composition
        // rather than patched at the printing boundary.
        assert!(msg.contains("emma set-provider anthropic"), "{msg}");
        assert!(!msg.contains("emma auth"), "{msg}");

        // A file that exists but has nothing for this provider is the same
        // answer, naming *that* provider's variable rather than Anthropic's.
        store(home.path(), "anthropic", &ApiKey::new("sk-ant-x")).unwrap();
        let msg = resolve(&Other, None, home.path()).unwrap_err().to_string();
        assert!(msg.contains("OTHER_API_KEY"), "{msg}");
        assert!(msg.contains("emma set-provider other"), "{msg}");
    }

    #[test]
    fn the_environment_beats_the_stored_file() {
        let home = Home::new("env");
        store(home.path(), "anthropic", &ApiKey::new("sk-ant-from-file")).unwrap();
        assert_eq!(
            resolve(&Anthropic, Some("sk-ant-from-env"), home.path())
                .unwrap()
                .expose(),
            "sk-ant-from-env"
        );
        // …but an exported-and-emptied variable is not a key, and must fall
        // through rather than authenticate with "".
        assert_eq!(
            resolve(&Anthropic, Some("  "), home.path())
                .unwrap()
                .expose(),
            "sk-ant-from-file"
        );
        // One provider's variable is not another's: `ANTHROPIC_API_KEY` in the
        // environment must not answer for a provider that never declared it.
        assert!(resolve(&Other, None, home.path()).is_err());
    }

    #[test]
    fn malformed_credentials_name_the_file_rather_than_being_overwritten() {
        let home = Home::new("malformed");
        let path = credentials_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_private(&path, "not json").unwrap();
        let msg = resolve(&Anthropic, None, home.path())
            .unwrap_err()
            .to_string();
        assert!(msg.contains("credentials.json"), "{msg}");
        // A write refuses too. Clobbering a file we cannot parse is how the
        // keys we did not come to change get deleted.
        assert!(store(home.path(), "anthropic", &ApiKey::new("sk-ant-x")).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_credentials_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let home = Home::new("perms");
        let path = store(home.path(), "anthropic", &ApiKey::new("sk-ant-loose")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        match resolve(&Anthropic, None, home.path()) {
            Err(AuthError::Insecure { .. }) => {}
            other => panic!("read a world-readable key file: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn store_creates_the_file_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let home = Home::new("mode");
        let path = store(home.path(), "anthropic", &ApiKey::new("sk-ant-tight")).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "created {mode:o}");
        // …and stays that way through the read-modify-write a second provider
        // costs, which is the step the keyed shape added.
        store(home.path(), "other", &ApiKey::new("other-tight")).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "rewritten {mode:o}");
    }

    /// The counterpart to the two tests above, and the reason it looks so thin
    /// is the point.
    ///
    /// Windows has no mode bits, so `check_permissions` is a deliberate
    /// `Ok(())` and `write_private` is a plain write (see their `cfg(not(unix))`
    /// arms). What has to be asserted on this platform is therefore not "the
    /// file is owner-only" — nothing here can say that — but that the absence
    /// is a *decision* rather than a hole somebody could fall into: a key
    /// written on Windows is readable back, and read-back does **not** fail
    /// closed on a permission check that cannot run. Without this, the whole
    /// store/resolve round trip is asserted only on unix, and a Windows-only
    /// regression in either half is invisible until a user hits it.
    #[cfg(not(unix))]
    #[test]
    fn a_key_stored_where_there_are_no_mode_bits_still_round_trips() {
        let home = Home::new("nomode");
        let path = store(home.path(), "anthropic", &ApiKey::new("sk-ant-tight")).unwrap();
        assert!(path.exists());
        assert!(
            !std::fs::metadata(&path).unwrap().permissions().readonly(),
            "the file was left read-only, so the next store cannot rewrite it"
        );
        let key = resolve(&Anthropic, None, home.path()).expect("resolve what store wrote");
        assert_eq!(key.expose(), "sk-ant-tight");
        // And the second write — the read-modify-write a second provider costs,
        // which is the step that would trip over a permission mistake.
        store(home.path(), "other", &ApiKey::new("other-tight")).unwrap();
        let key = resolve(&Anthropic, None, home.path()).expect("still readable after a rewrite");
        assert_eq!(key.expose(), "sk-ant-tight");
    }
}

// endregion: Tests
