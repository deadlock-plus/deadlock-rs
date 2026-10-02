//! Walking the Source 2 schema system to recover field offsets at runtime.
//!
//! # Provenance
//!
//! The original binary's schema walk (`sub_59e670`) operates on already-copied buffers,
//! so its internal struct offsets could not be recovered statically. The layout in
//! [`SchemaLayout::DEADLOCK`] was instead **derived empirically against a live Deadlock
//! client** by structural search: for each candidate offset, follow the pointer and check
//! whether it lands on something whose name field reads back as a plausible class
//! identifier. Re-derive them after a game update with `cargo run --example probe`.
//!
//! Confirmed against a running client (20 type scopes, 2914 classes in the `client.dll`
//! scope alone):
//!
//! ```text
//! CSchemaSystem     +0x190  CUtlVector<CSchemaSystemTypeScope*>  (count @ +0x190, data @ +0x198)
//! TypeScope         +0x008  char m_szScopeName[]                 "client.dll", "engine2.dll", ...
//!                   +0x5c0  HashBucket_t m_Buckets[256]          stride 0x18
//! HashBucket_t      +0x008  first entry      +0x010  first uncommitted entry
//! Entry             +0x008  next             +0x010  CSchemaClassBinding*
//! ClassBinding      +0x008  name  +0x020 size  +0x024 field count
//!                   +0x030  fields        +0x038 -> +0x008  base class binding
//! Field             stride 0x20, +0x000 name, +0x008 type, +0x010 offset
//! CSchemaType       +0x008  type name     +0x020  enum binding (when enum-typed)
//! EnumBinding       +0x008  name  +0x018 size  +0x01c count  +0x020 enumerators
//! Enumerator        stride 0x20, +0x000 name, +0x008 value
//! ```
//!
//! The class-binding table is a **256-bucket hash**, not the single linked list an
//! earlier draft assumed; that was the one thing the conventional layout got wrong.
//!
//! Enums are recovered by following a field's type pointer rather than by locating the
//! scope's declared-enum hash: it is more direct, and it only pays for enums actually
//! referenced by a field the walk visited. See [`SchemaIndex::enum_name_for`].
//!
//! A failed walk is still not fatal: [`crate::Reader::offset_of`] falls back to the
//! offsets baked into [`crate::fields::WIN64_FIELDS`]. Run `dlmr probe-schema` after a game
//! update to re-confirm.

use std::collections::HashMap;

use crate::error::{Error, Result};

/// A member address inside a struct that was read out of the target process.
///
/// `base` came from the game, so it can be anything at all, including a value near the top
/// of the address space where a plain `+` overflows - which panics in a debug build (every
/// test, example and `cargo run`) and wraps silently in release. Wrapping deliberately and
/// uniformly: a garbage base plus an offset is another garbage address, and the read that
/// follows is what reports it. Nothing here dereferences an address itself, so the cost of
/// a wrong one is a failed read.
#[inline]
fn at(base: u64, offset: u64) -> u64 {
    base.wrapping_add(offset)
}

/// The `i`th element of a strided array in the target process. See [`at`].
#[inline]
fn elem(base: u64, i: u64, stride: u64) -> u64 {
    at(base, i.wrapping_mul(stride))
}

use deadlock_memory::mem::MemoryReader;

/// Byte offsets describing the schema system's in-memory layout.
///
/// See the module docs: these were measured against a live client. Override any field
/// and re-run rather than editing the crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SchemaLayout {
    /// `CSchemaSystem` -> `CUtlVector<CSchemaSystemTypeScope*> m_TypeScopes`.
    pub system_type_scopes: u64,
    /// `CSchemaSystemTypeScope` -> `char m_szScopeName[256]`.
    pub scope_name: u64,
    /// `CSchemaSystemTypeScope` -> first `HashBucket_t` of the declared-class hash.
    pub scope_class_buckets: u64,
    /// Number of buckets in that hash.
    pub bucket_count: u64,
    /// `sizeof(HashBucket_t)`.
    pub bucket_stride: u64,
    /// `HashBucket_t` -> head of the committed entry chain.
    pub bucket_head: u64,
    /// `HashBucket_t` -> head of the uncommitted entry chain.
    pub bucket_head_uncommitted: u64,
    /// Entry -> `next` pointer.
    pub tshash_entry_next: u64,
    /// Entry -> payload pointer (`CSchemaClassBinding*`).
    pub tshash_entry_data: u64,
    /// `CSchemaClassBinding` -> `const char* m_pszName`.
    pub class_name: u64,
    /// `CSchemaClassBinding` -> `int m_nSize`.
    pub class_size: u64,
    /// `CSchemaClassBinding` -> `int16 m_nFieldCount`.
    pub class_field_count: u64,
    /// `CSchemaClassBinding` -> `CSchemaBaseClassInfo* m_pBaseClasses`.
    pub class_base_info: u64,
    /// `CSchemaBaseClassInfo` -> `CSchemaClassBinding*` of the base class.
    pub base_info_class: u64,
    /// `CSchemaClassBinding` -> `CSchemaClassFieldData* m_pFields`.
    pub class_fields: u64,
    /// `sizeof(CSchemaClassFieldData)`.
    pub field_stride: u64,
    /// Field -> `const char* m_pszName`.
    pub field_name: u64,
    /// Field -> `CSchemaType* m_pType`.
    pub field_type: u64,
    /// Field -> `int m_nSingleInheritanceOffset`.
    pub field_offset: u64,
    /// `CSchemaType` -> `const char*` naming the type.
    pub type_name: u64,
    /// `CSchemaType` -> `CSchemaEnumBinding*`, when the type is an enum.
    pub type_enum_binding: u64,
    /// `CSchemaEnumBinding` -> `const char* m_pszName`.
    pub enum_name: u64,
    /// `CSchemaEnumBinding` -> `uint8 m_nSize` (width of the underlying integer).
    pub enum_size: u64,
    /// `CSchemaEnumBinding` -> `uint32` enumerator count.
    pub enum_count: u64,
    /// `CSchemaEnumBinding` -> enumerator array.
    pub enum_values: u64,
    /// `sizeof(enumerator)`.
    pub enum_value_stride: u64,
    /// Enumerator -> `const char* m_pszName`.
    pub enum_value_name: u64,
    /// Enumerator -> `uint64 m_nValue`.
    pub enum_value_value: u64,
}

impl SchemaLayout {
    /// Layout confirmed against a live Deadlock client. See module docs.
    pub const DEADLOCK: SchemaLayout = SchemaLayout {
        system_type_scopes: 0x190,
        scope_name: 0x08,
        scope_class_buckets: 0x5c0,
        bucket_count: 256,
        bucket_stride: 0x18,
        bucket_head: 0x08,
        bucket_head_uncommitted: 0x10,
        tshash_entry_next: 0x08,
        tshash_entry_data: 0x10,
        class_name: 0x08,
        class_size: 0x20,
        class_field_count: 0x24,
        class_base_info: 0x38,
        base_info_class: 0x08,
        class_fields: 0x30,
        field_stride: 0x20,
        field_name: 0x00,
        field_type: 0x08,
        field_offset: 0x10,
        type_name: 0x08,
        type_enum_binding: 0x20,
        enum_name: 0x08,
        enum_size: 0x18,
        enum_count: 0x1C,
        enum_values: 0x20,
        enum_value_stride: 0x20,
        enum_value_name: 0x00,
        enum_value_value: 0x08,
    };

    /// Candidate `m_TypeScopes` offsets tried by [`SchemaIndex::discover`], in order.
    ///
    /// `0x190` is the confirmed value; the rest are swept so a game update that moves the
    /// vector can be recovered from automatically rather than needing a code change.
    pub const TYPE_SCOPE_CANDIDATES: &'static [u64] = &[
        0x190, 0x198, 0x1A0, 0x1B0, 0x1C0, 0x1D0, 0x1E0, 0x1F0, 0x200, 0x210, 0x220, 0x230, 0x240,
        0x250, 0x260, 0x270, 0x280, 0x2A0, 0x2C0, 0x2E0, 0x300, 0x320, 0x340, 0x360, 0x380, 0x3A0,
    ];

    /// Candidate bucket-array offsets, swept when the default yields nothing.
    pub const BUCKET_CANDIDATES: &'static [u64] = &[
        0x5c0, 0x5b0, 0x5d0, 0x5e0, 0x600, 0x620, 0x640, 0x540, 0x560, 0x580, 0x588,
    ];
}

impl Default for SchemaLayout {
    fn default() -> Self {
        Self::DEADLOCK
    }
}

/// Sanity bounds. A walk producing values outside these is rejected as a bad layout
/// rather than trusted, because a wrong offset yields plausible-looking garbage.
mod limits {
    /// Type scopes in a real client.
    pub const MAX_SCOPES: u32 = 64;
    /// Fields on one class.
    pub const MAX_FIELDS: i16 = 1024;
    /// Bytes in a class.
    pub const MAX_CLASS_SIZE: u32 = 1 << 22;
    /// Field offset within a class.
    pub const MAX_FIELD_OFFSET: u32 = 1 << 22;
    /// Classes walked before we assume we are chasing a corrupt chain.
    pub const MAX_CLASSES: usize = 40_000;
    /// Entries followed in one hash chain.
    pub const MAX_CHAIN: usize = 40_000;
    /// A walk yielding fewer classes than this is treated as a failed layout guess.
    pub const MIN_PLAUSIBLE_CLASSES: usize = 200;
    /// Base-class links followed before assuming a cycle.
    pub const MAX_INHERITANCE_DEPTH: usize = 32;
    /// Enumerators read from one enum binding.
    pub const MAX_ENUMERATORS: usize = 4096;
}

/// Scope whose definitions win when a class name appears in several scopes.
///
/// We read a client process, so `client.dll` offsets are the correct ones.
pub const PRIORITY_SCOPE: &str = "client.dll";

/// One class recovered from the schema.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ClassInfo {
    /// Schema class name.
    pub name: String,
    /// `sizeof(class)` as the schema reports it.
    pub size: u32,
    /// Immediate base class, if any.
    pub base: Option<String>,
    /// Field name -> offset. After [`SchemaIndex::flatten`] this includes inherited
    /// fields; before it, only the class's own.
    pub fields: HashMap<String, u32>,
    /// Fields declared directly on this class, excluding anything inherited.
    pub own_fields: usize,
}

/// An enum recovered from the schema, with its enumerators.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EnumInfo {
    /// Enum type name, e.g. `EGameState`.
    pub name: String,
    /// Width in bytes of the underlying integer.
    pub size: u8,
    /// Enumerator value -> name, e.g. `7 -> "EGameState_GameInProgress"`.
    pub values: HashMap<i64, String>,
}

/// A resolved `class -> field -> offset` index, plus the enums those fields use.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SchemaIndex {
    /// Every class the walk recovered, keyed by name.
    pub classes: HashMap<String, ClassInfo>,
    /// Every enum reachable from a walked field, keyed by enum type name.
    pub enums: HashMap<String, EnumInfo>,
    /// `(class, field)` -> enum type name, for enum-typed fields.
    /// Enum-typed fields, as `class -> field -> enum type name`.
    ///
    /// Nested rather than keyed on a `(String, String)` tuple: a tuple key cannot be
    /// looked up with a pair of `&str`, so every lookup had to allocate two `String`s
    /// first. A snapshot does this per poll, ten times a second, for a lookup that needs
    /// no allocation at all.
    pub field_enums: HashMap<String, HashMap<String, String>>,
    /// Scope names encountered, in merge order.
    pub scopes: Vec<String>,
    /// The layout that produced this index.
    pub layout: SchemaLayout,
}

fn plausible_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 128
        && s.as_bytes()[0].is_ascii_alphabetic()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b':')
}

impl SchemaIndex {
    /// Walk the schema system using an explicit layout.
    ///
    /// `schema_system_global` is the *address of the pointer variable* resolved from the
    /// `schema_system` signature; this dereferences it first.
    pub fn walk(
        proc: &dyn MemoryReader,
        schema_system_global: u64,
        layout: SchemaLayout,
    ) -> Result<SchemaIndex> {
        let system = proc
            .read_ptr(schema_system_global)
            .map_err(|e| Error::SchemaUnresolved(format!("could not deref schema system: {e}")))?;
        if system == 0 {
            return Err(Error::SchemaUnresolved(
                "schema system pointer is null".into(),
            ));
        }

        let vec_base = at(system, layout.system_type_scopes);
        let count = proc
            .read_u32(vec_base)
            .map_err(|e| Error::SchemaUnresolved(format!("type-scope count unreadable: {e}")))?;
        let elems = proc
            .read_ptr(at(vec_base, 8))
            .map_err(|e| Error::SchemaUnresolved(format!("type-scope data unreadable: {e}")))?;

        if count == 0 || count > limits::MAX_SCOPES || elems == 0 {
            return Err(Error::SchemaUnresolved(format!(
                "implausible type-scope vector (count={count}, data={elems:#x})"
            )));
        }

        let mut index = SchemaIndex {
            classes: HashMap::new(),
            enums: HashMap::new(),
            field_enums: HashMap::new(),
            scopes: Vec::new(),
            layout,
        };

        // Gather scopes first so we can control merge order. 2637 class names exist in
        // BOTH client.dll and server.dll with *different* offsets; since we are reading
        // a client process, client.dll must win. Walking it first plus keep-first
        // semantics achieves that.
        let mut scopes = Vec::new();
        for i in 0..count as u64 {
            let scope = match proc.read_ptr(elem(elems, i, 8)) {
                Ok(p) if p != 0 => p,
                _ => continue,
            };
            let name = proc
                .read_cstr(at(scope, layout.scope_name), 256)
                .unwrap_or_default();
            scopes.push((name, scope));
        }
        scopes.sort_by_key(|(name, _)| !name.eq_ignore_ascii_case(PRIORITY_SCOPE));

        for (name, scope) in &scopes {
            index.scopes.push(name.clone());
            // A single unreadable scope should not abandon the others.
            let _ = index.walk_scope(proc, *scope);
            if index.classes.len() > limits::MAX_CLASSES {
                break;
            }
        }

        if index.classes.len() < limits::MIN_PLAUSIBLE_CLASSES {
            return Err(Error::SchemaUnresolved(format!(
                "schema walk produced only {} classes; layout is probably wrong",
                index.classes.len()
            )));
        }
        index.flatten();
        Ok(index)
    }

    /// Fold inherited fields into each class.
    ///
    /// A Source 2 class binding lists only the fields declared directly on that class:
    /// `CCitadelPlayerController` has `m_PlayerDataGlobal`, but `m_iTeamNum` and
    /// `m_iHealth` live up on `C_BaseEntity`. Without this pass, lookups for inherited
    /// fields silently miss and fall through to the baked table.
    ///
    /// Own fields always win over inherited ones.
    pub fn flatten(&mut self) {
        let names: Vec<String> = self.classes.keys().cloned().collect();
        for name in names {
            let mut inherited: Vec<HashMap<String, u32>> = Vec::new();
            let mut cur = self.classes.get(&name).and_then(|c| c.base.clone());
            let mut guard = 0;
            while let Some(b) = cur {
                guard += 1;
                if guard > limits::MAX_INHERITANCE_DEPTH || b == name {
                    break;
                }
                let Some(bc) = self.classes.get(&b) else {
                    break;
                };
                inherited.push(bc.fields.clone());
                cur = bc.base.clone();
            }
            if let Some(c) = self.classes.get_mut(&name) {
                for level in inherited {
                    for (k, v) in level {
                        c.fields.entry(k).or_insert(v);
                    }
                }
            }
        }
    }

    fn walk_scope(&mut self, proc: &dyn MemoryReader, scope: u64) -> Result<()> {
        let l = self.layout;

        // The declared-class table is a bucket array, each bucket holding two chain
        // heads (committed and uncommitted). Both usually point at the same entry.
        for b in 0..l.bucket_count {
            let bucket = elem(at(scope, l.scope_class_buckets), b, l.bucket_stride);
            for head in [l.bucket_head, l.bucket_head_uncommitted] {
                let mut entry = match proc.read_ptr(at(bucket, head)) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let mut steps = 0usize;
                while entry != 0 && steps < limits::MAX_CHAIN {
                    steps += 1;
                    if let Ok(binding) = proc.read_ptr(at(entry, l.tshash_entry_data))
                        && binding != 0
                    {
                        let _ = self.read_class(proc, binding);
                    }
                    entry = match proc.read_ptr(at(entry, l.tshash_entry_next)) {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    if self.classes.len() > limits::MAX_CLASSES {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    fn read_class(&mut self, proc: &dyn MemoryReader, binding: u64) -> Result<()> {
        let l = self.layout;

        let name_ptr = proc.read_ptr(at(binding, l.class_name))?;
        if name_ptr == 0 {
            return Ok(());
        }
        let name = proc.read_cstr(name_ptr, 128)?;
        if !plausible_ident(&name) {
            return Ok(());
        }
        if self.classes.contains_key(&name) {
            return Ok(());
        }

        let size = proc.read_u32(at(binding, l.class_size))?;
        let field_count = proc.read_u16(at(binding, l.class_field_count))? as i16;
        let fields_ptr = proc.read_ptr(at(binding, l.class_fields))?;

        if size > limits::MAX_CLASS_SIZE || !(0..=limits::MAX_FIELDS).contains(&field_count) {
            return Ok(());
        }

        // Immediate base class, for the flattening pass.
        let base = proc
            .read_ptr(at(binding, l.class_base_info))
            .ok()
            .filter(|&p| p != 0)
            .and_then(|bi| proc.read_ptr(at(bi, l.base_info_class)).ok())
            .filter(|&p| p != 0)
            .and_then(|bb| proc.read_ptr(at(bb, l.class_name)).ok())
            .filter(|&p| p != 0)
            .and_then(|np| proc.read_cstr(np, 128).ok())
            .filter(|s| plausible_ident(s));

        let mut fields = HashMap::new();
        if fields_ptr != 0 {
            for i in 0..field_count as u64 {
                let f = elem(fields_ptr, i, l.field_stride);
                let fname_ptr = match proc.read_ptr(at(f, l.field_name)) {
                    Ok(p) if p != 0 => p,
                    _ => continue,
                };
                let fname = match proc.read_cstr(fname_ptr, 128) {
                    Ok(s) if plausible_ident(&s) => s,
                    _ => continue,
                };
                let off = match proc.read_u32(at(f, l.field_offset)) {
                    Ok(o) if o <= limits::MAX_FIELD_OFFSET => o,
                    _ => continue,
                };
                self.read_field_enum(proc, &name, &fname, f);
                fields.insert(fname, off);
            }
        }

        let own_fields = fields.len();
        self.classes.insert(
            name.clone(),
            ClassInfo {
                name,
                size,
                base,
                fields,
                own_fields,
            },
        );
        Ok(())
    }

    /// If `field` is enum-typed, record the enum and its enumerators.
    ///
    /// The route is `field -> CSchemaType -> CSchemaEnumBinding`. Following the field's
    /// type is much more direct than hunting for the scope's declared-enum hash table,
    /// and it only pays for enums actually referenced by a field we walked.
    ///
    /// The type record is validated by requiring the name it carries to match the name
    /// on the binding it points at; without that check, non-enum types resolve to
    /// whatever happens to sit at that offset.
    fn read_field_enum(
        &mut self,
        proc: &dyn MemoryReader,
        class: &str,
        field: &str,
        field_rec: u64,
    ) {
        let l = self.layout;
        let Some(ty) = proc
            .read_ptr(at(field_rec, l.field_type))
            .ok()
            .filter(|&p| p != 0)
        else {
            return;
        };
        let Some(ty_name) = proc
            .read_ptr(at(ty, l.type_name))
            .ok()
            .filter(|&p| p != 0)
            .and_then(|p| proc.read_cstr(p, 128).ok())
            .filter(|s| plausible_ident(s))
        else {
            return;
        };
        let Some(binding) = proc
            .read_ptr(at(ty, l.type_enum_binding))
            .ok()
            .filter(|&p| p != 0)
        else {
            return;
        };
        // The binding must agree with the type about its own name.
        let ok = proc
            .read_ptr(at(binding, l.enum_name))
            .ok()
            .filter(|&p| p != 0)
            .and_then(|p| proc.read_cstr(p, 128).ok())
            .map(|n| n == ty_name)
            .unwrap_or(false);
        if !ok {
            return;
        }

        self.field_enums
            .entry(class.to_string())
            .or_default()
            .insert(field.to_string(), ty_name.clone());
        if self.enums.contains_key(&ty_name) {
            return;
        }

        let size = proc.read_u8(at(binding, l.enum_size)).unwrap_or(0);
        let count = proc.read_u32(at(binding, l.enum_count)).unwrap_or(0);
        if count == 0 || count as usize > limits::MAX_ENUMERATORS {
            return;
        }
        let Some(arr) = proc
            .read_ptr(at(binding, l.enum_values))
            .ok()
            .filter(|&p| p != 0)
        else {
            return;
        };

        let mut values = HashMap::new();
        for i in 0..count as u64 {
            let e = elem(arr, i, l.enum_value_stride);
            let Some(nm) = proc
                .read_ptr(at(e, l.enum_value_name))
                .ok()
                .filter(|&p| p != 0)
                .and_then(|p| proc.read_cstr(p, 128).ok())
                .filter(|s| plausible_ident(s))
            else {
                continue;
            };
            let Ok(v) = proc.read_u64(at(e, l.enum_value_value)) else {
                continue;
            };
            values.insert(v as i64, nm);
        }
        if !values.is_empty() {
            self.enums.insert(
                ty_name.clone(),
                EnumInfo {
                    name: ty_name,
                    size,
                    values,
                },
            );
        }
    }

    /// Name of `value` within the enum used by `class::field`, if known.
    ///
    /// ```no_run
    /// # use deadlock_reader::schema::SchemaIndex;
    /// # let idx = SchemaIndex::default();
    /// // -> Some("EGameState_GameInProgress")
    /// idx.enum_name_for("C_CitadelGameRules", "m_eGameState", 7);
    /// ```
    pub fn enum_name_for(&self, class: &str, field: &str, value: i64) -> Option<&str> {
        let ty = self.field_enums.get(class)?.get(field)?;
        self.enums.get(ty)?.values.get(&value).map(String::as_str)
    }

    /// The enum type backing `class::field`, if it is enum-typed.
    pub fn enum_of(&self, class: &str, field: &str) -> Option<&EnumInfo> {
        let ty = self.field_enums.get(class)?.get(field)?;
        self.enums.get(ty)
    }

    /// Try the confirmed layout first, then sweep candidate offsets until one validates.
    ///
    /// Returns the index plus the layout that produced it, so it can be pinned.
    pub fn discover(
        proc: &dyn MemoryReader,
        schema_system_global: u64,
    ) -> Result<(SchemaIndex, SchemaLayout)> {
        Self::discover_from(proc, schema_system_global, SchemaLayout::DEADLOCK)
    }

    /// As [`SchemaIndex::discover`], but sweeping around an explicit base layout.
    ///
    /// The base supplies every offset except the two that are swept, so a different ABI
    /// can be probed by passing its own starting point.
    pub fn discover_from(
        proc: &dyn MemoryReader,
        schema_system_global: u64,
        base: SchemaLayout,
    ) -> Result<(SchemaIndex, SchemaLayout)> {
        let mut last = Error::SchemaUnresolved("no candidate layout tried".into());
        for &scopes in SchemaLayout::TYPE_SCOPE_CANDIDATES {
            for &buckets in SchemaLayout::BUCKET_CANDIDATES {
                let layout = SchemaLayout {
                    system_type_scopes: scopes,
                    scope_class_buckets: buckets,
                    ..base
                };
                match SchemaIndex::walk(proc, schema_system_global, layout) {
                    Ok(idx) => return Ok((idx, layout)),
                    Err(e) => last = e,
                }
            }
        }
        Err(last)
    }

    /// Offset of `field` within `class`, if the walk found it.
    pub fn offset_of(&self, class: &str, field: &str) -> Option<u32> {
        self.classes.get(class)?.fields.get(field).copied()
    }

    /// Number of classes recovered.
    pub fn class_count(&self) -> usize {
        self.classes.len()
    }

    /// Class names containing `needle`, case-sensitive. Mirrors the original's
    /// `find_classes(substr)` debug command.
    pub fn find_classes(&self, needle: &str) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .classes
            .keys()
            .filter(|k| k.contains(needle))
            .map(std::string::String::as_str)
            .collect();
        v.sort_unstable();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_filter() {
        assert!(plausible_ident("C_CitadelPlayerPawn"));
        assert!(plausible_ident("m_iHealth"));
        assert!(!plausible_ident(""));
        assert!(!plausible_ident("9bad"));
        assert!(!plausible_ident("has space"));
        assert!(!plausible_ident(&"x".repeat(200)));
    }

    /// Guards the constants measured against a live client; see module docs.
    #[test]
    fn default_layout_is_the_measured_one() {
        let l = SchemaLayout::default();
        assert_eq!(l.system_type_scopes, 0x190);
        assert_eq!(l.scope_name, 0x08);
        assert_eq!(l.scope_class_buckets, 0x5c0);
        assert_eq!(l.bucket_count, 256);
        assert_eq!(l.bucket_stride, 0x18);
        assert_eq!(l.class_name, 0x08);
        assert_eq!(l.class_base_info, 0x38);
        assert_eq!(l.field_stride, 0x20);
        assert_eq!(l.field_offset, 0x10);
        assert_eq!(l.bucket_count * l.bucket_stride, 0x1800);
    }

    /// Inherited fields must fold downwards, with own fields winning.
    #[test]
    fn flatten_folds_base_class_fields() {
        let mut idx = SchemaIndex::default();
        let mk = |name: &str, base: Option<&str>, fields: &[(&str, u32)]| ClassInfo {
            name: name.into(),
            size: 0,
            base: base.map(Into::into),
            fields: fields.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            own_fields: fields.len(),
        };
        idx.classes.insert(
            "C_BaseEntity".into(),
            mk(
                "C_BaseEntity",
                None,
                &[("m_iTeamNum", 0x3f3), ("m_iHealth", 0x354)],
            ),
        );
        idx.classes.insert(
            "CCitadelPlayerController".into(),
            mk(
                "CCitadelPlayerController",
                Some("C_BaseEntity"),
                &[("m_PlayerDataGlobal", 0x8f0), ("m_iHealth", 0x999)],
            ),
        );
        idx.flatten();

        assert_eq!(
            idx.offset_of("CCitadelPlayerController", "m_iTeamNum"),
            Some(0x3f3)
        );
        assert_eq!(
            idx.offset_of("CCitadelPlayerController", "m_iHealth"),
            Some(0x999)
        );
        assert_eq!(idx.offset_of("C_BaseEntity", "m_iHealth"), Some(0x354));
    }

    /// A cycle in the base chain must not hang the flatten pass.
    #[test]
    fn flatten_survives_a_cycle() {
        let mut idx = SchemaIndex::default();
        for (a, b) in [("A", "B"), ("B", "A")] {
            idx.classes.insert(
                a.into(),
                ClassInfo {
                    name: a.into(),
                    size: 0,
                    base: Some(b.into()),
                    fields: [(format!("f_{a}"), 4u32)].into_iter().collect(),
                    own_fields: 1,
                },
            );
        }
        idx.flatten();
        assert_eq!(idx.offset_of("A", "f_B"), Some(4));
    }

    #[test]
    fn candidate_list_contains_default() {
        assert!(
            SchemaLayout::TYPE_SCOPE_CANDIDATES
                .contains(&SchemaLayout::DEADLOCK.system_type_scopes)
        );
    }
}
