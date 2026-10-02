//! Re-derive the schema and entity struct layouts from a live client.
//!
//! ```text
//! cargo run --release --example probe
//! ```
//!
//! Run this after a Deadlock update. It rediscovers every layout constant by structural
//! search rather than trusting the baked values, then prints what it found next to what
//! the crate currently assumes. If a line says MISMATCH, update
//! [`SchemaLayout::DEADLOCK`] or [`EntityLayout::DEADLOCK`].
//!
//! Read-only, like everything else in this crate.

#[cfg(not(windows))]
fn main() {
    eprintln!("probe attaches to a live Windows game process; see README for porting notes.");
}

#[cfg(windows)]
use std::collections::HashMap;

#[cfg(windows)]
use deadlock_memory::mem::MemoryReader;
#[cfg(windows)]
use deadlock_memory::process::Process;
#[cfg(windows)]
use deadlock_reader::entity::EntityLayout;
#[cfg(windows)]
use deadlock_reader::schema::SchemaLayout;
#[cfg(windows)]
use deadlock_reader::{CLIENT_MODULE, DEADLOCK_PROCESS, globals::Globals};

#[cfg(windows)]
const USER_SPACE: std::ops::RangeInclusive<u64> = 0x1_0000..=0x7FFF_FFFF_FFFF;

#[cfg(windows)]
fn ptr(p: &Process, a: u64) -> Option<u64> {
    let v = p.read_ptr(a).ok()?;
    USER_SPACE.contains(&v).then_some(v)
}

/// Read a pointer *at* `v`, having first checked `v` itself is a sane address.
#[cfg(windows)]
fn deref(p: &Process, v: u64) -> Option<u64> {
    if !USER_SPACE.contains(&v) {
        return None;
    }
    let x = p.read_ptr(v).ok()?;
    USER_SPACE.contains(&x).then_some(x)
}

#[cfg(windows)]
fn ident(p: &Process, a: u64) -> Option<String> {
    let s = p.read_cstr(a, 96).ok()?;
    let ok = s.len() >= 3
        && s.is_ascii()
        && s.as_bytes()[0].is_ascii_alphabetic()
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_');
    ok.then_some(s)
}

#[cfg(windows)]
fn check<T: Copy + std::fmt::Debug + PartialEq>(label: &str, found: T, assumed: T) {
    let tag = if found == assumed { "ok" } else { "MISMATCH" };
    println!("  {label:<34} found {found:<10?} assumed {assumed:<10?}  {tag}");
}

#[cfg(windows)]
fn main() {
    let proc = match Process::attach(DEADLOCK_PROCESS) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("attach: {e} (is Deadlock running?)");
            std::process::exit(1);
        }
    };
    let client = proc.module(CLIENT_MODULE).unwrap();
    let (image, _) = proc.read_image(client.base, client.size);
    let g = Globals::resolve(&image, client.base, client.size).expect("signature scan");
    let (lo, hi) = (client.base, client.base + client.size as u64);
    let sl = SchemaLayout::DEADLOCK;
    let el = EntityLayout::DEADLOCK;

    println!("client.dll {:#x}  {:#x?}\n", client.base, g);

    probe_schema(&proc, &g, &sl);
    probe_entities(&proc, &g, &el, lo, hi);
}

/// Locate the schema system's type-scope vector and class bucket array, and compare
/// what is found against the baked [`SchemaLayout`].
#[cfg(windows)]
fn probe_schema(proc: &Process, g: &Globals, sl: &SchemaLayout) {
    println!("=== schema ===");
    let sys = proc.read_ptr(g.schema_system).expect("deref schema system");

    // Type-scope vector: a small count next to a pointer whose targets carry ".dll" names.
    let mut scopes_off = None;
    for off in (0..0x800u64).step_by(8) {
        let Ok(count) = proc.read_u32(sys + off) else {
            continue;
        };
        if !(1..=64).contains(&count) {
            continue;
        }
        let Some(data) = ptr(proc, sys + off + 8) else {
            continue;
        };
        let named = (0..count.min(6) as u64)
            .filter_map(|i| ptr(proc, data + i * 8))
            .filter(|&s| {
                proc.read_cstr(s + 0x08, 32)
                    .unwrap_or_default()
                    .ends_with(".dll")
            })
            .count();
        if named >= 3 {
            scopes_off = Some(off);
            break;
        }
    }
    let Some(scopes_off) = scopes_off else {
        println!("  could not locate the type-scope vector; schema layout needs manual work");
        return;
    };
    check(
        "CSchemaSystem::m_TypeScopes",
        scopes_off,
        sl.system_type_scopes,
    );

    let count = proc.read_u32(sys + scopes_off).unwrap();
    let data = proc.read_ptr(sys + scopes_off + 8).unwrap();
    let mut client_scope = 0;
    let mut names = Vec::new();
    for i in 0..count as u64 {
        let s = proc.read_ptr(data + i * 8).unwrap_or(0);
        let n = proc.read_cstr(s + sl.scope_name, 64).unwrap_or_default();
        if n == "client.dll" {
            client_scope = s;
        }
        names.push(n);
    }
    println!("  {count} scopes: {}", names.join(", "));

    // Bucket array: find the offset where consecutive 0x18-strided slots hold entries
    // whose +0x10 payload has a class name at +0x08.
    let looks_like_entry = |e: u64| -> bool {
        ptr(proc, e + 0x10)
            .and_then(|b| deref(proc, b + 0x08))
            .and_then(|n| ident(proc, n))
            .map(|n| n.starts_with('C'))
            .unwrap_or(false)
    };
    // Fully walk each candidate base and keep the one that recovers the most classes.
    //
    // Two heuristics are needed here. Requiring *both* chain heads of a bucket to agree
    // pins the phase (testing one head alone aliases onto a base 0x40 lower, landing on
    // the +0x08 slots of the real buckets). And a scope holds several 0x18-strided hash
    // tables back to back - declared classes, declared enums - which are phase-aligned
    // with each other, so "first plausible base" is not good enough; only a full walk
    // tells them apart.
    let walk_from = |base: u64| -> HashMap<String, u64> {
        let mut binds = HashMap::new();
        for b in 0..sl.bucket_count {
            let bucket = client_scope + base + b * sl.bucket_stride;
            for head in [sl.bucket_head, sl.bucket_head_uncommitted] {
                let mut e = proc.read_ptr(bucket + head).unwrap_or(0);
                let mut steps = 0;
                while e != 0 && steps < 4096 {
                    steps += 1;
                    if let Some(bind) = ptr(proc, e + sl.tshash_entry_data)
                        && let Some(n) =
                            deref(proc, bind + sl.class_name).and_then(|n| ident(proc, n))
                    {
                        binds.insert(n, bind);
                    }
                    e = proc.read_ptr(e + sl.tshash_entry_next).unwrap_or(0);
                }
            }
        }
        binds
    };

    let mut candidates = Vec::new();
    for off in (0x100..0x1000u64).step_by(8) {
        let hits = (0..16u64)
            .filter(|b| {
                let bucket = client_scope + off + b * sl.bucket_stride;
                match (
                    ptr(proc, bucket + sl.bucket_head),
                    ptr(proc, bucket + sl.bucket_head_uncommitted),
                ) {
                    (Some(a), Some(c)) => a == c && looks_like_entry(a),
                    _ => false,
                }
            })
            .count();
        if hits >= 4 {
            candidates.push(off);
        }
    }

    let mut best: Option<(usize, u64, HashMap<String, u64>)> = None;
    for &off in &candidates {
        let binds = walk_from(off);
        // The winner must contain the classes we actually read.
        let anchored = [
            "C_BaseEntity",
            "CCitadelPlayerController",
            "C_CitadelPlayerPawn",
        ]
        .iter()
        .all(|c| binds.contains_key(*c));
        println!(
            "  candidate m_Buckets @ {off:#06x}: {} names, anchors {}",
            binds.len(),
            if anchored { "present" } else { "MISSING" }
        );
        if anchored && best.as_ref().map(|b| binds.len() > b.0).unwrap_or(true) {
            best = Some((binds.len(), off, binds));
        }
    }

    let Some((_, buckets_off, binds)) = best else {
        println!("  MISMATCH: no candidate bucket array yielded the expected classes");
        return;
    };
    check("TypeScope::m_Buckets", buckets_off, sl.scope_class_buckets);
    println!("  client.dll scope: {} classes", binds.len());

    if let Some(&b) = binds.get("C_BaseEntity") {
        let fields = proc.read_ptr(b + sl.class_fields).unwrap_or(0);
        let fc = proc.read_u16(b + sl.class_field_count).unwrap_or(0);
        println!(
            "  C_BaseEntity: size {:#x}, {fc} own fields",
            proc.read_u32(b + sl.class_size).unwrap_or(0)
        );
        let base_name = ptr(proc, b + sl.class_base_info)
            .and_then(|bi| ptr(proc, bi + sl.base_info_class))
            .and_then(|bb| deref(proc, bb + sl.class_name))
            .and_then(|n| ident(proc, n));
        println!("  C_BaseEntity base class: {base_name:?} (expect CEntityInstance)");
        for i in 0..fc.min(6) as u64 {
            let f = fields + i * sl.field_stride;
            let n = ptr(proc, f + sl.field_name).and_then(|p| ident(proc, p));
            let o = proc.read_u32(f + sl.field_offset).unwrap_or(0);
            println!("    {n:?} @ {o:#x}");
        }
    }
}

/// Recover the entity system's identity stride and chunk table, and compare what is
/// found against the baked [`EntityLayout`].
#[cfg(windows)]
fn probe_entities(proc: &Process, g: &Globals, el: &EntityLayout, lo: u64, hi: u64) {
    let in_client = |v: u64| v >= lo && v < hi;
    println!("\n=== entities ===");
    let esys = proc.read_ptr(g.entity_system).unwrap_or(0);
    let valid_at = |chunk: u64, stride: u64| -> usize {
        (0..512u64)
            .filter(|i| {
                ptr(proc, chunk + i * stride)
                    .and_then(|inst| ptr(proc, inst))
                    .map(&in_client)
                    .unwrap_or(false)
            })
            .count()
    };

    let Some(chunk0) = ptr(proc, esys + el.chunk_table_base) else {
        println!("  no chunk at entity_system+{:#x}", el.chunk_table_base);
        return;
    };
    let mut best = (0usize, 0u64);
    for stride in [0x50u64, 0x58, 0x60, 0x68, 0x70, 0x78, 0x80, 0x88, 0x130] {
        let n = valid_at(chunk0, stride);
        println!("  stride {stride:#05x}: {n:>3}/512 valid");
        if n > best.0 {
            best = (n, stride);
        }
    }
    check("sizeof(CEntityIdentity)", best.1, el.identity_stride);

    let populated = (0..64u64)
        .filter_map(|c| ptr(proc, esys + el.chunk_table_base + c * el.chunk_table_stride))
        .filter(|&c| valid_at(c, el.identity_stride) > 0)
        .count();
    println!("  {populated}/64 chunks populated");

    // Class-name path: identity+class -> info -> name.
    let mut named = 0;
    let mut total = 0;
    let mut sample = Vec::new();
    for i in 0..256u64 {
        let id = chunk0 + i * el.identity_stride;
        let Some(inst) = ptr(proc, id) else { continue };
        if ptr(proc, inst).map(&in_client) != Some(true) {
            continue;
        }
        total += 1;
        let n = ptr(proc, id + el.identity_class)
            .and_then(|c| ptr(proc, c + el.class_info_ptr))
            .and_then(|i2| deref(proc, i2 + el.class_info_name))
            .and_then(|p| ident(proc, p));
        if let Some(n) = n {
            named += 1;
            if sample.len() < 6 && !sample.contains(&n) {
                sample.push(n);
            }
        }
    }
    println!("  class names resolved: {named}/{total}  e.g. {sample:?}");
    if named * 2 < total {
        println!("  MISMATCH: class-name path is not resolving; check class_info_* offsets");
    }
}
