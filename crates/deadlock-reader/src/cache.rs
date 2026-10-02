//! Skip the entity walk on most ticks.
//!
//! # Why
//!
//! [`crate::Reader::live_snapshot`] walks the whole entity list every call. Live entities
//! are spread across indices 0..~19,500 in 512-slot chunks, so finding the few thousand
//! that exist means reading roughly 2 MB out of the game and iterating every slot.
//!
//! **Measure before you reach for this.** On a spectated ranked match - 4,040 entities,
//! 13 players, release build - the walk is ~250 us of a ~720 us snapshot, and skipping it
//! takes that to ~430 us. Worth having in a hot loop, but a 1.7x saving, not an order of
//! magnitude. (An earlier debug-build measurement suggested 5x; almost all of that gap was
//! unoptimised iteration, not I/O. Benchmark in release.)
//!
//! # How
//!
//! A snapshot only ever reads a few dozen entities - the player controllers and pawns, the
//! game-rules proxy, the team entities, the structures. [`SnapshotCache`] walks once, keeps
//! exactly those, and on later ticks re-reads their fields directly.
//!
//! Pinned addresses are the obvious way to read a stranger's data, so every reuse is
//! checked first: each pinned entity's `CEntityHandle` is re-read and compared. Handles
//! carry a serial that changes when a slot is recycled, so a dead entity replaced by a new
//! one fails the check and forces a full walk. That check is one 4-byte read per pinned
//! entity - tens of small reads, against a walk of the whole identity table.
//!
//! That validation is itself an assumption worth stating: it trusts that a released entity
//! either has its identity record cleared or its slot recycled with a new serial. An entity
//! freed while its identity record is left byte-identical would pass the check with a
//! dangling `instance`. That has not been observed, but it has not been proven either.
//!
//! # What this costs you
//!
//! Roughly 40% of a snapshot's cost, in exchange for the two caveats below. If your poll
//! interval is comfortable without it, prefer [`crate::Reader::live_snapshot`]: it has no
//! staleness window and no pinned addresses.
//!
//! Field values - kills, deaths, souls, levels, items, health - are as fresh as with an
//! uncached snapshot. Every tick reads them directly from the pinned entities.
//!
//! What lags is the **entity list itself**: an objective that spawns between full walks is
//! not seen until the next one. Anything derived from entity presence inherits that -
//! Midboss and Urn presence, the structure list, hideout detection. [`SnapshotCache::new`]
//! refreshes once a second, so that is the staleness bound; pass a different interval to
//! [`SnapshotCache::every`] if it matters. Destruction is *not* affected: a structure that
//! dies fails the handle check and triggers a walk immediately.
//!
//! ```no_run
//! use std::time::Duration;
//! use deadlock_reader::{cache::SnapshotCache, Reader};
//!
//! let reader = Reader::attach()?;
//! let mut cache = SnapshotCache::new();
//! loop {
//!     if let Some(snap) = reader.live_snapshot_cached(&mut cache)? {
//!         println!("{:?}", snap.timers.display());
//!     }
//!     std::thread::sleep(Duration::from_millis(50));
//! }
//! # Ok::<(), deadlock_reader::Error>(())
//! ```

use std::time::{Duration, Instant};

use crate::entity::{Entity, EntitySnapshot, is_structure_class};
use crate::tunables::Tunables;

/// How often the entity list is re-walked by default.
///
/// 100 ms, not something longer: this window is exactly how late a spawning Midboss or Urn
/// can be reported, and those are among the things people build overlays for. A full walk
/// is ~250 us, so refreshing ten times a second still amortises to well under 1% of a tick.
pub const DEFAULT_REFRESH: Duration = Duration::from_millis(100);

/// Classes a live snapshot reads by name, beyond what the tunables already list.
///
/// Anything not pinned is dropped from the pinned set, which is what makes handle
/// validation cheap. A class missing here does not corrupt data (the entity is just
/// absent until the next full walk), but it does mean silently missing information.
const PINNED_CLASSES: &[&str] = &[
    // Players and the camera.
    "CCitadelPlayerController",
    "C_CitadelPlayerPawn",
    "C_CitadelObserverPawn",
    // Match state.
    "C_CitadelGameRulesProxy",
    "C_CitadelTeam",
];

/// The set of class names a snapshot reads, gathered once.
///
/// Replaces a per-entity predicate that ran six linear scans, four of them string
/// comparisons against `Vec<String>`s, for every one of ~5,400 entities on every full
/// walk - ten times a second at the default interval.
///
/// Structure classes stay a predicate: they are matched by prefix rather than by an
/// enumerable list.
struct PinSet<'a> {
    named: std::collections::HashSet<&'a str>,
}

impl<'a> PinSet<'a> {
    fn new(tunables: &'a Tunables) -> Self {
        let mut named: std::collections::HashSet<&str> = PINNED_CLASSES.iter().copied().collect();
        for list in [
            &tunables.midboss_classes,
            &tunables.urn_classes,
            &tunables.hideout_classes,
            &tunables.clock_source_classes,
        ] {
            named.extend(list.iter().map(String::as_str));
        }
        PinSet { named }
    }

    fn contains(&self, class: &str) -> bool {
        self.named.contains(class) || is_structure_class(class)
    }
}

/// Pinned entities and when they were last refreshed.
///
/// Owned by the caller, so two threads polling independently do not fight over one cache.
/// Not `Clone`: two copies would each believe they were current.
#[derive(Debug)]
pub struct SnapshotCache {
    pinned: Option<EntitySnapshot>,
    /// Entity count from the last full walk. The pinned set is a fraction of it, and
    /// reporting the pinned count as `entity_count` would be a quiet lie.
    total: usize,
    walked_at: Option<Instant>,
    refresh: Duration,
    full_walks: u64,
    reuses: u64,
}

impl Default for SnapshotCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotCache {
    /// A cache that re-walks the entity list ten times a second.
    pub fn new() -> Self {
        Self::every(DEFAULT_REFRESH)
    }

    /// A cache that re-walks on the given interval.
    ///
    /// Shorter means fresher entity presence and more time in the walk; `Duration::ZERO`
    /// walks every tick, which is the same as calling [`crate::Reader::live_snapshot`].
    pub fn every(refresh: Duration) -> Self {
        SnapshotCache {
            pinned: None,
            total: 0,
            walked_at: None,
            refresh,
            full_walks: 0,
            reuses: 0,
        }
    }

    /// Force the next tick to re-walk.
    ///
    /// Call this whenever you know the world changed out from under the cache - the game
    /// restarting, or a new match - though a recycled entity handle catches that anyway.
    pub fn invalidate(&mut self) {
        self.pinned = None;
        self.walked_at = None;
    }

    /// Full walks and cache reuses so far, for checking the cache is doing its job.
    ///
    /// A reuse ratio near zero means something is invalidating constantly and you are
    /// paying the validation reads on top of the walk.
    pub fn stats(&self) -> (u64, u64) {
        (self.full_walks, self.reuses)
    }

    /// Entity count from the last full walk.
    pub fn total_entities(&self) -> usize {
        self.total
    }

    /// Whether the pinned set can be reused this tick.
    pub(crate) fn usable(&self, proc: &dyn deadlock_memory::mem::MemoryReader) -> bool {
        let Some(pinned) = &self.pinned else {
            return false;
        };
        // An empty pinned set validates trivially - `all()` over nothing is `true` - so
        // caching one would hold "there is nothing here" for the whole refresh window.
        // A walk that pinned nothing is not a cheap answer, it is a walk that found no
        // players, no teams and no structures, which in a live match means it failed:
        // a truncated chunk-table read gives exactly this. Re-walking is the only way to
        // find out, and it is also what makes a match starting show up promptly.
        if pinned.is_empty() {
            return false;
        }
        // Nothing is ever within a zero-length window, which is what turns the cache
        // off: `every(Duration::ZERO)` re-walks on every tick.
        if !self.walked_at.is_some_and(|t| t.elapsed() < self.refresh) {
            return false;
        }
        pinned.handles_still_valid(proc)
    }

    pub(crate) fn pinned(&self) -> Option<&EntitySnapshot> {
        self.pinned.as_ref()
    }

    pub(crate) fn record_reuse(&mut self) {
        self.reuses += 1;
    }

    /// Keep the entities a snapshot reads and drop the rest.
    pub(crate) fn store(&mut self, full: &EntitySnapshot, tunables: &Tunables) {
        // Built once for the whole walk rather than re-derived per entity.
        let pins = PinSet::new(tunables);
        let kept: Vec<Entity> = full
            .all()
            .iter()
            .filter(|e| pins.contains(e.best_name()))
            .cloned()
            .collect();
        self.total = full.len();
        self.pinned = Some(EntitySnapshot::from_entities(kept, full.layout()));
        self.walked_at = Some(Instant::now());
        self.full_walks += 1;
    }
}

#[cfg(test)]
mod tests {
    /// Convenience for the single-class questions below; production builds the set once
    /// per walk instead.
    fn is_pinned(class: &str, tunables: &Tunables) -> bool {
        PinSet::new(tunables).contains(class)
    }

    use super::*;
    use crate::entity::{Entity, EntityLayout};
    use deadlock_memory::mock::MockMemory;

    const IDENTITY: u64 = 0x4000;
    const HANDLE: u32 = 0x1234;

    /// A snapshot holding one pinned entity, and a backing store that confirms its handle.
    ///
    /// The cache tests used `EntitySnapshot::default()` - an *empty* set - and asserted it
    /// was reusable. That was the bug, not the fixture: an empty set validates trivially
    /// because `all()` over nothing is `true`. They need something real to validate.
    fn one_pinned_entity() -> (EntitySnapshot, MockMemory) {
        let layout = EntityLayout::DEADLOCK;
        let e = Entity {
            index: 1,
            identity: IDENTITY,
            instance: 0x5000,
            handle: HANDLE,
            class_name: "CCitadelPlayerController".into(),
            ..Default::default()
        };
        let mut mem = MockMemory::new(1);
        mem.write_u32(IDENTITY + layout.identity_handle, HANDLE);
        (EntitySnapshot::from_entities(vec![e], layout), mem)
    }

    #[test]
    fn the_pinned_set_covers_every_class_the_snapshot_reads() {
        let t = Tunables::default();
        for class in [
            "CCitadelPlayerController",
            "C_CitadelPlayerPawn",
            "C_CitadelObserverPawn",
            "C_CitadelGameRulesProxy",
            "C_CitadelTeam",
            "C_NPC_MidBoss",
        ] {
            assert!(
                is_pinned(class, &t),
                "{class} would be dropped from the cache"
            );
        }
    }

    #[test]
    fn structures_and_hideout_markers_are_pinned() {
        let t = Tunables::default();
        assert!(is_pinned("C_NPC_Boss_Tier2", &t), "walkers are objectives");
        assert!(
            is_pinned("C_NPC_Boss_Tier3", &t),
            "the patron is an objective"
        );
        assert!(
            is_pinned("C_CitadelTriggerHideout", &t),
            "hideout detection"
        );
    }

    #[allow(clippy::field_reassign_with_default)]
    #[test]
    fn overridden_class_names_stay_pinned() {
        let mut t = Tunables::default();
        t.urn_classes = vec!["C_CitadelItemPickupIdol_V2".to_string()];
        assert!(is_pinned("C_CitadelItemPickupIdol_V2", &t));
        assert!(
            !is_pinned("C_CitadelItemPickupIdol", &t),
            "the stale urn name is no longer pinned"
        );
    }

    #[test]
    fn the_noise_that_makes_the_walk_expensive_is_dropped() {
        let t = Tunables::default();
        for class in [
            "C_NPC_Trooper",
            "C_BarnLight",
            "C_EnvVolumetricFogVolume",
            "C_Citadel_BreakableProp",
            "CCitadelZipLineNode",
        ] {
            assert!(!is_pinned(class, &t), "{class} should not be pinned");
        }
    }

    #[test]
    fn a_fresh_cache_is_not_usable() {
        let c = SnapshotCache::new();
        assert!(c.pinned().is_none());
        assert_eq!(c.stats(), (0, 0));
    }

    #[test]
    fn invalidate_forces_a_walk() {
        let mut c = SnapshotCache::new();
        c.invalidate();
        assert!(c.pinned().is_none());
    }

    #[test]
    fn zero_refresh_means_never_reuse() {
        let (full, mock) = one_pinned_entity();
        let mut c = SnapshotCache::every(Duration::ZERO);
        c.store(&full, &Tunables::default());
        assert!(c.pinned().is_some(), "the walk did populate the cache");
        assert!(!c.usable(&mock), "a zero interval must re-walk every tick");
    }

    #[test]
    fn a_normal_interval_reuses_until_it_lapses() {
        let (full, mock) = one_pinned_entity();
        let mut c = SnapshotCache::every(Duration::from_millis(1));
        c.store(&full, &Tunables::default());
        assert!(
            c.usable(&mock),
            "inside the window the pinned set is reused"
        );
        std::thread::sleep(Duration::from_millis(5));
        assert!(!c.usable(&mock), "past the window it must re-walk");
    }

    #[test]
    fn an_invalidated_cache_is_not_usable() {
        let (full, mock) = one_pinned_entity();
        let mut c = SnapshotCache::new();
        c.store(&full, &Tunables::default());
        assert!(c.usable(&mock));
        c.invalidate();
        assert!(!c.usable(&mock), "invalidate must force a walk");
    }

    /// A walk that pinned nothing is not a cheap answer - in a live match it is a walk
    /// that found no players, no teams and no structures, which is what a truncated
    /// chunk-table read looks like. Caching it held "not in a match" for the whole refresh
    /// window, and the validation that should have caught it passes vacuously: `all()`
    /// over an empty set is `true`.
    #[test]
    fn a_walk_that_pinned_nothing_is_never_reused() {
        let mock = MockMemory::new(1);
        let mut c = SnapshotCache::new();
        c.store(&EntitySnapshot::default(), &Tunables::default());

        assert!(c.pinned().is_some(), "the walk did happen and stored a set");
        assert_eq!(c.pinned().map(EntitySnapshot::len), Some(0));
        assert!(
            !c.usable(&mock),
            "an empty pinned set validates trivially and must not be reused"
        );
    }

    /// The other half of the same guarantee: a *non*-empty set whose handle no longer
    /// matches is a recycled slot, and must also force a walk. Without this the test above
    /// could pass for the wrong reason - by nothing ever being reusable.
    #[test]
    fn a_recycled_handle_forces_a_walk() {
        let (full, mut mock) = one_pinned_entity();
        let mut c = SnapshotCache::new();
        c.store(&full, &Tunables::default());
        assert!(c.usable(&mock), "the fixture starts valid");

        mock.write_u32(
            IDENTITY + EntityLayout::DEADLOCK.identity_handle,
            HANDLE + 1,
        );
        assert!(!c.usable(&mock), "a recycled handle must not be reused");
    }
}
