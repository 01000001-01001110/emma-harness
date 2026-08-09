//! Where the Brave key may come from — and the one place it may not.
//!
//! Its own test binary because one of these changes the process's working
//! directory, and a `set_current_dir` racing other tests in the same binary is
//! a flake nobody enjoys diagnosing.

use emma_tools_web::credentials;

/// A scratch directory standing in for `$HOME`, removed on drop.
struct Dir(std::path::PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "emma-brave-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }

    /// Write `~/.emma/credentials.json` with owner-only permissions where the
    /// platform has them.
    fn store(&self, body: &str) -> std::path::PathBuf {
        let path = self.0.join(".emma").join("credentials.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const STORED: &str = r#"{ "api_key": "sk-ant-x", "brave_search_api_key": "BSA-from-file" }"#;

#[test]
fn the_key_is_read_from_the_home_credentials_file() {
    let home = Dir::new("home");
    home.store(STORED);
    let key = credentials::resolve(None, home.path()).expect("no key found");
    assert_eq!(key.expose(), "BSA-from-file");
}

#[test]
fn the_environment_beats_the_stored_file() {
    let home = Dir::new("env");
    home.store(STORED);
    assert_eq!(
        credentials::resolve(Some("BSA-from-env"), home.path())
            .unwrap()
            .expose(),
        "BSA-from-env"
    );
    // …but an exported-and-emptied variable is not a key. It must fall through
    // rather than authenticate with "".
    assert_eq!(
        credentials::resolve(Some("   "), home.path())
            .unwrap()
            .expose(),
        "BSA-from-file"
    );
}

#[test]
fn a_file_without_the_brave_field_yields_no_key() {
    // The Anthropic key living in the same file must not be handed to Brave.
    let home = Dir::new("anthropic-only");
    home.store(r#"{ "api_key": "sk-ant-x" }"#);
    assert!(credentials::resolve(None, home.path()).is_none());
    assert!(credentials::missing_message().contains("BRAVE_SEARCH_API_KEY"));
}

#[test]
fn a_credentials_file_in_the_project_directory_is_ignored() {
    // The scenario: a user drops credentials.json next to the code they are
    // working on. Emma must not find it, because the next `git add .`
    // publishes it. The home path is computed, never searched upward, so this
    // holds no matter where the process is standing.
    let home = Dir::new("clean-home");
    let project = Dir::new("project");
    project.store(r#"{ "brave_search_api_key": "BSA-in-the-repo" }"#);

    let cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(project.path()).unwrap();
    let found = credentials::resolve(None, home.path());
    std::env::set_current_dir(cwd).unwrap();

    assert!(
        found.is_none(),
        "a key was resolved out of the project directory"
    );
}
