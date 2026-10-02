//! Display names, keyed by class name.
//!
//! Names are split out from the roster because the two have different availability. The
//! id table covers roughly 57 heroes; the installed game localises roughly 88, in any of
//! [`crate::LANGUAGES`]. Keying names by class name rather than storing them on the
//! roster row means a name is reachable whether or not an id is known for it, so there
//! is no second "names we could not link" table to keep in step.

use std::collections::HashMap;

/// A `class_name -> display name` table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Names {
    /// Language of the most recent contribution, when one was recorded.
    #[cfg_attr(feature = "serde", serde(default))]
    language: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    map: HashMap<String, String>,
}

impl Names {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// The language of the most recent contribution.
    ///
    /// A table layered from several sources reports the last one merged, which is the
    /// language a caller will actually see for most entries.
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Record the language of the entries most recently merged.
    pub fn set_language(&mut self, language: impl Into<String>) {
        self.language = Some(language.into());
    }

    /// Look up a display name by class name.
    pub fn get(&self, class_name: &str) -> Option<&str> {
        self.map.get(class_name).map(String::as_str)
    }

    /// Whether a class name has a display name.
    pub fn contains(&self, class_name: &str) -> bool {
        self.map.contains_key(class_name)
    }

    /// Insert or replace one name.
    ///
    /// Returns whether this changed anything, so callers can count real replacements
    /// rather than re-writes of an identical string.
    pub fn insert(&mut self, class_name: impl Into<String>, name: impl Into<String>) -> bool {
        let class_name = class_name.into();
        let name = name.into();
        match self.map.get(&class_name) {
            Some(existing) if *existing == name => false,
            _ => {
                self.map.insert(class_name, name);
                true
            }
        }
    }

    /// Overlay another table, replacing entries that differ and adding unknown ones.
    ///
    /// Returns how many entries changed.
    pub fn merge(&mut self, other: &Names) -> usize {
        let mut changed = 0;
        for (class_name, name) in &other.map {
            if self.insert(class_name.clone(), name.clone()) {
                changed += 1;
            }
        }
        if let Some(lang) = &other.language {
            self.language = Some(lang.clone());
        }
        changed
    }

    /// Every `(class_name, display name)` pair, in arbitrary order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.map.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// How many names are known.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_reports_only_real_changes() {
        let mut n = Names::new();
        assert!(n.insert("hero_inferno", "Infernus"));
        assert!(!n.insert("hero_inferno", "Infernus"));
        assert!(n.insert("hero_inferno", "Infernus PTR"));
        assert_eq!(n.get("hero_inferno"), Some("Infernus PTR"));
        assert_eq!(n.len(), 1);
    }

    #[test]
    fn merge_counts_changes_and_adopts_language() {
        let mut base = Names::new();
        base.insert("hero_inferno", "Infernus");
        base.insert("hero_gigawatt", "Seven");
        base.set_language("english");

        let mut over = Names::new();
        over.insert("hero_inferno", "Infernus");
        over.insert("hero_gigawatt", "Sieben");
        over.insert("hero_astro", "Holliday");
        over.set_language("german");

        assert_eq!(base.merge(&over), 2);
        assert_eq!(base.get("hero_gigawatt"), Some("Sieben"));
        assert_eq!(base.get("hero_astro"), Some("Holliday"));
        assert_eq!(base.language(), Some("german"));
    }

    #[test]
    fn unknown_class_names_have_no_name() {
        let n = Names::new();
        assert_eq!(n.get("hero_nobody"), None);
        assert!(!n.contains("hero_nobody"));
        assert!(n.is_empty());
    }
}
