//! Badge art for the two rank ladders the overlay shows, one URL per era.
//!
//! Deadlock's rank badges were redrawn when Valve shipped ranked matchmaking, and the two
//! sets are visibly nothing alike. That is load-bearing here: **the era of the artwork is
//! the source label.** Old-era art means Statlocker's own pre-matchmaking PP rating,
//! new-era art means the game's own badge out of memory, and a row showing both needs no
//! caption to say which is which. Serving one era's art for the other rating would
//! therefore be an outright lie about where the number came from, so neither function
//! here ever falls back to the other set.
//!
//! # How the two eras were told apart
//!
//! Both sets live on deadlock-api.com, and nothing there labels them by era, so this was
//! settled by looking at the pictures on 2026-08-21:
//!
//! - `assets-api-res/images/ranks/rank3/badge_lg_subrank1.png` is an alchemy flask in an
//!   ornate frame - **Alchemist**, which is what tier 3 was called before the update.
//! - `api.deadlock-api.com/v1/assets/ranks/3/1/image` is a rat on a stone plaque -
//!   **Acolyte**, which is what tier 3 is called now.
//!
//! The current names on the Deadlock wiki (`Acolyte`, `Sentinel`, `Mystic`) match the
//! second set, and its Phantom badge is the one the wiki shows today.
//!
//! deadlock-api's own `/v1/assets/ranks?client_version=...` corroborates the naming
//! split, since an old build id returns `Alchemist`, `Arcanist` and `Archon`. It is
//! **not** an era switch for the art, though: the image URLs it returns are identical at
//! every version, and requesting the image endpoint with an old `client_version` returns
//! the same bytes. That was checked, and is why it is not used here.
//!
//! # Sizes
//!
//! Both builders return the smallest published art, because these are painted as row
//! icons. The old-era set publishes a real `sm` variant (about 9KB). The new-era endpoint
//! publishes **one size only** - deadlock-api's asset listing gives the same URL for
//! `small_subrank1` and `large_subrank1` - so that one arrives large and the window has
//! to scale it down.

use deadlock_core::RankBadge;

/// Host serving the old-era badge art.
pub const OLD_ERA_HOST: &str = "https://assets-bucket.deadlock-api.com";

/// Host serving the new-era badge art.
///
/// Different host from [`OLD_ERA_HOST`], and both have to be in the window's `img-src`.
pub const NEW_ERA_HOST: &str = "https://api.deadlock-api.com";

/// Highest tier on either ladder. Tier 12 is a 404 on both, which is how this was fixed.
const MAX_TIER: u32 = 11;

/// Highest subrank within a tier.
const MAX_SUBRANK: u32 = 6;

/// Whether a badge names a real step on the ladder.
///
/// Unranked is not a step: [`RankBadge`] treats zero as "no rank assigned", so it gets no
/// artwork rather than tier zero's.
fn on_the_ladder(badge: RankBadge) -> bool {
    badge.is_ranked()
        && (1..=MAX_TIER).contains(&badge.tier())
        && (1..=MAX_SUBRANK).contains(&badge.subrank())
}

/// Old-era badge art, for Statlocker's PP rating.
///
/// `None` for unranked and for any badge off the ladder - the window must draw a
/// deliberate placeholder for that, not the other era's art and not a blank.
///
/// Every tier 1-11 and subrank 1-6 was confirmed to return 200 on 2026-08-21.
pub fn old_era_art(badge: RankBadge) -> Option<String> {
    if !on_the_ladder(badge) {
        return None;
    }
    Some(format!(
        "{OLD_ERA_HOST}/assets-api-res/images/ranks/rank{}/badge_sm_subrank{}.webp",
        badge.tier(),
        badge.subrank()
    ))
}

/// New-era badge art, for the game's own rank badge.
///
/// `None` for unranked and for any badge off the ladder, for the same reason as
/// [`old_era_art`].
///
/// Served large: this endpoint publishes one size, so the window scales it.
pub fn new_era_art(badge: RankBadge) -> Option<String> {
    if !on_the_ladder(badge) {
        return None;
    }
    Some(format!(
        "{NEW_ERA_HOST}/v1/assets/ranks/{}/{}/image?format=webp",
        badge.tier(),
        badge.subrank()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two eras are two different URLs, and neither is ever the other.
    ///
    /// This is the whole contract: the artwork is what tells a viewer whether a badge is
    /// their Valve rank or Statlocker's PP rating, so a builder that quietly returned the
    /// wrong set would mislabel the source with no caption to contradict it.
    #[test]
    fn the_two_eras_never_share_a_url() {
        let badge = RankBadge(93);
        let old = old_era_art(badge).expect("tier 9 subrank 3 is on the ladder");
        let new = new_era_art(badge).expect("tier 9 subrank 3 is on the ladder");
        assert_ne!(old, new);
        assert!(old.starts_with(OLD_ERA_HOST), "{old}");
        assert!(new.starts_with(NEW_ERA_HOST), "{new}");
        assert!(old.contains("/ranks/rank9/badge_sm_subrank3."), "{old}");
        assert!(new.contains("/v1/assets/ranks/9/3/image"), "{new}");
        assert!(!new.contains("badge_sm_subrank"), "{new}");
        assert!(!old.contains("/v1/assets/"), "{old}");
    }

    /// Both ends of both ladders resolve, and one past either end does not.
    #[test]
    fn art_covers_the_whole_ladder_and_stops_at_its_edges() {
        for tier in 1..=11 {
            for subrank in 1..=6 {
                let badge = RankBadge(tier * 10 + subrank);
                assert!(old_era_art(badge).is_some(), "old era missing {badge}");
                assert!(new_era_art(badge).is_some(), "new era missing {badge}");
            }
        }
        assert_eq!(old_era_art(RankBadge(121)), None);
        assert_eq!(new_era_art(RankBadge(121)), None);
        assert_eq!(old_era_art(RankBadge(90)), None);
        assert_eq!(new_era_art(RankBadge(97)), None);
    }

    /// Unranked gets no artwork, and it is not tier zero's.
    ///
    /// `RankBadge(0)` means no rank assigned. Drawing Obscurus for it would report a rank
    /// the player does not hold, and drawing nothing at all would be indistinguishable
    /// from art that failed to load - which is why this is `None` and the view keeps a
    /// separate state saying *why* there is no URL.
    #[test]
    fn unranked_has_no_badge_art() {
        assert_eq!(old_era_art(RankBadge(0)), None);
        assert_eq!(new_era_art(RankBadge(0)), None);
    }

    /// The row icons come from the small set where a small set exists.
    #[test]
    fn the_old_era_art_is_the_small_variant() {
        let url = old_era_art(RankBadge(11)).expect("initiate 1 is on the ladder");
        assert!(
            url.contains("badge_sm_"),
            "row icons want the small art: {url}"
        );
        assert!(!url.contains("badge_lg_"), "{url}");
    }

    /// Every host these URLs name is one the window is allowed to load images from.
    ///
    /// `tauri.conf.json` is the only thing standing between a correct URL and a blank
    /// badge, and the two drift silently: the fetch fails in the webview console, not in
    /// any build.
    #[test]
    fn every_art_host_is_in_the_windows_image_policy() {
        let conf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json");
        let text = std::fs::read_to_string(&conf)
            .unwrap_or_else(|e| panic!("read {}: {e}", conf.display()));
        let csp = text
            .split("\"csp\":")
            .nth(1)
            .expect("a csp in tauri.conf.json");
        let img_src = csp
            .split("img-src")
            .nth(1)
            .expect("an img-src directive")
            .split(';')
            .next()
            .expect("a terminated img-src directive");
        for host in [OLD_ERA_HOST, NEW_ERA_HOST] {
            assert!(
                img_src.contains(host),
                "img-src does not allow {host}: {img_src}"
            );
        }
        assert!(
            !img_src.contains('*'),
            "the image policy must name hosts, not wildcards: {img_src}"
        );
    }
}
