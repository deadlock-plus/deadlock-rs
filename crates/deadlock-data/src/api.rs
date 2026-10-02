//! deadlock-api.com client.
//!
//! Community-run service that publishes the game's asset tables, including the hero list
//! with ids, class names and localised display names in one response - the same data the
//! game keeps split between its VPK archives and its localisation files.
//!
//! Enabled by the `online` feature. Everything here is blocking; call it from a
//! background thread or a blocking task.
//!
//! Nothing in this crate requires the network: [`crate::HeroCatalog::bundled`] and
//! [`crate::HeroCatalog::from_game_dir`] both work offline, and this is a refresh path
//! rather than a dependency.

use crate::error::{Error, Result};
use crate::heroes::HeroCatalog;
use crate::items::ItemCatalog;

/// Base URL of the asset API.
pub const ASSETS_BASE: &str = "https://api.deadlock-api.com/v1/assets";

/// How long to wait for a response.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Fetch the hero catalog for a language.
///
/// ```no_run
/// let heroes = deadlock_data::api::fetch_heroes("english")?;
/// # Ok::<(), deadlock_data::Error>(())
/// ```
pub fn fetch_heroes(language: &str) -> Result<HeroCatalog> {
    let url = format!("{ASSETS_BASE}/heroes?language={}", encode(language));
    let body = get(&url)?;
    HeroCatalog::from_json(&body)
}

/// Fetch the hero catalog for a specific client build.
///
/// Pass the build id the reader reports so the names match the running client rather
/// than whatever is current.
pub fn fetch_heroes_for_build(language: &str, client_version: u32) -> Result<HeroCatalog> {
    let url = format!(
        "{ASSETS_BASE}/heroes?language={}&client_version={client_version}",
        encode(language)
    );
    let body = get(&url)?;
    HeroCatalog::from_json(&body)
}

/// Fetch the item catalog - abilities, upgrades and weapons - for a language.
///
/// ```no_run
/// let items = deadlock_data::api::fetch_items("english")?;
/// # Ok::<(), deadlock_data::Error>(())
/// ```
pub fn fetch_items(language: &str) -> Result<ItemCatalog> {
    let url = format!("{ASSETS_BASE}/items?language={}", encode(language));
    ItemCatalog::from_json(&get(&url)?)
}

/// Fetch the item catalog for a specific client build.
pub fn fetch_items_for_build(language: &str, client_version: u32) -> Result<ItemCatalog> {
    let url = format!(
        "{ASSETS_BASE}/items?language={}&client_version={client_version}",
        encode(language)
    );
    ItemCatalog::from_json(&get(&url)?)
}

/// One agent for the process, so requests share a connection pool.
///
/// Building an agent sets up a TLS stack; doing it per request meant the heroes and items
/// fetches shared nothing and paid a fresh handshake each.
fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .user_agent(concat!("deadlock-data/", env!("CARGO_PKG_VERSION")))
            .build()
    })
}

/// Percent-encode a query-string value.
///
/// `language` reaches these URLs from a public function, and interpolating it raw let a
/// value containing `&`, `#`, `?` or `/` rewrite the query or the path. In-tree callers
/// only ever pass literals, which is exactly why this would have gone unnoticed.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn get(url: &str) -> Result<String> {
    let response = agent().get(url).call().map_err(|e| match e {
        // A status the server actually returned. Kept as a number so a caller can tell a
        // rate limit from a not-found, which one flattened string could not.
        ureq::Error::Status(code, _) => Error::Http {
            status: Some(code),
            source: format!("HTTP {code}"),
        },
        // No response: DNS, connect, TLS or timeout.
        other => Error::Http {
            status: None,
            source: other.to_string(),
        },
    })?;
    response.into_string().map_err(|e| Error::Http {
        status: None,
        source: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `language` is a public parameter interpolated straight into a URL. In-tree callers
    /// pass literals, so a value that rewrites the query would never have shown up here.
    #[test]
    fn a_hostile_language_cannot_rewrite_the_url() {
        assert_eq!(encode("english"), "english");
        assert_eq!(encode("zh-Hans"), "zh-Hans");

        assert_eq!(encode("a&client_version=1"), "a%26client_version%3D1");
        assert_eq!(encode("../items"), "..%2Fitems");
        assert_eq!(encode("a#frag"), "a%23frag");
        assert_eq!(encode("a?b"), "a%3Fb");
        assert_eq!(encode("a b"), "a%20b");

        assert_eq!(encode("A-Z_a-z.0~9"), "A-Z_a-z.0~9");
    }

    /// The point of keeping the status: a caller can tell a rate limit from a not-found
    /// and decide whether another attempt is worth making.
    #[test]
    fn retryable_failures_are_distinguishable() {
        let http = |status| Error::Http {
            status,
            source: "x".into(),
        };
        assert!(http(Some(429)).is_retryable(), "rate limited");
        assert!(http(Some(503)).is_retryable(), "server side");
        assert!(http(Some(408)).is_retryable(), "request timeout");
        assert!(http(None).is_retryable(), "never got a response");

        assert!(!http(Some(404)).is_retryable(), "it will still be missing");
        assert!(!http(Some(400)).is_retryable());
        assert!(!Error::GameNotFound.is_retryable());

        assert_eq!(http(Some(429)).status(), Some(429));
        assert_eq!(Error::GameNotFound.status(), None);
    }

    #[test]
    fn urls_are_well_formed() {
        assert!(ASSETS_BASE.starts_with("https://"));
        assert!(!ASSETS_BASE.contains("assets.deadlock-api.com"));
    }
}
