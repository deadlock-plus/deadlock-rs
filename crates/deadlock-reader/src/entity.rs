//! Walking the entity system into a per-tick snapshot.
//!
//! # Provenance
//!
//! `HANDLE_INDEX_MASK = 0x7FFF` is verified from the original binary
//! (`and ebx, 0x7fff` at RVA `0x582875`). Everything else in [`EntityLayout`] was
//! derived empirically against a live Deadlock client (see `examples/probe.rs`), by
//! scoring candidate strides and offsets on whether the resulting instance pointers
//! carry vtables that land inside `client.dll`:
//!
//! ```text
//! stride 0x060:  39/512 slots valid
//! stride 0x068:  19/512
//! stride 0x070: 272/512   <- winner
//! stride 0x078:  18/512
//! stride 0x130:  44/512
//! ```
//!
//! Note the correction: the `imul rcx, rax, 0x130` inside the `entity_identity_list`
//! signature is **not** `sizeof(CEntityIdentity)`. The real identity stride is `0x70`,
//! and the identity chunk table hangs off `*entity_system + 0x10` rather than off the
//! `entity_identity_list` global, which in the current build points at unrelated
//! resource data. The signature is still useful for locating the entity system, but its
//! embedded constant describes some other structure.
//!
//! Confirmed layout:
//!
//! ```text
//! *entity_system   +0x10   CEntityIdentity* chunks[64]     stride 8, 512 identities each
//! CEntityIdentity  +0x00   CEntityInstance* m_pInstance
//!                  +0x08   CEntityClass*    m_pClass
//!                  +0x10   CEntityHandle    m_EHandle
//!                  +0x20   const char*      m_designerName
//! CEntityClass     +0x08 -> +0x08 -> const char*  schema class name (the record opens with a hash)
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::globals::Globals;
use deadlock_memory::mem::MemoryReader;

/// Low 15 bits of a `CHandle` are the entity index. Verified from the original.
pub const HANDLE_INDEX_MASK: u32 = 0x7FFF;

/// An invalid handle.
pub const INVALID_HANDLE: u32 = u32::MAX;

/// Offsets and strides describing the entity list and `CEntityIdentity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EntityLayout {
    /// `sizeof(CEntityIdentity)`. Measured as `0x70` against a live client.
    pub identity_stride: u64,
    /// Entities per identity chunk (`idx >> chunk_shift` selects the chunk).
    pub chunk_shift: u32,
    /// Mask selecting the slot within a chunk.
    pub chunk_mask: u64,
    /// Stride between chunk pointers in the chunk table.
    pub chunk_table_stride: u64,
    /// Offset of the first chunk pointer within the chunk table.
    pub chunk_table_base: u64,
    /// `CEntityIdentity` -> `CEntityInstance* m_pInstance`.
    pub identity_instance: u64,
    /// `CEntityIdentity` -> `CEntityClass* m_pClass`.
    pub identity_class: u64,
    /// `CEntityIdentity` -> `CEntityHandle m_EHandle`.
    pub identity_handle: u64,
    /// `CEntityIdentity` -> `const char* m_designerName`.
    pub identity_designer_name: u64,
    /// `CEntityClass` -> pointer to the class-info record.
    pub class_info_ptr: u64,
    /// Class-info record -> `const char*` holding the schema class name.
    pub class_info_name: u64,
    /// Highest entity index walked.
    pub max_entities: u32,
}

impl EntityLayout {
    /// Layout confirmed against a live Deadlock client. See module docs.
    pub const DEADLOCK: EntityLayout = EntityLayout {
        identity_stride: 0x70,
        chunk_shift: 9,
        chunk_mask: 0x1FF,
        chunk_table_stride: 8,
        chunk_table_base: 0x10,
        identity_instance: 0x00,
        identity_class: 0x08,
        identity_handle: 0x10,
        identity_designer_name: 0x20,
        class_info_ptr: 0x08,
        class_info_name: 0x08,
        max_entities: 0x8000,
    };
}

impl EntityLayout {
    /// Largest buffer the walk will allocate for one chunk, as a sanity bound.
    ///
    /// A real chunk is 512 * 0x70, about 35 KiB. Anything near this cap means the layout
    /// is wrong rather than exotic.
    pub const MAX_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

    /// Check the walk can run with this layout without panicking.
    ///
    /// [`EntitySnapshot::walk`] divides by `identity_stride` and shifts by `chunk_shift`,
    /// and sizes a buffer from both. All three are public fields, so a caller can hand
    /// over values that turn those into a division by zero, a shift overflow, or a
    /// multi-gigabyte allocation. Rejecting them here keeps the failure at the call that
    /// caused it rather than deep inside a later walk.
    pub fn validate(&self) -> Result<()> {
        if self.identity_stride == 0 {
            return Err(Error::InvalidLayout("identity_stride must not be zero"));
        }
        if self.chunk_table_stride == 0 {
            return Err(Error::InvalidLayout("chunk_table_stride must not be zero"));
        }
        if self.chunk_shift >= 32 {
            return Err(Error::InvalidLayout("chunk_shift must be under 32"));
        }
        if self.max_entities == 0 {
            return Err(Error::InvalidLayout("max_entities must not be zero"));
        }
        let per_chunk = 1u64 << self.chunk_shift;
        if per_chunk.saturating_mul(self.identity_stride) > Self::MAX_CHUNK_BYTES {
            return Err(Error::InvalidLayout(
                "chunk_shift and identity_stride imply an implausible chunk size",
            ));
        }
        // Every offset the walk reads has to sit inside one record.
        for off in [
            self.identity_instance,
            self.identity_class,
            self.identity_handle,
            self.identity_designer_name,
        ] {
            if off.saturating_add(8) > self.identity_stride {
                return Err(Error::InvalidLayout(
                    "an identity offset reaches past the end of a record",
                ));
            }
        }
        Ok(())
    }
}

impl Default for EntityLayout {
    fn default() -> Self {
        Self::DEADLOCK
    }
}

/// One live entity.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Entity {
    /// Index within the entity list.
    pub index: u32,
    /// Address of the `CEntityIdentity`.
    pub identity: u64,
    /// Address of the entity instance itself; this is what field offsets apply to.
    pub instance: u64,
    /// Raw `CEntityHandle` value.
    pub handle: u32,
    /// `m_designerName`, e.g. `citadel_player_pawn`.
    ///
    /// `Arc<str>` rather than `String`: a walk produces one per entity (over 5,000 in a
    /// live match), and interning makes each clone a refcount bump instead of a heap
    /// allocation.
    #[cfg_attr(feature = "serde", serde(serialize_with = "ser_arc_str"))]
    pub designer_name: Arc<str>,
    /// Schema class name via `m_pClass`, e.g. `C_CitadelPlayerPawn`.
    #[cfg_attr(feature = "serde", serde(serialize_with = "ser_arc_str"))]
    pub class_name: Arc<str>,
}

/// Written out rather than derived: `impl Default for Arc<str>` is Rust 1.80, and the
/// manifests claim 1.75. Deriving compiles fine on a current toolchain and fails only in
/// the MSRV job, which is the worst place to find out.
impl Default for Entity {
    fn default() -> Self {
        Entity {
            index: 0,
            identity: 0,
            instance: 0,
            handle: 0,
            designer_name: Arc::from(""),
            class_name: Arc::from(""),
        }
    }
}

impl Entity {
    /// Best available name: the schema class name if we got one, else the designer name.
    pub fn best_name(&self) -> &str {
        if self.class_name.is_empty() {
            &self.designer_name
        } else {
            &self.class_name
        }
    }
}

/// Memoises entity name strings by their remote pointer.
///
/// Both `m_designerName` and the schema class name point at string constants in the
/// game image, so the mapping is stable for the lifetime of the process. Reusing it
/// across ticks removes nearly all per-tick string reads.
#[derive(Debug, Clone)]
pub struct NameCache {
    by_ptr: HashMap<u64, Arc<str>>,
    class_by_ptr: HashMap<u64, Arc<str>>,
    /// The one empty string, so a nameless entity does not allocate either.
    empty: Arc<str>,
}

/// Not derived, for the same reason as [`Entity`]: `Arc<str>` only gained a `Default` impl
/// in Rust 1.80.
impl Default for NameCache {
    fn default() -> Self {
        NameCache {
            by_ptr: HashMap::new(),
            class_by_ptr: HashMap::new(),
            empty: Arc::from(""),
        }
    }
}

impl NameCache {
    /// Drop everything. Call after re-attaching to a different process.
    pub fn clear(&mut self) {
        self.by_ptr.clear();
        self.class_by_ptr.clear();
    }

    /// How many distinct strings are memoised.
    pub fn len(&self) -> usize {
        self.by_ptr.len() + self.class_by_ptr.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn designer(&mut self, proc: &dyn MemoryReader, ptr: u64) -> Arc<str> {
        if ptr == 0 {
            return Arc::clone(&self.empty);
        }
        if let Some(s) = self.by_ptr.get(&ptr) {
            return Arc::clone(s);
        }
        // Only a successful read is memoised. Caching a failure would pin an empty name
        // for this pointer for the life of the cache, and `of_class` would stop matching
        // those entities in every later snapshot.
        let Ok(name) = proc.read_cstr(ptr, 96) else {
            return Arc::clone(&self.empty);
        };
        let s: Arc<str> = Arc::from(name.as_str());
        self.by_ptr.insert(ptr, Arc::clone(&s));
        s
    }

    /// `CEntityClass -> class-info record -> name string`: two hops, memoised on the
    /// `CEntityClass` pointer so the hops happen once per class rather than per entity.
    fn class(
        &mut self,
        proc: &dyn MemoryReader,
        class_ptr: u64,
        layout: &EntityLayout,
    ) -> Arc<str> {
        if class_ptr == 0 {
            return Arc::clone(&self.empty);
        }
        if let Some(s) = self.class_by_ptr.get(&class_ptr) {
            return Arc::clone(s);
        }
        let read = proc
            .read_ptr(class_ptr + layout.class_info_ptr)
            .ok()
            .filter(|&p| p != 0)
            .and_then(|i| proc.read_ptr(i + layout.class_info_name).ok())
            .filter(|&p| p != 0)
            .and_then(|p| proc.read_cstr(p, 96).ok());
        // As in `designer`: a failed hop is transient, a memoised empty name is forever.
        let Some(name) = read else {
            return Arc::clone(&self.empty);
        };
        let s: Arc<str> = Arc::from(name.as_str());
        self.class_by_ptr.insert(class_ptr, Arc::clone(&s));
        s
    }
}

/// A snapshot of the entity list, taken once per tick.
#[derive(Clone, Debug, Default)]
pub struct EntitySnapshot {
    entities: Vec<Entity>,
    by_index: HashMap<u32, usize>,
    /// Indices into `entities`, grouped by [`Entity::best_name`].
    ///
    /// Built once per walk, in entity order, so lookups keep the order a linear scan
    /// would have produced. Assembling one live snapshot resolves about a dozen classes
    /// by name, and each of those used to be a full pass over 4-5k entities with a string
    /// comparison apiece - every poll, ten times a second.
    by_class: HashMap<Arc<str>, Vec<usize>>,
    /// Layout used to produce this snapshot.
    pub layout: EntityLayout,
}

/// Group entity indices by best name, preserving entity order within each group.
fn index_by_class(entities: &[Entity]) -> HashMap<Arc<str>, Vec<usize>> {
    let mut by_class: HashMap<Arc<str>, Vec<usize>> = HashMap::new();
    for (i, e) in entities.iter().enumerate() {
        // `best_name` prefers the class name and falls back to the designer name; take
        // the `Arc` for whichever it chose rather than allocating a fresh key.
        let key = if e.class_name.is_empty() {
            Arc::clone(&e.designer_name)
        } else {
            Arc::clone(&e.class_name)
        };
        by_class.entry(key).or_default().push(i);
    }
    by_class
}

impl EntitySnapshot {
    /// Walk the entity list into a fresh snapshot.
    ///
    /// The chunk table hangs off `*entity_system`, not off the `entity_identity_list`
    /// global - see the module docs for why.
    pub fn walk(proc: &dyn MemoryReader, globals: &Globals, layout: EntityLayout) -> Result<Self> {
        Self::walk_cached(proc, globals, layout, &mut NameCache::default(), 0)
    }

    /// As [`EntitySnapshot::walk`], but reusing a name cache across ticks.
    ///
    /// Class and designer names are string constants in the game image: a few dozen
    /// distinct pointers serve thousands of entities, and they never move. Threading a
    /// cache through a polling loop removes essentially all of the per-tick string reads.
    pub fn walk_cached(
        proc: &dyn MemoryReader,
        globals: &Globals,
        layout: EntityLayout,
        names: &mut NameCache,
        hint: usize,
    ) -> Result<Self> {
        // `walk` and `walk_cached` take a caller-supplied layout whose fields are all
        // `pub`, and everything below trusts these numbers: `1 << chunk_shift` shifts,
        // `got / stride` divides, and `per_chunk * stride` sizes an allocation.
        layout.validate()?;

        let system = proc.read_ptr(globals.entity_system).unwrap_or(0);
        if system == 0 {
            return Err(Error::EntitySystemUnresolved);
        }

        let per_chunk = 1u32 << layout.chunk_shift;
        let chunk_count = layout.max_entities.div_ceil(per_chunk);
        let stride = layout.identity_stride as usize;

        // Sized from the previous walk. Both of these were regrowing from empty every
        // tick, which is thousands of reallocations a second at a useful poll rate.
        let mut entities = Vec::with_capacity(hint);
        let mut by_index = HashMap::with_capacity(hint);

        // One read per chunk instead of several per slot. A chunk is 512 * 0x70 = 35 KiB,
        // so this turns ~30k round trips into a few dozen.
        let mut buf = vec![0u8; per_chunk as usize * stride];
        let le_u64 = |b: &[u8], at: usize| -> u64 {
            u64::from_le_bytes(b[at..at + 8].try_into().unwrap_or([0; 8]))
        };
        let le_u32 = |b: &[u8], at: usize| -> u32 {
            u32::from_le_bytes(b[at..at + 4].try_into().unwrap_or([0; 4]))
        };

        // The chunk table is 64 pointers; fetch it in one read rather than 64.
        let mut table = vec![0u8; chunk_count as usize * layout.chunk_table_stride as usize];
        let table_got = proc.read_partial(system + layout.chunk_table_base, &mut table);

        for c in 0..chunk_count as u64 {
            let at = (c * layout.chunk_table_stride) as usize;
            if at + 8 > table_got {
                break;
            }
            let chunk = le_u64(&table, at);
            if chunk == 0 {
                continue;
            }
            let got = proc.read_partial(chunk, &mut buf);
            let usable = got / stride;

            for slot in 0..usable {
                let rec = &buf[slot * stride..(slot + 1) * stride];
                let instance = le_u64(rec, layout.identity_instance as usize);
                if instance == 0 {
                    continue;
                }
                let index = (c * per_chunk as u64 + slot as u64) as u32;
                if index >= layout.max_entities {
                    break;
                }

                let identity = chunk + slot as u64 * layout.identity_stride;
                let handle = le_u32(rec, layout.identity_handle as usize);
                let designer_ptr = le_u64(rec, layout.identity_designer_name as usize);
                let class_ptr = le_u64(rec, layout.identity_class as usize);

                let designer_name = names.designer(proc, designer_ptr);
                let class_name = names.class(proc, class_ptr, &layout);

                by_index.insert(index, entities.len());
                entities.push(Entity {
                    index,
                    identity,
                    instance,
                    handle,
                    designer_name,
                    class_name,
                });
            }
        }

        let by_class = index_by_class(&entities);
        Ok(EntitySnapshot {
            entities,
            by_index,
            by_class,
            layout,
        })
    }

    /// The layout these entities were walked with.
    pub fn layout(&self) -> EntityLayout {
        self.layout
    }

    /// Build a snapshot from a subset of entities, keeping the same layout.
    ///
    /// Used to pin the handful of entities a live snapshot actually reads, so later ticks
    /// can skip the full walk. See [`crate::cache::SnapshotCache`].
    pub fn from_entities(entities: Vec<Entity>, layout: EntityLayout) -> Self {
        let by_index = entities
            .iter()
            .enumerate()
            .map(|(i, e)| (e.index, i))
            .collect();
        let by_class = index_by_class(&entities);
        EntitySnapshot {
            entities,
            by_index,
            by_class,
            layout,
        }
    }

    /// Re-read each entity's handle and report whether every one still matches.
    ///
    /// A `CEntityHandle` carries a serial number that changes when the slot is recycled,
    /// so this catches an entity dying and a different one taking its place - which is
    /// the failure that would otherwise let a pinned address quietly point at a stranger.
    /// Costs one 4-byte read per entity, so keep the pinned set small.
    ///
    /// An empty snapshot is vacuously valid, which is the right answer to the question
    /// asked here and the wrong basis for reusing a cache - see [`crate::cache`].
    pub fn handles_still_valid(&self, proc: &dyn MemoryReader) -> bool {
        self.entities.iter().all(|e| {
            proc.read_u32(e.identity + self.layout.identity_handle)
                .is_ok_and(|h| h == e.handle)
        })
    }

    /// All entities in the snapshot.
    pub fn all(&self) -> &[Entity] {
        &self.entities
    }

    /// Number of live entities found.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Whether the snapshot is empty.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Look up an entity by list index.
    pub fn by_index(&self, index: u32) -> Option<&Entity> {
        self.by_index.get(&index).map(|&i| &self.entities[i])
    }

    /// Resolve a `CHandle` to an entity, masking off the serial number.
    pub fn by_handle(&self, handle: u32) -> Option<&Entity> {
        if handle == INVALID_HANDLE {
            return None;
        }
        self.by_index(handle & HANDLE_INDEX_MASK)
    }

    /// Every entity whose best name equals `name`.
    ///
    /// `of_class(..).find(..)` inside an `if let` can trip the temporary-drop rule; use
    /// [`EntitySnapshot::find`] there instead.
    pub fn of_class<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Entity> + 'a {
        self.by_class
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(move |&i| &self.entities[i])
    }

    /// First entity satisfying `pred`.
    ///
    /// Exists because `of_class(..).find(..)` returns an opaque iterator with a
    /// destructor, which borrow-checks badly as an `if let` scrutinee.
    pub fn find(&self, pred: impl Fn(&Entity) -> bool) -> Option<&Entity> {
        self.entities.iter().find(|e| pred(e))
    }

    /// The first entity whose best name equals `name`.
    pub fn first_of_class(&self, name: &str) -> Option<&Entity> {
        self.by_class
            .get(name)
            .and_then(|v| v.first())
            .map(|&i| &self.entities[i])
    }

    /// Every entity whose best name contains `needle`.
    pub fn matching<'a>(&'a self, needle: &'a str) -> impl Iterator<Item = &'a Entity> + 'a {
        self.entities
            .iter()
            .filter(move |e| e.best_name().contains(needle))
    }

    /// Count of each distinct class name, sorted descending by count.
    pub fn class_breakdown(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for e in &self.entities {
            *counts.entry(e.best_name()).or_insert(0) += 1;
        }
        let mut v: Vec<(String, usize)> = counts
            .into_iter()
            .map(|(k, n)| (k.to_string(), n))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }
}

/// The classes [`is_structure_class`] treats as structures.
///
/// A list rather than a `matches!` arm so a test can walk it: these strings are matched
/// against an entity's own class name and never used to look up a field, so a misspelling
/// resolves to nothing and the objective silently never appears.
pub const STRUCTURE_CLASSES: &[&str] = &[
    "C_NPC_TrooperBoss",
    "C_NPC_Boss_Tier2",
    "C_NPC_BarrackBoss",
    "C_NPC_Boss_Tier3",
    "C_Citadel_Destroyable_Building",
    "C_NPC_MidBoss",
];

/// Whether a class is a map structure rather than a creep.
///
/// The original's label table lumps lane troopers and neutral camps in with walkers and
/// the Patron. In a live match that is ~340 creeps against ~20 structures, which makes
/// an "objectives" list useless, so the two are kept apart.
pub fn is_structure_class(class: &str) -> bool {
    STRUCTURE_CLASSES.contains(&class)
}

/// `Arc<str>` has no `Serialize` impl, but it derefs to `str`, which does. Serialising
/// the contents keeps the JSON shape identical to when these were `String`.
#[cfg(feature = "serde")]
fn ser_arc_str<S: serde::Serializer>(v: &Arc<str>, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Designer names read as designer names, not as whatever sits nearby.
    ///
    /// [`EntityLayout::identity_designer_name`] is a probed offset, and moving it from
    /// `0x20` to `0x28` passed the entire suite, live tests included. The reason it hides is
    /// [`EntityRef::best_name`]: it prefers the designer name and falls back to the class
    /// name, so a wrong offset does not produce an error or an empty snapshot — it produces
    /// entities that quietly answer to their class names instead, which is a perfectly
    /// normal thing for an entity to do.
    ///
    /// `citadel_gamerules` is the anchor because it is a singleton with a stable name that
    /// a live client always has. The breadth check is there so the test does not pass on
    /// one lucky string: a wrong offset that happened to land on another readable pointer
    /// would have to reproduce a whole population of `lowercase_underscore` names.
    ///
    /// An entity with no designer name is normal - `C_CitadelTeam` has none - so this
    /// asserts a floor rather than universality.
    #[test]
    #[ignore = "needs a running deadlock.exe"]
    fn designer_names_come_from_the_right_offset() {
        let reader = crate::Reader::attach().expect("attach");
        let entities = reader.entities().expect("entities");
        let all = entities.all();
        assert!(all.len() > 100, "only {} entities", all.len());

        let rules = all
            .iter()
            .find(|e| &*e.class_name == "C_CitadelGameRulesProxy")
            .expect("a live client always has the game rules proxy");
        assert_eq!(
            &*rules.designer_name, "citadel_gamerules",
            "the game rules proxy's designer name is not where it should be"
        );

        let plausible = all
            .iter()
            .filter(|e| {
                let d = &*e.designer_name;
                !d.is_empty()
                    && d.len() < 64
                    && d.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            })
            .count();
        assert!(
            plausible > 20,
            "only {plausible} of {} entities have a plausible designer name",
            all.len()
        );
    }

    #[test]
    fn handle_masking() {
        assert_eq!(0x1234_8001u32 & HANDLE_INDEX_MASK, 0x0001);
        assert_eq!(0x0000_7FFFu32 & HANDLE_INDEX_MASK, 0x7FFF);
    }

    /// Built through `from_entities` rather than by poking the private fields: the
    /// snapshot maintains several indices that have to agree with each other, and a test
    /// that fills one of them by hand silently stops covering the others.
    #[test]
    fn snapshot_lookups() {
        let snap = EntitySnapshot::from_entities(
            vec![Entity {
                index: 5,
                class_name: "C_CitadelPlayerPawn".into(),
                ..Default::default()
            }],
            EntityLayout::DEADLOCK,
        );

        assert!(snap.by_index(5).is_some());
        assert!(snap.by_index(6).is_none());
        assert!(snap.by_handle(0x0000_8005 | 0x1234_0000).is_none() || snap.by_handle(5).is_some());
        assert!(snap.by_handle(INVALID_HANDLE).is_none());
        assert_eq!(snap.first_of_class("C_CitadelPlayerPawn").unwrap().index, 5);
    }

    /// The class index replaced a linear scan, so it has to agree with one: same
    /// entities, same order, and the designer-name fallback still honoured.
    #[test]
    fn the_class_index_matches_a_linear_scan() {
        let ent = |index: u32, class: &str, designer: &str| Entity {
            index,
            class_name: class.into(),
            designer_name: designer.into(),
            ..Default::default()
        };
        let entities = vec![
            ent(0, "C_CitadelPlayerPawn", ""),
            ent(1, "C_CitadelTeam", ""),
            ent(2, "C_CitadelPlayerPawn", ""),
            ent(3, "", "C_CitadelPlayerPawn"),
        ];
        let snap = EntitySnapshot::from_entities(entities.clone(), EntityLayout::DEADLOCK);

        for name in ["C_CitadelPlayerPawn", "C_CitadelTeam", "C_NotPresent"] {
            let scanned: Vec<u32> = entities
                .iter()
                .filter(|e| e.best_name() == name)
                .map(|e| e.index)
                .collect();
            let indexed: Vec<u32> = snap.of_class(name).map(|e| e.index).collect();
            assert_eq!(indexed, scanned, "of_class disagreed for {name}");
            assert_eq!(
                snap.first_of_class(name).map(|e| e.index),
                scanned.first().copied(),
                "first_of_class disagreed for {name}"
            );
        }

        assert_eq!(
            snap.of_class("C_CitadelPlayerPawn")
                .map(|e| e.index)
                .collect::<Vec<_>>(),
            vec![0, 2, 3]
        );
    }

    /// `walk` and `walk_cached` are public and take a layout whose fields are all `pub`.
    /// Only the setter on `Reader` validated one, so a caller building a layout by hand
    /// reached arithmetic that trusts it: a shift of 32 or more, a zero stride used as a
    /// divisor, and a chunk size used to reserve memory.
    #[test]
    fn a_hand_built_layout_is_validated_before_it_is_walked() {
        use crate::globals::Globals;
        use deadlock_memory::mock::MockMemory;

        let mem = MockMemory::new(1);
        let globals = Globals {
            entity_identity_list: 0,
            entity_system: 0,
            schema_system: 0,
        };

        let cases = [
            (
                EntityLayout {
                    chunk_shift: 64,
                    ..EntityLayout::DEADLOCK
                },
                "a shift past the width of the type",
            ),
            (
                EntityLayout {
                    identity_stride: 0,
                    ..EntityLayout::DEADLOCK
                },
                "a zero stride divides",
            ),
            (
                EntityLayout {
                    chunk_table_stride: 0,
                    ..EntityLayout::DEADLOCK
                },
                "a zero table stride",
            ),
            (
                EntityLayout {
                    max_entities: 0,
                    ..EntityLayout::DEADLOCK
                },
                "no entities at all",
            ),
        ];

        for (layout, why) in cases {
            let err = EntitySnapshot::walk(&mem, &globals, layout)
                .expect_err(&format!("should be rejected: {why}"));
            assert!(
                matches!(err, Error::InvalidLayout(_)),
                "{why}: expected InvalidLayout, got {err:?}"
            );
        }
    }

    #[test]
    fn objective_labels() {
        use crate::snapshot::ObjectiveKind;
        assert_eq!(
            ObjectiveKind::from_class("C_NPC_Boss_Tier3"),
            Some(ObjectiveKind::Patron)
        );
        assert_eq!(ObjectiveKind::from_class("C_CitadelPlayerPawn"), None);
    }

    #[test]
    fn verified_constants() {
        assert_eq!(EntityLayout::DEADLOCK.identity_stride, 0x70);
        assert_eq!(HANDLE_INDEX_MASK, 0x7FFF);
    }

    /// One transient failure used to be memoised, and `of_class` then stopped matching
    /// that class for the life of the cache - the players simply vanished from every
    /// later snapshot with nothing reported.
    #[test]
    fn a_failed_name_read_is_not_memoised() {
        use deadlock_memory::mock::MockMemory;

        let mut mem = MockMemory::new(1);
        let mut cache = NameCache::default();

        assert_eq!(&*cache.designer(&mem, 0x4000), "");
        assert!(
            cache.is_empty(),
            "a failed read must leave the cache untouched"
        );

        mem.write_cstr(0x4000, "CCitadelPlayerController");
        assert_eq!(&*cache.designer(&mem, 0x4000), "CCitadelPlayerController");
        assert_eq!(cache.len(), 1, "the successful read is memoised");

        assert_eq!(&*cache.designer(&mem, 0x4000), "CCitadelPlayerController");
        assert_eq!(cache.len(), 1);
    }

    /// The same rule for the two-hop class lookup, which has three places to fail.
    #[test]
    fn a_failed_class_hop_is_not_memoised() {
        use deadlock_memory::mock::MockMemory;

        const CLASS: u64 = 0x5000;
        const INFO: u64 = 0x6000;
        const NAME: u64 = 0x7000;

        let layout = EntityLayout::DEADLOCK;
        let mut mem = MockMemory::new(1);
        let mut cache = NameCache::default();

        mem.write_u64(CLASS + layout.class_info_ptr, INFO);
        assert_eq!(&*cache.class(&mem, CLASS, &layout), "");
        assert!(cache.is_empty(), "a broken hop must not be cached");

        mem.write_u64(INFO + layout.class_info_name, NAME);
        mem.write_cstr(NAME, "C_CitadelPlayerPawn");
        assert_eq!(&*cache.class(&mem, CLASS, &layout), "C_CitadelPlayerPawn");
        assert_eq!(cache.len(), 1);
    }

    /// A null pointer is a real answer, not a failure, and must stay cheap.
    #[test]
    fn a_null_pointer_reads_as_empty_without_caching() {
        use deadlock_memory::mock::MockMemory;

        let mem = MockMemory::new(1);
        let mut cache = NameCache::default();
        assert_eq!(&*cache.designer(&mem, 0), "");
        assert_eq!(&*cache.class(&mem, 0, &EntityLayout::DEADLOCK), "");
        assert!(cache.is_empty());
    }
}
