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
//! The file holds exactly one key, under the name `api_key`, because Anthropic
//! is the only provider. A second provider needs this keyed by provider; the
//! shape here is not it.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

pub const ENV_VAR: &str = "ANTHROPIC_API_KEY";

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
    // wrong string: the command is `emma api`, so the string says `emma api`
    // and the rewrite is now a no-op that can be deleted.
    #[error("no API key found. Set {ENV_VAR}, or run `emma api` to store one at {path}")]
    Missing { path: PathBuf },

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

#[derive(Debug, Serialize, Deserialize)]
struct Credentials {
    api_key: String,
}

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

/// Resolve a key: environment first, then the stored file.
///
/// Takes the environment value rather than reading it, so the decision is a
/// pure function of its two inputs — a test can pin either source without
/// mutating process state that every other test shares.
///
/// The environment wins because that is how CI and one-off overrides work, and
/// because a user who exports a key expects it to be the one used.
pub fn resolve(env_key: Option<&str>, home: &Path) -> Result<ApiKey, AuthError> {
    match from_env(env_key) {
        Some(key) => Ok(key),
        None => load_file(&credentials_path(home)),
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
pub fn load_default() -> Result<ApiKey, AuthError> {
    let env_key = std::env::var(ENV_VAR).ok();
    if let Some(key) = from_env(env_key.as_deref()) {
        return Ok(key);
    }
    resolve(None, &home_dir().ok_or(AuthError::NoHome)?)
}

fn load_file(path: &Path) -> Result<ApiKey, AuthError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AuthError::Missing {
                path: path.to_path_buf(),
            })
        }
        Err(source) => {
            return Err(AuthError::Unreadable {
                path: path.to_path_buf(),
                source,
            })
        }
    };

    check_permissions(path)?;

    let creds: Credentials = serde_json::from_str(&raw).map_err(|source| AuthError::Malformed {
        path: path.to_path_buf(),
        source,
    })?;
    if creds.api_key.trim().is_empty() {
        return Err(AuthError::Missing {
            path: path.to_path_buf(),
        });
    }
    Ok(ApiKey::new(creds.api_key))
}

// endregion: Where the key comes from

// region: Owner-only on disk
// ---------------------------------------------------------------------------
// Owner-only on disk
//
// Writing and checking the permission bits, each with a Unix implementation
// and a Windows one that says plainly what it does not do.
// ---------------------------------------------------------------------------

/// Write the credentials file with owner-only permissions, creating the
/// directory if needed. This is what `emma api` calls; the command itself
/// lives in `crates/emma/src/commands.rs`, which is also where the key is read
/// from a pipe or a no-echo prompt rather than an argument.
pub fn store(home: &Path, key: &ApiKey) -> Result<PathBuf, AuthError> {
    let path = credentials_path(home);
    let dir = path.parent().expect("credentials path always has a parent");
    std::fs::create_dir_all(dir).map_err(|source| AuthError::Unwritable {
        path: dir.to_path_buf(),
        source,
    })?;

    let body = serde_json::json!({ "api_key": key.expose() }).to_string();
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
        let path = store(home.path(), &ApiKey::new("sk-ant-stored")).unwrap();
        assert_eq!(path, credentials_path(home.path()));
        assert_eq!(load_file(&path).unwrap().expose(), "sk-ant-stored");
    }

    #[test]
    fn a_credentials_file_in_the_project_directory_is_ignored() {
        // The scenario this guards: a user drops credentials.json next to the
        // code they are working on. Emma must not find it, because the next
        // `git add .` publishes it.
        let home = Home::new("home");
        let project = Home::new("project");
        store(project.path(), &ApiKey::new("sk-ant-in-the-repo")).unwrap();

        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(project.path()).unwrap();
        let found = resolve(None, home.path());
        std::env::set_current_dir(cwd).unwrap();

        match found {
            Err(AuthError::Missing { path }) => {
                assert!(path.starts_with(home.path()), "{}", path.display())
            }
            other => panic!("resolved a key from the project directory: {other:?}"),
        }
    }

    #[test]
    fn a_missing_file_says_what_to_do() {
        let home = Home::new("missing");
        let msg = resolve(None, home.path()).unwrap_err().to_string();
        assert!(msg.contains("ANTHROPIC_API_KEY"), "{msg}");
        // The command a user actually runs, named correctly at composition
        // rather than patched at the printing boundary.
        assert!(msg.contains("emma api"), "{msg}");
        assert!(!msg.contains("emma auth"), "{msg}");
    }

    #[test]
    fn the_environment_beats_the_stored_file() {
        let home = Home::new("env");
        store(home.path(), &ApiKey::new("sk-ant-from-file")).unwrap();
        assert_eq!(
            resolve(Some("sk-ant-from-env"), home.path())
                .unwrap()
                .expose(),
            "sk-ant-from-env"
        );
        // …but an exported-and-emptied variable is not a key, and must fall
        // through rather than authenticate with "".
        assert_eq!(
            resolve(Some("  "), home.path()).unwrap().expose(),
            "sk-ant-from-file"
        );
    }

    #[test]
    fn malformed_credentials_name_the_file() {
        let home = Home::new("malformed");
        let path = credentials_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let msg = resolve(None, home.path()).unwrap_err().to_string();
        assert!(msg.contains("credentials.json"), "{msg}");
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_credentials_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let home = Home::new("perms");
        let path = store(home.path(), &ApiKey::new("sk-ant-loose")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        match resolve(None, home.path()) {
            Err(AuthError::Insecure { .. }) => {}
            other => panic!("read a world-readable key file: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn store_creates_the_file_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let home = Home::new("mode");
        let path = store(home.path(), &ApiKey::new("sk-ant-tight")).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "created {mode:o}");
    }
}

// endregion: Tests
