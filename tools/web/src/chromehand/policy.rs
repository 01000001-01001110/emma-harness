//! Policy: which URLs the binary will touch at all (ADR-4).
//!
//! - localhost / loopback / file / chrome-internal URLs are refused always
//!   (`--allow-local` exists solely for offline fixture tests and only lifts
//!   the loopback refusal — never the scheme refusal).
//! - If a domain allowlist is in force (explicit `--allowlist <path>`, or
//!   `data/browser-allowlist.json` present in the working directory), a
//!   non-allowlisted domain is refused with the file named in the message.
//!   No allowlist file → allowlist-off for the read verbs P1.5 ships.
//!
//! A refusal is exit 2 for single-URL commands; in batch mode it is a
//! per-URL result so one bad URL never sinks the run.

use std::path::PathBuf;

pub const DEFAULT_ALLOWLIST_PATH: &str = "data/browser-allowlist.json";

pub struct Policy {
    /// (path it was loaded from, lowercased domains)
    pub allowlist: Option<(PathBuf, Vec<String>)>,
    pub allow_local: bool,
}

impl Policy {
    /// `explicit` = --allowlist path (must exist and parse); otherwise the
    /// default path is picked up only if present.
    pub fn load(explicit: Option<&str>, allow_local: bool) -> Result<Self, String> {
        let path = match explicit {
            Some(p) => Some(PathBuf::from(p)),
            None => {
                let p = PathBuf::from(DEFAULT_ALLOWLIST_PATH);
                if p.exists() {
                    Some(p)
                } else {
                    None
                }
            }
        };
        let allowlist = match path {
            None => None,
            Some(p) => {
                let raw = std::fs::read_to_string(&p)
                    .map_err(|e| format!("allowlist {}: {}", p.display(), e))?;
                let v: serde_json::Value = serde_json::from_str(&raw)
                    .map_err(|e| format!("allowlist {}: invalid JSON: {}", p.display(), e))?;
                let domains: Vec<String> = v
                    .get("domains")
                    .and_then(|d| d.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str())
                            .map(|s| s.trim().trim_start_matches('.').to_lowercase())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .ok_or_else(|| {
                        format!(
                            "allowlist {}: expected {{\"domains\": [\"…\"]}}",
                            p.display()
                        )
                    })?;
                Some((p, domains))
            }
        };
        Ok(Policy {
            allowlist,
            allow_local,
        })
    }

    /// Ok(()) = may proceed. Err(reason) = refuse (exit 2 / per-URL refusal).
    pub fn check(&self, raw_url: &str) -> Result<(), String> {
        let parsed =
            url::Url::parse(raw_url).map_err(|e| format!("refused: unparseable URL: {}", e))?;
        match parsed.scheme() {
            "http" | "https" => {}
            s => return Err(format!("refused: scheme '{}' — only http/https", s)),
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| "refused: URL has no host".to_string())?
            .to_lowercase();

        let is_local = match parsed.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_unspecified(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unspecified(),
            _ => host == "localhost" || host.ends_with(".localhost"),
        };
        if is_local && !self.allow_local {
            return Err(format!(
                "refused: local/loopback URL ({}); --allow-local exists for offline fixture tests only",
                host
            ));
        }

        self.check_allowlist(&host)
    }

    /// Interaction verbs (click/type/select) REQUIRE an allowlist (ADR-4):
    /// reading the web is ordinary; acting on it is opt-in.
    pub fn check_interaction(&self, url: &str) -> Result<(), String> {
        if self.allowlist.is_none() {
            return Err(format!(
                "refused: interaction verbs require a user-owned domain allowlist — create {} ({{\"domains\": [\"example.com\"]}}) or pass --allowlist <path>",
                DEFAULT_ALLOWLIST_PATH
            ));
        }
        self.check(url)
    }

    fn check_allowlist(&self, host: &str) -> Result<(), String> {
        if let Some((path, domains)) = &self.allowlist {
            let allowed = domains
                .iter()
                .any(|d| host == *d || host.ends_with(&format!(".{}", d)));
            if !allowed {
                return Err(format!(
                    "refused: domain '{}' not in allowlist {} — add it there to opt in",
                    host,
                    path.display()
                ));
            }
        }
        Ok(())
    }
}
