//! The attached reader facade: resolve once, then read fields by name.

use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::abi::Abi;
use crate::drift::{self, Drift};
use crate::entity::{EntityLayout, EntitySnapshot, NameCache};
use crate::error::{Error, Result};
use crate::globals::Globals;
use crate::schema::{SchemaIndex, SchemaLayout};
use crate::snapshot::{LiveSnapshot, LiveState, Loading};
use crate::tunables::Tunables;
#[cfg(any(windows, target_os = "linux"))]
use crate::{CLIENT_MODULE, DEADLOCK_PROCESS};
use deadlock_memory::mem::{MemoryReader, Module};

/// Drift entries, plus a membership index so re-noticing one costs nothing.
///
/// The index keys on a hash of the class and field rather than on the entry, because the
/// entry owns the two `String`s that are the cost being avoided. Nothing is read back out
/// of it; it only answers "already recorded?" without building the thing to look up.
#[derive(Debug, Default)]
struct DriftLog {
    seen: HashSet<u64>,
    entries: BTreeSet<Drift>,
}

/// Which way a fallback resolution went, for keying the drift index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fallback {
    /// The baked offset was used.
    Stale,
    /// The baked offset was refused; see [`drift::UNSAFE_FALLBACKS`].
    Refused,
}

/// Key a fallback resolution for [`DriftLog::seen`].
///
/// The kind is part of the key: a field can only ever go one way, but keying without it
/// would let a change in [`drift::UNSAFE_FALLBACKS`] hide the new verdict behind the old
/// one for the life of the process.
fn fallback_key(kind: Fallback, class: &str, field: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // `Hash for str` terminates each string, so `("ab", "c")` and `("a", "bc")` differ.
    (kind == Fallback::Refused).hash(&mut h);
    class.hash(&mut h);
    field.hash(&mut h);
    h.finish()
}

/// Where two `u32` fields sit inside each element of a strided struct vector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PairLayout {
    /// `sizeof(element)`.
    pub stride: u64,
    /// Offset of the first field.
    pub a: u64,
    /// Offset of the second field.
    pub b: u64,
}

/// An attached, read-only view of the game.
///
/// Cheap to share: `Send + Sync`, so it drops straight into a `tauri::State`, an
/// `Arc` behind a polling thread, or a web framework's app state. All the read methods
/// take `&self`.
pub struct Reader {
    mem: Arc<dyn MemoryReader>,
    abi: Abi,
    client: Module,
    globals: Globals,
    schema: Option<SchemaIndex>,
    schema_error: Option<Error>,
    entity_layout: EntityLayout,
    tunables: Tunables,
    image_bytes_read: usize,
    /// Entity name memoisation, shared across ticks. Behind a mutex so `Reader` stays
    /// `Sync` and `entities()` can keep taking `&self`.
    names: Mutex<NameCache>,
    /// Everything noticed that suggests the game has moved on. See [`Reader::drift`].
    drift: RwLock<DriftLog>,
    /// Entity count from the last walk, used to pre-size the next one.
    entity_hint: AtomicUsize,
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader")
            .field("pid", &self.mem.pid())
            .field("abi", &self.abi)
            .field("module", &self.client.name)
            .field("base", &format_args!("{:#x}", self.client.base))
            .field("globals", &self.globals)
            .field(
                "schema",
                &self
                    .schema
                    .as_ref()
                    .map(super::schema::SchemaIndex::class_count),
            )
            .field("schema_error", &self.schema_error)
            .finish()
    }
}

impl Reader {
    /// Attach to `deadlock.exe` and resolve everything.
    ///
    /// A failing schema walk is **not** an error: the reader falls back to the offset
    /// table baked into [`crate::fields`]. Check [`Reader::schema_error`] to see whether
    /// the runtime schema is actually in play.
    #[cfg(windows)]
    pub fn attach() -> Result<Self> {
        Self::attach_named(DEADLOCK_PROCESS, CLIENT_MODULE)
    }

    /// Attach to the game, trying Proton first and then a native build.
    ///
    /// Proton is checked first because it is what ships today: the Windows `deadlock.exe`
    /// hosted by Wine, with the real PE mapped. A native `deadlock` + `client.so` is
    /// tried second so this keeps working if one appears - though
    /// [`Reader::with_memory`] will then report [`Error::UnsupportedAbi`] until `SysV`
    /// constants are derived.
    #[cfg(target_os = "linux")]
    pub fn attach() -> Result<Self> {
        use crate::abi::Abi;
        match Self::attach_named(DEADLOCK_PROCESS, CLIENT_MODULE) {
            Err(Error::Memory(deadlock_memory::Error::ProcessNotFound(_))) => {}
            other => return other,
        }
        Self::attach_named(crate::DEADLOCK_PROCESS_NATIVE, Abi::SysV.client_module())
    }

    /// Attach to an arbitrary process/module pair, e.g. another Source 2 title.
    #[cfg(windows)]
    pub fn attach_named(exe: &str, module: &str) -> Result<Self> {
        Self::with_memory(
            Arc::new(deadlock_memory::process::Process::attach(exe)?),
            module,
        )
    }

    /// Attach to an arbitrary process/module pair.
    #[cfg(target_os = "linux")]
    pub fn attach_named(exe: &str, module: &str) -> Result<Self> {
        Self::with_memory(
            Arc::new(deadlock_memory::linux::LinuxProcess::attach(exe)?),
            module,
        )
    }

    /// Build a reader over any [`MemoryReader`] backend.
    ///
    /// This is the platform-independent entry point: [`Reader::attach`] is a thin
    /// wrapper over it, and a Linux or macOS backend would plug in here without any
    /// other change. It is also how tests drive the reader over
    /// [`deadlock_memory::mock::MockMemory`].
    pub fn with_memory(mem: Arc<dyn MemoryReader>, module: &str) -> Result<Self> {
        let client = mem.module(module)?;

        let (image, image_bytes_read) = mem.read_image(client.base, client.size);
        if image_bytes_read == 0 {
            // Unreadable pages are left zeroed, so a process that exited mid-attach hands
            // back a full-size buffer of zeros. `Abi::detect` then finds no PE header,
            // falls through to `Win64`, and the whole thing surfaces as
            // `SignatureNotFound("entity_identity_list")` - a game-changed error for a
            // game-closed condition.
            return Err(deadlock_memory::Error::ProcessGone {
                pid: mem.pid(),
                os: 0,
            }
            .into());
        }

        // Which tables apply is decided by what is actually mapped, not by the host OS.
        // A Proton-hosted game read from Linux is still a PE, and still Win64.
        let abi = Abi::detect(&image)
            .or_else(|| Abi::from_module_name(&client.name))
            .unwrap_or(Abi::Win64);
        let signatures = abi.signatures().ok_or(Error::UnsupportedAbi { abi })?;
        let entity_layout = abi.entity_layout().ok_or(Error::UnsupportedAbi { abi })?;
        let schema_base = abi.schema_layout().ok_or(Error::UnsupportedAbi { abi })?;

        let globals = Globals::resolve_with(signatures, &image, client.base, client.size)?;

        let (schema, schema_error) =
            match SchemaIndex::discover_from(mem.as_ref(), globals.schema_system, schema_base) {
                Ok((idx, _layout)) => (Some(idx), None),
                Err(e) => (None, Some(e)),
            };

        Ok(Reader {
            mem,
            abi,
            client,
            globals,
            schema,
            schema_error,
            entity_layout,
            tunables: Tunables::default(),
            image_bytes_read,
            names: Mutex::new(NameCache::default()),
            drift: RwLock::new(DriftLog::default()),
            entity_hint: AtomicUsize::new(0),
        })
    }

    /// The ABI of the module this reader attached to.
    ///
    /// `Win64` for the shipping client, whether on Windows or under Proton.
    pub fn abi(&self) -> Abi {
        self.abi
    }

    /// Re-run the schema walk, e.g. once the game has finished loading a map.
    pub fn refresh_schema(&mut self) -> Result<()> {
        let base = self.abi.schema_layout().unwrap_or_default();
        match SchemaIndex::discover_from(self.mem.as_ref(), self.globals.schema_system, base) {
            Ok((idx, _)) => {
                self.schema = Some(idx);
                self.schema_error = None;
                Ok(())
            }
            Err(e) => {
                self.schema_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Pin an explicit schema layout instead of auto-discovering one.
    pub fn set_schema_layout(&mut self, layout: SchemaLayout) -> Result<()> {
        let idx = SchemaIndex::walk(self.mem.as_ref(), self.globals.schema_system, layout)?;
        self.schema = Some(idx);
        self.schema_error = None;
        Ok(())
    }

    /// Override the entity layout constants.
    ///
    /// Rejects a layout the entity walk cannot survive rather than accepting it and
    /// panicking later: a zero stride divides by zero, and a chunk shift of 32 or more
    /// overflows the shift and then asks for an absurd allocation. See
    /// [`EntityLayout::validate`].
    pub fn set_entity_layout(&mut self, layout: EntityLayout) -> Result<()> {
        layout.validate()?;
        self.entity_layout = layout;
        Ok(())
    }

    /// The patch-sensitive game constants in use. See [`crate::tunables`].
    pub fn tunables(&self) -> &Tunables {
        &self.tunables
    }

    /// Override patch-sensitive game constants, e.g. the bridge-buff cadence after a
    /// game patch retunes it. Do this before sharing the reader across threads.
    pub fn tunables_mut(&mut self) -> &mut Tunables {
        &mut self.tunables
    }

    /// Replace the whole set of patch-sensitive constants.
    pub fn set_tunables(&mut self, tunables: Tunables) {
        self.tunables = tunables;
    }

    /// The target process id.
    pub fn pid(&self) -> u32 {
        self.mem.pid()
    }

    /// The underlying memory backend, for custom reads.
    pub fn memory(&self) -> &dyn MemoryReader {
        self.mem.as_ref()
    }

    /// A cloneable handle to the memory backend.
    pub fn memory_arc(&self) -> Arc<dyn MemoryReader> {
        Arc::clone(&self.mem)
    }

    /// `client.dll` base address.
    pub fn client_base(&self) -> u64 {
        self.client.base
    }

    /// `client.dll` image size.
    pub fn client_size(&self) -> usize {
        self.client.size
    }

    /// How many bytes of the client image were actually readable.
    pub fn image_bytes_read(&self) -> usize {
        self.image_bytes_read
    }

    /// The three resolved globals.
    pub fn globals(&self) -> &Globals {
        &self.globals
    }

    /// The runtime schema index, if the walk succeeded.
    pub fn schema(&self) -> Option<&SchemaIndex> {
        self.schema.as_ref()
    }

    /// Why the schema walk failed, if it did.
    pub fn schema_error(&self) -> Option<&Error> {
        self.schema_error.as_ref()
    }

    /// The entity layout in use.
    pub fn entity_layout(&self) -> EntityLayout {
        self.entity_layout
    }

    /// Where the game is installed, derived from the loaded client module.
    ///
    /// `.../citadel/bin/win64/client.dll` -> `.../citadel`, which is the root the game's
    /// own localisation files hang off. `None` when the module path is not available or is
    /// too shallow to be that layout.
    ///
    /// Lives here because the client module is this type's to know about. It was otherwise
    /// going to be - and was - copied into every consumer that wanted localised names.
    pub fn game_dir(&self) -> Option<std::path::PathBuf> {
        let m = self.memory().module(&self.client.name).ok()?;
        m.path.ancestors().nth(3).map(std::path::PathBuf::from)
    }

    /// Resolve a field offset: runtime schema first, baked fallback second.
    ///
    /// This is the crate's central operation, and mirrors the original's
    /// `schema_offset_of(class, field)` + `cmov` fallback idiom, with one deliberate
    /// departure: a fallback listed in [`drift::UNSAFE_FALLBACKS`] is refused rather
    /// than used, because for those fields a stale number is worse than no number.
    ///
    /// Everything noticed here is recorded; see [`Reader::drift`].
    pub fn offset_of(&self, class: &str, field: &str) -> Option<u32> {
        if let Some(off) = self.schema.as_ref().and_then(|s| s.offset_of(class, field)) {
            return Some(off);
        }
        let off = self.abi.fallback_offset(class, field)?;
        // Reached only when the live schema could not answer, which is exactly when the
        // baked number is least likely to still be right.
        if drift::fallback_is_unsafe(class, field) {
            self.note_field(Fallback::Refused, class, field);
            return None;
        }
        self.note_field(Fallback::Stale, class, field);
        Some(off)
    }

    /// Record something that suggests the game has changed under us.
    ///
    /// Deduplicated, because a miss recurs on every tick and is worth reporting once.
    pub(crate) fn note(&self, d: Drift) {
        self.write_drift().entries.insert(d);
    }

    /// Record a fallback resolution, allocating only the first time this field is seen.
    ///
    /// The plain [`Reader::note`] path builds the entry - two `String`s - before the set
    /// gets to say it already had it. That is fine for the enum check, which runs once per
    /// snapshot, and wrong here: a failed schema walk sends every field through the
    /// fallback, so this is around 500 calls per poll, ten polls a second, all but the
    /// first of each discarded.
    fn note_field(&self, kind: Fallback, class: &str, field: &str) {
        let key = fallback_key(kind, class, field);
        if self.read_drift().seen.contains(&key) {
            return;
        }
        let mut log = self.write_drift();
        // Re-checked under the write lock: another thread may have inserted between the
        // read and the upgrade.
        if !log.seen.insert(key) {
            return;
        }
        let (class, field) = (class.to_owned(), field.to_owned());
        log.entries.insert(match kind {
            Fallback::Refused => Drift::RefusedFallback { class, field },
            Fallback::Stale => Drift::StaleOffset { class, field },
        });
    }

    fn read_drift(&self) -> std::sync::RwLockReadGuard<'_, DriftLog> {
        self.drift
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_drift(&self) -> std::sync::RwLockWriteGuard<'_, DriftLog> {
        self.drift
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether an offset came from the live schema rather than the baked table.
    pub fn offset_is_from_schema(&self, class: &str, field: &str) -> bool {
        self.schema
            .as_ref()
            .and_then(|s| s.offset_of(class, field))
            .is_some()
    }

    /// Everything noticed so far that suggests the game has moved on, deduplicated.
    ///
    /// This is the crate's staleness signal, and it is also carried on every
    /// [`LiveSnapshot::drift`] so a caller does not have to remember to ask.
    ///
    /// Empty is the healthy state: every field came from the running game and every enum
    /// agreed with it. Non-empty means a patch has changed something this crate depends
    /// on by name, and the crate is due an update.
    ///
    /// Populated lazily. A field only appears once something has tried to read it, so
    /// call this *after* a snapshot rather than before.
    pub fn drift(&self) -> Vec<Drift> {
        self.read_drift().entries.iter().cloned().collect()
    }

    /// Whether anything has drifted. See [`Reader::drift`].
    pub fn has_drifted(&self) -> bool {
        !self.read_drift().entries.is_empty()
    }

    /// Whether any drift means values are likely *wrong* rather than merely absent.
    ///
    /// This is the one worth alerting on. A refused fallback costs data and is safe; a
    /// stale offset or a renumbered enum hands back a number that looks right.
    pub fn is_reading_corrupt_data(&self) -> bool {
        self.read_drift().entries.iter().any(Drift::is_corrupting)
    }

    /// Every `(class, field)` that fell back to a baked offset.
    ///
    /// Kept for callers that only care about offsets; [`Reader::drift`] is the fuller
    /// picture.
    pub fn stale_fields(&self) -> Vec<(String, String)> {
        self.drift()
            .into_iter()
            .filter_map(|d| match d {
                Drift::StaleOffset { class, field } => Some((class, field)),
                _ => None,
            })
            .collect()
    }

    /// Whether any field has fallen back to a baked offset. See [`Reader::drift`].
    pub fn has_stale_offsets(&self) -> bool {
        !self.stale_fields().is_empty()
    }

    /// Take a fresh entity snapshot.
    pub fn entities(&self) -> Result<EntitySnapshot> {
        let mut names = self
            .names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let hint = self.entity_hint.load(Ordering::Relaxed);
        let snap = EntitySnapshot::walk_cached(
            self.mem.as_ref(),
            &self.globals,
            self.entity_layout,
            &mut names,
            hint,
        )?;
        // Next walk pre-sizes from this one. Entity counts drift slowly, so this is
        // right nearly always and merely a wasted reserve when it is not.
        self.entity_hint.store(snap.len(), Ordering::Relaxed);
        Ok(snap)
    }

    /// Drop the memoised entity-name strings.
    pub fn clear_name_cache(&self) {
        self.names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    /// Build a high-level match snapshot.
    ///
    /// `Ok(None)` means not in a match. An `Err` means a match is there but something
    /// load-bearing could not be read - see [`LiveSnapshot::build`].
    pub fn live_snapshot(&self) -> Result<Option<LiveSnapshot>> {
        let entities = self.entities()?;
        LiveSnapshot::build(self, &entities)
    }

    /// Like [`Reader::live_snapshot`], but says when the client is loading a map.
    ///
    /// `Ok(None)` from the plain call covers loading and nothing-to-read alike; here the
    /// first is [`LiveState::Loading`]. See [`LiveState`] for what "no game" looks like.
    pub fn live_state(&self) -> Result<LiveState> {
        let entities = self.entities()?;
        Ok(match LiveSnapshot::build(self, &entities)? {
            Some(snap) => LiveState::Live(Box::new(snap)),
            None => LiveState::Loading(Loading {
                entity_count: entities.len(),
            }),
        })
    }

    /// A live snapshot that reuses pinned entities instead of re-walking the entity list.
    ///
    /// Field values are exactly as fresh as [`Reader::live_snapshot`]; what lags is the
    /// entity list, bounded by the cache's refresh interval. Read the
    /// [`crate::cache::SnapshotCache`] docs for the tradeoff before using this.
    pub fn live_snapshot_cached(
        &self,
        cache: &mut crate::cache::SnapshotCache,
    ) -> Result<Option<LiveSnapshot>> {
        if cache.usable(self.mem.as_ref()) {
            cache.record_reuse();
            let total = cache.total_entities();
            if let Some(pinned) = cache.pinned() {
                let mut snap = LiveSnapshot::build(self, pinned)?;
                if let Some(s) = snap.as_mut() {
                    // The pinned set is a fraction of the world; reporting its size as the
                    // entity count would understate it by ~100x.
                    s.entity_count = total;
                }
                return Ok(snap);
            }
        }
        let entities = self.entities()?;
        cache.store(&entities, &self.tunables);
        LiveSnapshot::build(self, &entities)
    }

    /// Read a `u32` field by name from an object at `base`.
    pub fn field_u32(&self, base: u64, class: &str, field: &str) -> Option<u32> {
        let off = self.offset_of(class, field)?;
        self.mem.read_u32(base + off as u64).ok()
    }

    /// Read an `i32` field by name.
    pub fn field_i32(&self, base: u64, class: &str, field: &str) -> Option<i32> {
        let off = self.offset_of(class, field)?;
        self.mem.read_i32(base + off as u64).ok()
    }

    /// Read a `u64` field by name.
    pub fn field_u64(&self, base: u64, class: &str, field: &str) -> Option<u64> {
        let off = self.offset_of(class, field)?;
        self.mem.read_u64(base + off as u64).ok()
    }

    /// Read an `f32` field by name.
    pub fn field_f32(&self, base: u64, class: &str, field: &str) -> Option<f32> {
        let off = self.offset_of(class, field)?;
        self.mem.read_f32(base + off as u64).ok()
    }

    /// Read a `u8` field by name.
    pub fn field_u8(&self, base: u64, class: &str, field: &str) -> Option<u8> {
        let off = self.offset_of(class, field)?;
        self.mem.read_u8(base + off as u64).ok()
    }

    /// Read a `bool` field by name.
    pub fn field_bool(&self, base: u64, class: &str, field: &str) -> Option<bool> {
        Some(self.field_u8(base, class, field)? != 0)
    }

    /// Read a pointer field by name and dereference it, rejecting null.
    pub fn field_ptr(&self, base: u64, class: &str, field: &str) -> Option<u64> {
        let off = self.offset_of(class, field)?;
        self.mem
            .read_ptr(base + off as u64)
            .ok()
            .filter(|&p| p != 0)
    }

    /// Address of an embedded struct field, e.g. `m_PlayerDataGlobal`.
    ///
    /// Embedded structs are not pointers: the offset is added to the base and the result
    /// is itself the object address.
    pub fn field_addr(&self, base: u64, class: &str, field: &str) -> Option<u64> {
        Some(base + self.offset_of(class, field)? as u64)
    }

    /// Read a `CUtlVector<T>` field: its element count and data pointer.
    ///
    /// Source 2 lays these out as `{ i32 count; _pad; T* data; }`, so the count is at
    /// the field offset and the pointer eight bytes later.
    pub fn field_utlvec(&self, base: u64, class: &str, field: &str) -> Option<(u32, u64)> {
        let off = self.offset_of(class, field)? as u64;
        let count = self.mem.read_u32(base + off).ok()?;
        let data = self.mem.read_ptr(base + off + 8).ok()?;
        // A populated vector must have a pointer; an empty one legitimately has neither.
        if count == 0 || data == 0 {
            return Some((0, 0));
        }
        Some((count, data))
    }

    /// Read a `CUtlVector<u32>` field into a `Vec`.
    ///
    /// `limit` caps how many elements are read, so a corrupt count cannot make the
    /// reader allocate wildly.
    ///
    /// `None` when the vector could not be read *completely*. A short list is worse than
    /// no list: a consumer diffing one tick against the next reads the missing elements
    /// as deliberate removals, so a transient failure became a burst of phantom
    /// "item lost" events followed by "item purchased" when the read recovered.
    pub fn field_vec_u32(
        &self,
        base: u64,
        class: &str,
        field: &str,
        limit: usize,
    ) -> Option<Vec<u32>> {
        let (count, data) = self.field_utlvec(base, class, field)?;
        (0..count.min(limit as u32) as u64)
            .map(|i| self.mem.read_u32(data + i * 4).ok())
            .collect()
    }

    /// Read two `u32`s from each element of a strided `CUtlVector` of structs.
    ///
    /// Used for `AbilityUpgradeState_t` (stride `0x38`, with `m_ItemID` and
    /// `m_nUpgradeInfo` well into the record) where the element is far larger than the
    /// two fields wanted.
    /// `None` when the vector could not be read completely; see
    /// [`Reader::field_vec_u32`] for why a short list is not returned.
    pub fn field_vec_pairs(
        &self,
        base: u64,
        class: &str,
        field: &str,
        layout: PairLayout,
        limit: usize,
    ) -> Option<Vec<(u32, u32)>> {
        let PairLayout { stride, a, b } = layout;
        self.field_vec_strided(base, class, field, stride, limit, |mem, e| {
            Some((mem.read_u32(e + a).ok()?, mem.read_u32(e + b).ok()?))
        })
    }

    /// Read one value per element of a strided `CUtlVector` of structs.
    ///
    /// The general form of [`Reader::field_vec_pairs`], for elements that are not two
    /// `u32`s: `read` is handed each element's address and decodes whatever it needs.
    ///
    /// `stride` is `sizeof(element)`, which the runtime schema declares as the class size
    /// of the element type. Members of a Citadel networked value type sit well into the
    /// record - `StatViewerModifierValues_t` is 64 bytes with its three members at 0x30,
    /// 0x34 and 0x38 - so walking by anything other than the element size reads the next
    /// element's prologue.
    ///
    /// `limit` caps how many elements are read, so a corrupt count cannot make the reader
    /// allocate wildly.
    ///
    /// `None` when the vector could not be read *completely*; see
    /// [`Reader::field_vec_u32`] for why a short list is not returned.
    pub fn field_vec_strided<T>(
        &self,
        base: u64,
        class: &str,
        field: &str,
        stride: u64,
        limit: usize,
        read: impl Fn(&dyn MemoryReader, u64) -> Option<T>,
    ) -> Option<Vec<T>> {
        let (count, data) = self.field_utlvec(base, class, field)?;
        read_strided(self.mem.as_ref(), count, data, stride, limit, read)
    }

    /// Read a `CUtlVector` of pointers into a `Vec` of addresses.
    ///
    /// Zero pointers are dropped: the game leaves holes in these lists, and an address of
    /// zero is not something a caller can follow.
    ///
    /// `None` when the vector could not be read completely; see
    /// [`Reader::field_vec_u32`] for why a short list is not returned.
    pub fn field_vec_ptrs(
        &self,
        base: u64,
        class: &str,
        field: &str,
        limit: usize,
    ) -> Option<Vec<u64>> {
        let all =
            self.field_vec_strided(base, class, field, 8, limit, |mem, e| mem.read_ptr(e).ok())?;
        Some(all.into_iter().filter(|&p| p != 0).collect())
    }

    /// Read a `Vector` (three `f32`s) field by name.
    pub fn field_vec3(&self, base: u64, class: &str, field: &str) -> Option<[f32; 3]> {
        let off = self.offset_of(class, field)?;
        self.mem.read_vec3(base + off as u64).ok()
    }
}

/// Walk `count` elements of `stride` bytes from `data`, decoding each with `read`.
///
/// Split out from [`Reader::field_vec_strided`] because a `Reader` needs a live process
/// or the whole fake-image harness in `tests/`, and the striding arithmetic is worth
/// testing on its own against a sparse address space.
fn read_strided<T>(
    mem: &dyn MemoryReader,
    count: u32,
    data: u64,
    stride: u64,
    limit: usize,
    read: impl Fn(&dyn MemoryReader, u64) -> Option<T>,
) -> Option<Vec<T>> {
    (0..u64::from(count.min(limit as u32)))
        .map(|i| read(mem, data + i * stride))
        .collect()
}

/// Most bytes read for one object.
///
/// Entity classes run to 7 KB and more, but the fields anything here reads sit low -
/// health at 0x354, team at 0x3f3, the whole player controller inside 0xc40. Pulling 7 KB
/// to reach four `i32`s would spend more on bandwidth than the syscalls it saves. Fields
/// past the cap fall back to a direct read, so this only ever costs speed.
const MAX_OBJECT_BYTES: usize = 4096;

/// Absolute ceiling on a schema-sized object read.
///
/// [`Reader::read_object_spanning`] takes its size from the running game's schema, so a
/// corrupt or hostile value has to be bounded by something that is not.
const HARD_OBJECT_CAP: usize = 64 * 1024;

/// One object's bytes, read in a single call.
///
/// Field reads on a `Reader` are one syscall each, and a snapshot does hundreds of them:
/// twelve player controllers at ~35 fields apiece is 400 round trips into another process
/// to fetch 1,600 bytes. Reading the whole struct once and slicing locally turns that into
/// twelve.
///
/// Values are a point-in-time copy. That is usually an improvement - every field comes
/// from the same instant instead of drifting across a few hundred microseconds of reads -
/// but it does mean the buffer will not observe changes made after it was taken.
#[derive(Debug)]
pub struct Object<'r> {
    reader: &'r Reader,
    base: u64,
    bytes: Vec<u8>,
}

/// A fixed-width value [`Object`] can read either out of its buffer or live from the
/// process.
///
/// The point is that the two paths stay one piece of code. Each accessor used to carry its
/// own copy, and a copy is where the buffered path and the fallback path get to disagree
/// about what a field means.
trait Scalar: Sized {
    /// Bytes this occupies inside the object.
    const WIDTH: usize;
    /// Decode from the object's buffer. `None` when the slice is not [`Scalar::WIDTH`]
    /// long, which sends the caller to the live read rather than to a fabricated zero.
    fn from_le_slice(bytes: &[u8]) -> Option<Self>;
    /// Read straight from the process, for fields the buffer does not reach.
    fn read_live(mem: &dyn MemoryReader, addr: u64) -> Result<Self>;
}

macro_rules! impl_scalar {
    ($ty:ty, $width:expr, $read:ident) => {
        impl Scalar for $ty {
            const WIDTH: usize = $width;

            fn from_le_slice(bytes: &[u8]) -> Option<Self> {
                bytes.try_into().ok().map(<$ty>::from_le_bytes)
            }

            fn read_live(mem: &dyn MemoryReader, addr: u64) -> Result<Self> {
                Ok(mem.$read(addr)?)
            }
        }
    };
}

impl_scalar!(u8, 1, read_u8);
impl_scalar!(u16, 2, read_u16);
impl_scalar!(u32, 4, read_u32);
impl_scalar!(i32, 4, read_i32);
impl_scalar!(f32, 4, read_f32);
impl_scalar!(u64, 8, read_u64);

impl<'r> Object<'r> {
    /// Remote address this buffer starts at.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// Absolute address of a member, for the reads that still need the process - a
    /// `CUtlVector`'s backing store, say.
    pub fn member_addr(&self, class: &str, field: &str) -> Option<u64> {
        Some(self.base + self.reader.offset_of(class, field)? as u64)
    }

    /// Byte offset of an embedded struct within this buffer, for the `*_at` readers.
    pub fn member_off(&self, class: &str, field: &str) -> Option<u64> {
        self.reader.offset_of(class, field).map(u64::from)
    }

    /// Bytes of a field from the buffer, or `None` if the buffer does not reach it.
    ///
    /// Callers fall back to a live read rather than returning `None` to the user: a class
    /// with no schema size, or an object that was only partly mapped, must degrade to a
    /// slower read and not to missing data.
    fn slice(&self, at: u64, class: &str, field: &str, len: usize) -> Option<&[u8]> {
        let off = at as usize + self.reader.offset_of(class, field)? as usize;
        self.bytes.get(off..off + len)
    }

    /// Absolute address of a field, for the live-read fallback.
    fn addr_of(&self, at: u64, class: &str, field: &str) -> Option<u64> {
        Some(self.base + at + self.reader.offset_of(class, field)? as u64)
    }

    /// One fixed-width field: from the buffer when it reaches, else straight from the
    /// process.
    ///
    /// The five accessors below were byte-for-byte copies of this, differing only in the
    /// width and in which `read_*` to call - so the buffer-or-fallback rule, the offset
    /// arithmetic and the short-buffer handling each existed five times over.
    fn scalar_at<T: Scalar>(&self, at: u64, class: &str, field: &str) -> Option<T> {
        if let Some(v) = self
            .slice(at, class, field, T::WIDTH)
            .and_then(T::from_le_slice)
        {
            return Some(v);
        }
        // Also the path taken when the buffer somehow holds the wrong number of bytes,
        // which the copies handled by decoding a zero. A zero meaning "could not read" is
        // the exact failure the fallback exists to prevent.
        T::read_live(self.reader.memory(), self.addr_of(at, class, field)?).ok()
    }

    /// `u8` field. Source 2 packs booleans and team numbers at unaligned offsets, so this
    /// is not interchangeable with a masked `u32`.
    pub fn u8(&self, class: &str, field: &str) -> Option<u8> {
        self.u8_at(0, class, field)
    }

    /// `u8` field of a struct embedded `at` bytes into this object.
    pub fn u8_at(&self, at: u64, class: &str, field: &str) -> Option<u8> {
        self.scalar_at(at, class, field)
    }

    /// `bool` field.
    pub fn bool(&self, class: &str, field: &str) -> Option<bool> {
        self.u8(class, field).map(|v| v != 0)
    }

    /// `u32` field.
    pub fn u32(&self, class: &str, field: &str) -> Option<u32> {
        self.u32_at(0, class, field)
    }

    /// `u32` field of a struct embedded `at` bytes into this object.
    pub fn u32_at(&self, at: u64, class: &str, field: &str) -> Option<u32> {
        self.scalar_at(at, class, field)
    }

    /// `u16` field.
    ///
    /// Narrow fields are packed tight enough that width matters: `CBaseModifier` puts
    /// `m_iStackCount` at 0x62 and `m_iMaxStackCount` at 0x64, so a `u32` read of the
    /// first returns the second in its high half.
    pub fn u16(&self, class: &str, field: &str) -> Option<u16> {
        self.u16_at(0, class, field)
    }

    /// `u16` field of a struct embedded `at` bytes into this object.
    pub fn u16_at(&self, at: u64, class: &str, field: &str) -> Option<u16> {
        self.scalar_at(at, class, field)
    }

    /// `i32` field.
    pub fn i32(&self, class: &str, field: &str) -> Option<i32> {
        self.i32_at(0, class, field)
    }

    /// `i32` field of a struct embedded `at` bytes into this object.
    pub fn i32_at(&self, at: u64, class: &str, field: &str) -> Option<i32> {
        self.scalar_at(at, class, field)
    }

    /// `f32` field.
    pub fn f32(&self, class: &str, field: &str) -> Option<f32> {
        self.f32_at(0, class, field)
    }

    /// `f32` field of a struct embedded `at` bytes into this object.
    pub fn f32_at(&self, at: u64, class: &str, field: &str) -> Option<f32> {
        self.scalar_at(at, class, field)
    }

    /// `u64` field.
    pub fn u64(&self, class: &str, field: &str) -> Option<u64> {
        self.u64_at(0, class, field)
    }

    /// `u64` field of a struct embedded `at` bytes into this object.
    pub fn u64_at(&self, at: u64, class: &str, field: &str) -> Option<u64> {
        self.scalar_at(at, class, field)
    }

    /// Inline NUL-terminated string field, e.g. `m_iszPlayerName`.
    pub fn cstr(&self, class: &str, field: &str, max: usize) -> Option<String> {
        let off = self.reader.offset_of(class, field)? as usize;
        // Saturating because both operands can come from the running game.
        let want_end = off.saturating_add(max);
        let end = want_end.min(self.bytes.len());
        if let Some(raw) = self.bytes.get(off..end).filter(|r| !r.is_empty()) {
            if let Some(len) = raw.iter().position(|&b| b == 0) {
                return Some(String::from_utf8_lossy(&raw[..len]).into_owned());
            }
            // No terminator, but the buffer did reach the caller's own limit: the string
            // is simply longer than `max`, and stopping there is what was asked for.
            if end == want_end {
                return Some(String::from_utf8_lossy(raw).into_owned());
            }
            // No terminator and the buffer ran out first. This is a *truncated* read, and
            // returning it would hand back half a player name as though it were the whole
            // one - indistinguishable from someone who really is called "Alexand". Fall
            // through and read it properly.
        }
        self.reader
            .memory()
            .read_cstr(self.addr_of(0, class, field)?, max)
            .ok()
    }
}

impl Reader {
    /// Read an object's whole struct in one call, so its fields cost no more syscalls.
    ///
    /// The length comes from the schema's own class size. A short read is kept - a
    /// partially mapped object still answers for the fields that landed inside it - and
    /// anything the buffer misses falls back to a direct read, so this is never a
    /// correctness tradeoff, only a speed one.
    pub fn read_object(&self, base: u64, class: &str) -> Object<'_> {
        let mut bytes = match self.class_size(class) {
            Some(size) => vec![0u8; (size as usize).min(MAX_OBJECT_BYTES)],
            // No schema size: hand back an empty buffer. Every accessor falls through to
            // a live read, so this is slower, never wrong.
            None => Vec::new(),
        };
        let got = self.mem.read_partial(base, &mut bytes);
        bytes.truncate(got);
        Object {
            reader: self,
            base,
            bytes,
        }
    }

    /// Read one object, sized to cover exactly the fields named.
    ///
    /// [`Reader::read_object`] caps at 4 KiB, which suits the player
    /// controllers it was written for: large classes, and every field read from them sits
    /// low. `C_CitadelGameRules` breaks that assumption. A snapshot pulls two dozen fields
    /// from it, and some sit well past 4 KiB - where every accessor would fall through to
    /// a live read anyway and the bulk read would be pure waste on top.
    ///
    /// Sizing from the schema instead makes the bulk read worth taking wherever the fields
    /// actually are. When the schema cannot answer, the buffer is empty and every accessor
    /// falls through to a live read: slower, never wrong.
    pub fn read_object_spanning(&self, base: u64, class: &str, fields: &[&str]) -> Object<'_> {
        // The highest offset to be touched, plus room for the value sitting at it.
        let span = fields
            .iter()
            .filter_map(|f| self.offset_of(class, f))
            .max()
            .map(|hi| hi as usize + 8);
        let size = match (span, self.class_size(class)) {
            (Some(s), Some(declared)) => s.min(declared as usize),
            (Some(s), None) => s,
            (None, _) => 0,
        };
        let mut bytes = vec![0u8; size.min(HARD_OBJECT_CAP)];
        let got = self.mem.read_partial(base, &mut bytes);
        bytes.truncate(got);
        Object {
            reader: self,
            base,
            bytes,
        }
    }

    /// Size of a class according to the game's own schema.
    pub fn class_size(&self, class: &str) -> Option<u32> {
        self.schema
            .as_ref()?
            .classes
            .get(class)
            .map(|c| c.size)
            .filter(|&s| s > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::{Fallback, fallback_key, read_strided};
    use deadlock_memory::mock::MockMemory;

    /// `StatViewerModifierValues_t` as the runtime schema declares it: 64-byte elements
    /// with the three members at 0x30, 0x34 and 0x38.
    ///
    /// The wide gap before the first member is not padding this reader may skip. Every
    /// Citadel networked value type carries a 0x30 prologue, and the element size is the
    /// stride, so an accessor that assumed the members sat at the front of the record
    /// would read the prologue of the next element along.
    const STRIDE: u64 = 0x40;
    const DATA: u64 = 0x2000;

    fn three_elements() -> MockMemory {
        let mut mem = MockMemory::new(1);
        let mut buf = vec![0u8; (STRIDE * 3) as usize];
        for (i, (modifier, val_type, value)) in [(11u32, 1u32, 1.5f32), (22, 2, 2.5), (33, 3, 3.5)]
            .into_iter()
            .enumerate()
        {
            let at = i * STRIDE as usize;
            buf[at + 0x30..at + 0x34].copy_from_slice(&modifier.to_le_bytes());
            buf[at + 0x34..at + 0x38].copy_from_slice(&val_type.to_le_bytes());
            buf[at + 0x38..at + 0x3c].copy_from_slice(&value.to_le_bytes());
        }
        mem.write(DATA, &buf);
        mem
    }

    fn element(mem: &dyn deadlock_memory::mem::MemoryReader, at: u64) -> Option<(u32, u32, f32)> {
        Some((
            mem.read_u32(at + 0x30).ok()?,
            mem.read_u32(at + 0x34).ok()?,
            mem.read_f32(at + 0x38).ok()?,
        ))
    }

    #[test]
    fn a_strided_read_walks_by_the_element_size_not_by_the_fields_it_wants() {
        let mem = three_elements();
        let got = read_strided(&mem, 3, DATA, STRIDE, 64, element).expect("reads");
        assert_eq!(got, [(11, 1, 1.5), (22, 2, 2.5), (33, 3, 3.5)]);
    }

    #[test]
    fn a_strided_read_stops_at_the_limit() {
        let mem = three_elements();
        let got = read_strided(&mem, 3, DATA, STRIDE, 2, element).expect("reads");
        assert_eq!(got.len(), 2);
    }

    /// A short list is worse than no list: a consumer diffing one tick against the next
    /// reads the missing elements as deliberate removals.
    #[test]
    fn one_unreadable_element_fails_the_whole_strided_read() {
        let mem = three_elements();
        assert!(read_strided(&mem, 4, DATA, STRIDE, 64, element).is_none());
    }

    #[test]
    fn an_empty_strided_vector_is_an_empty_list_not_a_failure() {
        let mem = three_elements();
        let got = read_strided(&mem, 0, DATA, STRIDE, 64, element).expect("reads");
        assert!(got.is_empty());
    }

    /// The drift index answers "already recorded?" from this key alone, so anything it
    /// collides on is a report that never gets made.
    #[test]
    fn a_fallback_key_varies_on_every_part_of_what_it_identifies() {
        let base = fallback_key(Fallback::Stale, "C_Class", "m_field");
        assert_ne!(base, fallback_key(Fallback::Refused, "C_Class", "m_field"));
        assert_ne!(base, fallback_key(Fallback::Stale, "C_Other", "m_field"));
        assert_ne!(base, fallback_key(Fallback::Stale, "C_Class", "m_other"));
        assert_ne!(
            fallback_key(Fallback::Stale, "ab", "c"),
            fallback_key(Fallback::Stale, "a", "bc")
        );
        assert_eq!(base, fallback_key(Fallback::Stale, "C_Class", "m_field"));
    }
}
