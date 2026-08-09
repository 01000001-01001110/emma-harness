//! The Brave key, and where it must never come from.
//!
//! Same two sources as the Anthropic key and in the same order: the
//! environment, then `~/.emma/credentials.json`. **Never the project
//! directory.** Emma works inside repositories; a key written next to the code
//! is a key that gets committed, and the first person to notice is usually the
//! scanner that finds it in a public push.
//!
//! This module deliberately owns no path logic. [`emma_llm::auth::home_dir`]
//! and [`emma_llm::auth::credentials_path`] compute the location, so there is
//! exactly one answer in the workspace to "where may a secret live" and adding
//! a provider cannot quietly add a second. What is here is the one thing that
//! genuinely differs: the field name in the file.

use std::path::Path;

use emma_llm::auth::{credentials_path, home_dir};
use emma_llm::ApiKey;

pub const ENV_VAR: &str = "BRAVE_SEARCH_API_KEY";

/// The key in `~/.emma/credentials.json`. Alongside `api_key`, not instead of
/// it — one file, one set of permissions, one thing to back up.
pub const FILE_FIELD: &str = "brave_search_api_key";

/// Why there is no key. A string rather than an error enum because every
/// caller does the same thing with it: refuse to register `WebSearch` and say
/// this out loud.
pub fn missing_message() -> String {
    format!(
        "no Brave Search key. Set {ENV_VAR}, or add \"{FILE_FIELD}\" to \
         ~/.emma/credentials.json. Get one at https://brave.com/search/api/"
    )
}

/// Resolve from an explicit environment value and an explicit home.
///
/// Both inputs are passed rather than read, so the decision is a pure function
/// of them and a test can pin either without mutating process state every
/// other test shares.
pub fn resolve(env_key: Option<&str>, home: &Path) -> Option<ApiKey> {
    if let Some(key) = env_key.map(str::trim).filter(|k| !k.is_empty()) {
        return Some(ApiKey::new(key));
    }
    from_file(&credentials_path(home))
}

/// [`resolve`] against the real environment and the real home directory.
pub fn load_default() -> Option<ApiKey> {
    let env_key = std::env::var(ENV_VAR).ok();
    if let Some(key) = env_key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        return Some(ApiKey::new(key));
    }
    from_file(&credentials_path(&home_dir()?))
}

fn from_file(path: &Path) -> Option<ApiKey> {
    let raw = std::fs::read_to_string(path).ok()?;
    if !permissions_ok(path) {
        // Mirrors `emma_llm::auth`'s refusal, which is private to that module.
        // Refusing rather than warning: a warning about a key file other
        // accounts can read is a warning nobody acts on until after the key is
        // gone. Here it degrades to "WebSearch is not registered", which is
        // loud in a different way and equally safe.
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let key = v.get(FILE_FIELD)?.as_str()?.trim();
    (!key.is_empty()).then(|| ApiKey::new(key))
}

#[cfg(unix)]
fn permissions_ok(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(m) => m.permissions().mode() & 0o077 == 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn permissions_ok(_path: &Path) -> bool {
    // Windows has no mode bits. The file lands in the user's profile, whose
    // default ACL already excludes other users.
    true
}
