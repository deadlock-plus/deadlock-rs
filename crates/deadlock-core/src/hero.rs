//! Hero identity and the trait that resolves ids to names.

/// A hero's numeric id, as the game reports it in `PlayerDataGlobal_t::m_nHeroID`.
///
/// Ids are assigned by Valve and are stable across patches; the mapping from id to a
/// name lives in game data, not in the id itself. Resolve one with a
/// [`HeroNames`] implementation - `deadlock-data` provides several.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct HeroId(pub u32);

impl HeroId {
    /// No hero picked. The game uses `0` for an unassigned slot.
    pub const NONE: HeroId = HeroId(0);

    /// Whether a hero has actually been picked.
    pub fn is_some(&self) -> bool {
        self.0 != 0
    }

    /// The raw numeric id.
    pub fn get(&self) -> u32 {
        self.0
    }
}

impl From<u32> for HeroId {
    fn from(v: u32) -> Self {
        HeroId(v)
    }
}

impl From<HeroId> for u32 {
    fn from(v: HeroId) -> Self {
        v.0
    }
}

impl std::fmt::Display for HeroId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Resolves hero ids to names.
///
/// This is the seam that lets a memory reader print "Infernus" without knowing anything
/// about game files, CDNs or localisation. Implement it over whatever source you have;
/// `deadlock-data` ships implementations backed by a vendored snapshot, the installed
/// game's own files, and the deadlock-api.com service.
///
/// ```
/// use deadlock_core::{HeroId, HeroNames};
///
/// struct Tiny;
/// impl HeroNames for Tiny {
///     fn hero_name(&self, id: HeroId) -> Option<&str> {
///         (id == HeroId(1)).then_some("Infernus")
///     }
/// }
///
/// assert_eq!(Tiny.hero_name(HeroId(1)), Some("Infernus"));
/// assert_eq!(Tiny.display_name(HeroId(99)), "hero 99");
/// ```
pub trait HeroNames: Send + Sync {
    /// The display name for a hero, if known.
    fn hero_name(&self, id: HeroId) -> Option<&str>;

    /// The internal class name, e.g. `hero_inferno`, if known.
    ///
    /// Useful for matching against entity class names seen in memory, which embed the
    /// codename (`CCitadel_Ability_Bebop_LaserBeam`).
    fn hero_class_name(&self, _id: HeroId) -> Option<&str> {
        None
    }

    /// The display name, falling back to `hero <id>` so callers always have something
    /// to render.
    fn display_name(&self, id: HeroId) -> String {
        self.hero_name(id)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("hero {}", id.0))
    }
}

impl<T: HeroNames + ?Sized> HeroNames for std::sync::Arc<T> {
    fn hero_name(&self, id: HeroId) -> Option<&str> {
        (**self).hero_name(id)
    }
    fn hero_class_name(&self, id: HeroId) -> Option<&str> {
        (**self).hero_class_name(id)
    }
}

impl<T: HeroNames + ?Sized> HeroNames for Box<T> {
    fn hero_name(&self, id: HeroId) -> Option<&str> {
        (**self).hero_name(id)
    }
    fn hero_class_name(&self, id: HeroId) -> Option<&str> {
        (**self).hero_class_name(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hero_id_basics() {
        assert!(!HeroId::NONE.is_some());
        assert!(HeroId(65).is_some());
        assert_eq!(u32::from(HeroId(65)), 65);
        assert_eq!(HeroId::from(65u32), HeroId(65));
        assert_eq!(HeroId(65).to_string(), "65");
    }

    struct Two;
    impl HeroNames for Two {
        fn hero_name(&self, id: HeroId) -> Option<&str> {
            match id.0 {
                1 => Some("Infernus"),
                15 => Some("Bebop"),
                _ => None,
            }
        }
        fn hero_class_name(&self, id: HeroId) -> Option<&str> {
            (id.0 == 1).then_some("hero_inferno")
        }
    }

    #[test]
    fn display_name_always_renders() {
        assert_eq!(Two.display_name(HeroId(15)), "Bebop");
        assert_eq!(Two.display_name(HeroId(999)), "hero 999");
        assert_eq!(Two.hero_class_name(HeroId(1)), Some("hero_inferno"));
        assert_eq!(Two.hero_class_name(HeroId(15)), None);
    }

    #[test]
    fn works_through_arc_and_box() {
        let a: std::sync::Arc<dyn HeroNames> = std::sync::Arc::new(Two);
        assert_eq!(a.hero_name(HeroId(1)), Some("Infernus"));
        let b: Box<dyn HeroNames> = Box::new(Two);
        assert_eq!(b.display_name(HeroId(15)), "Bebop");
    }
}
