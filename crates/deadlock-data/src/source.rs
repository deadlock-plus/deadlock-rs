//! Which source answered, per facet.
//!
//! A catalog is assembled from up to three sources with different availability, and the
//! answer to "where did this name come from" is not the same as "where did this id come
//! from". [`Provenance`] records one [`Source`] per facet so a caller can tell a
//! localised name read from the running build apart from a vendored snapshot that may be
//! several patches stale.
//!
//! Layering overwrites, so each field names the *last* source that contributed to that
//! facet rather than every source that touched it.

/// Where a facet's data came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Source {
    /// The snapshot vendored into this crate at build time.
    Bundled,
    /// deadlock-api.com.
    Api,
    /// The installed game's own files.
    Client,
}

impl Source {
    /// A short lowercase label, for logs and status lines.
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Bundled => "bundled",
            Source::Api => "api",
            Source::Client => "client",
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The source behind each facet of a catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Provenance {
    /// Where ids, class names and flags came from.
    pub roster: Option<Source>,
    /// Where display names came from.
    pub names: Option<Source>,
    /// Where art URLs came from.
    ///
    /// `None` means no art-bearing source was consulted, which is a different statement
    /// from a hero having no published art. Art is a deadlock-api.com construct: the
    /// installed game cannot supply it, so a client-only catalog leaves this `None`
    /// rather than reporting empty art as though it had been looked up.
    pub art: Option<Source>,
}

impl Provenance {
    /// Nothing consulted yet.
    pub const EMPTY: Provenance = Provenance {
        roster: None,
        names: None,
        art: None,
    };

    /// Every facet from one source.
    pub fn all(source: Source) -> Self {
        Provenance {
            roster: Some(source),
            names: Some(source),
            art: Some(source),
        }
    }
}

impl Default for Provenance {
    fn default() -> Self {
        Provenance::EMPTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_stable() {
        assert_eq!(Source::Bundled.to_string(), "bundled");
        assert_eq!(Source::Api.as_str(), "api");
        assert_eq!(Source::Client.as_str(), "client");
    }

    #[test]
    fn empty_provenance_consulted_nothing() {
        let p = Provenance::default();
        assert_eq!(p, Provenance::EMPTY);
        assert!(p.roster.is_none() && p.names.is_none() && p.art.is_none());
    }

    #[test]
    fn all_sets_every_facet() {
        let p = Provenance::all(Source::Client);
        assert_eq!(p.roster, Some(Source::Client));
        assert_eq!(p.names, Some(Source::Client));
        assert_eq!(p.art, Some(Source::Client));
    }
}
