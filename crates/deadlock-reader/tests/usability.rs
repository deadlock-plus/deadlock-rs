//! Contract tests for using this as an ordinary library dependency.
//!
//! These are the properties a CLI, a Tauri app or a server needs and that are easy to
//! break silently: thread-safety bounds, a real `Error` type, no panics on hostile input,
//! and the ability to drive the whole pipeline without a game running.
//!
//! Everything here is platform-independent and runs on Linux and macOS too.

use std::sync::Arc;

use deadlock_memory::mem::MemoryReader;
use deadlock_memory::mock::MockMemory;
use deadlock_reader::entity::{EntityLayout, EntitySnapshot, NameCache};
use deadlock_reader::globals::Globals;
use deadlock_reader::schema::{SchemaIndex, SchemaLayout};
use deadlock_reader::{Error, Reader};

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
fn assert_static<T: 'static>() {}

/// `tauri::State<T>` requires `T: Send + Sync + 'static`; axum/actix state is the same.
/// If these ever stop holding, the crate silently becomes unusable in the apps it is for.
#[test]
fn reader_is_usable_as_shared_app_state() {
    assert_send::<Reader>();
    assert_sync::<Reader>();
    assert_static::<Reader>();
    assert_send::<Arc<Reader>>();
    assert_sync::<Arc<Reader>>();
}

#[test]
fn public_types_are_send_and_sync() {
    assert_send::<Error>();
    assert_sync::<Error>();
    assert_send::<EntitySnapshot>();
    assert_sync::<EntitySnapshot>();
    assert_send::<deadlock_reader::snapshot::LiveSnapshot>();
    assert_sync::<deadlock_reader::snapshot::LiveSnapshot>();
    assert_send::<deadlock_reader::schema::SchemaIndex>();
    assert_sync::<deadlock_reader::schema::SchemaIndex>();
}

#[test]
fn error_is_a_real_std_error() {
    fn takes_std_error<E: std::error::Error + Send + Sync + 'static>(_: E) {}

    fn fallible() -> std::result::Result<(), Box<dyn std::error::Error>> {
        Err(Error::Memory(deadlock_memory::Error::ProcessNotFound(
            "deadlock.exe".into(),
        )))?;
        Ok(())
    }

    takes_std_error(Error::NotAttached);
    let msg = fallible().unwrap_err().to_string();
    assert!(msg.contains("deadlock.exe"), "{msg}");
}

/// Build a fake `client.dll` image containing the three signatures, so `Reader` can be
/// constructed end to end without Windows or a running game.
fn synthetic_client(base: u64) -> MockMemory {
    use deadlock_memory::sig::Pattern;
    use deadlock_reader::globals::SIGNATURES;

    const SIZE: usize = 0x4000;
    let mut image = vec![0xCCu8; SIZE];
    // Real PE magic, so ABI detection is exercised rather than falling back to the
    // module name.
    image[0] = 0x4D;
    image[1] = 0x5A;

    // Lay each signature down at a known offset, with a displacement chosen so the
    // resolved global lands inside the image.
    let mut at = 0x100usize;
    let mut targets = Vec::new();
    for d in SIGNATURES {
        let pat = Pattern::parse(d.pattern).unwrap();
        // Concrete bytes for the pattern: fixed bytes as-is, wildcards as zero.
        let mut bytes = vec![0u8; pat.len()];
        for (i, tok) in d.pattern.split_whitespace().enumerate() {
            if tok != "??" && tok != "?" {
                bytes[i] = u8::from_str_radix(tok, 16).unwrap();
            }
        }
        // Aim the reference at a scratch slot late in the image.
        let want = 0x3000u64 + targets.len() as u64 * 8;
        let next = (at + d.instr_len) as u64;
        let disp = (want as i64 - next as i64) as i32;
        bytes[d.disp_off..d.disp_off + 4].copy_from_slice(&disp.to_le_bytes());
        image[at..at + bytes.len()].copy_from_slice(&bytes);
        targets.push(base + want);
        at += 0x200;
    }

    let mut m = MockMemory::new(4321);
    m.add_module("client.dll", base, SIZE);
    m.write(base, &image);
    m
}

#[test]
fn signatures_resolve_out_of_a_synthetic_image() {
    let base = 0x1_0000_0000u64;
    let m = synthetic_client(base);
    let (image, read) = m.read_image(base, 0x4000);
    assert_eq!(read, 0x4000);

    let g = Globals::resolve(&image, base, 0x4000).expect("all three signatures resolve");
    for va in [g.entity_identity_list, g.entity_system, g.schema_system] {
        assert!((base..base + 0x4000).contains(&va), "{va:#x} outside image");
    }
    assert_ne!(g.entity_system, g.schema_system);
}

#[test]
fn reader_builds_over_a_mock_backend() {
    let base = 0x1_0000_0000u64;
    let m = synthetic_client(base);

    let r = Reader::with_memory(Arc::new(m), "client.dll").expect("reader builds");
    assert_eq!(r.pid(), 4321);
    assert_eq!(r.client_base(), base);

    assert!(r.schema().is_none());
    assert!(r.schema_error().is_some());
    assert_eq!(
        r.offset_of("C_CitadelPlayerPawn", "m_pGameSceneNode"),
        Some(0x330)
    );
    assert!(!r.offset_is_from_schema("C_CitadelPlayerPawn", "m_pGameSceneNode"));

    assert_eq!(r.offset_of("C_CitadelGameRulesProxy", "m_pGameRules"), None);
    assert!(
        r.drift().iter().any(|d| matches!(
            d,
            deadlock_reader::Drift::RefusedFallback { field, .. } if field == "m_pGameRules"
        )),
        "the refusal has to be reported, not silent"
    );

    assert_eq!(r.offset_of("C_Nonexistent", "m_nope"), None);
}

#[test]
fn missing_module_is_an_error_not_a_panic() {
    let m = MockMemory::new(1);
    let err = Reader::with_memory(Arc::new(m), "client.dll").unwrap_err();
    assert!(
        matches!(
            err,
            Error::Memory(deadlock_memory::Error::ModuleNotFound(_))
        ),
        "{err:?}"
    );
}

#[test]
fn signature_scan_failure_is_reported_cleanly() {
    let base = 0x2000_0000u64;
    let mut m = MockMemory::new(1);
    m.add_module("client.dll", base, 0x1000);
    m.write(base, &vec![0u8; 0x1000]); // no signatures present
    let err = Reader::with_memory(Arc::new(m), "client.dll").unwrap_err();
    assert!(
        matches!(
            err,
            Error::Memory(deadlock_memory::Error::SignatureNotFound(_))
        ),
        "{err:?}"
    );
}

#[test]
fn entity_walk_reads_a_synthetic_chunk() {
    let layout = EntityLayout::DEADLOCK;
    let stride = layout.identity_stride;
    let per_chunk = 1u64 << layout.chunk_shift;

    let (esys_global, esys, chunk) = (0x1000u64, 0x2000u64, 0x10_0000u64);
    let (inst_a, inst_b) = (0x50_0000u64, 0x50_1000u64);
    let (class_ptr, info_ptr, name_ptr, designer_ptr) =
        (0x60_0000u64, 0x60_1000u64, 0x60_2000u64, 0x60_3000u64);

    let mut m = MockMemory::new(7);
    m.write_u64(esys_global, esys);
    m.write_u64(esys + layout.chunk_table_base, chunk);

    let mut buf = vec![0u8; (per_chunk * stride) as usize];
    let mut put = |slot: u64, inst: u64| {
        let o = (slot * stride) as usize;
        buf[o..o + 8].copy_from_slice(&inst.to_le_bytes());
        let c = o + layout.identity_class as usize;
        buf[c..c + 8].copy_from_slice(&class_ptr.to_le_bytes());
        let h = o + layout.identity_handle as usize;
        buf[h..h + 4].copy_from_slice(&((slot as u32) | 0x8000).to_le_bytes());
        let d = o + layout.identity_designer_name as usize;
        buf[d..d + 8].copy_from_slice(&designer_ptr.to_le_bytes());
    };
    put(0, inst_a);
    put(2, inst_b);
    m.write(chunk, &buf);

    m.write_u64(class_ptr + layout.class_info_ptr, info_ptr);
    m.write_u64(info_ptr + layout.class_info_name, name_ptr);
    m.write_cstr(name_ptr, "C_CitadelPlayerPawn");
    m.write_cstr(designer_ptr, "player");

    let globals = Globals {
        entity_identity_list: 0,
        entity_system: esys_global,
        schema_system: 0,
    };
    let snap = EntitySnapshot::walk(&m, &globals, layout).expect("walk");

    assert_eq!(snap.len(), 2);
    assert_eq!(snap.by_index(0).unwrap().instance, inst_a);
    assert_eq!(snap.by_index(2).unwrap().instance, inst_b);
    assert!(snap.by_index(1).is_none());
    assert_eq!(
        &*snap.by_index(0).unwrap().class_name,
        "C_CitadelPlayerPawn"
    );
    assert_eq!(&*snap.by_index(0).unwrap().designer_name, "player");
    assert_eq!(snap.first_of_class("C_CitadelPlayerPawn").unwrap().index, 0);

    assert_eq!(snap.by_handle(0x8000 | 2).unwrap().instance, inst_b);
    assert_eq!(
        snap.class_breakdown(),
        vec![("C_CitadelPlayerPawn".into(), 2)]
    );
}

#[test]
fn name_cache_collapses_repeated_string_reads() {
    struct Counting {
        inner: MockMemory,
        reads: std::sync::atomic::AtomicUsize,
    }
    impl MemoryReader for Counting {
        fn pid(&self) -> u32 {
            self.inner.pid()
        }
        fn read_into(&self, addr: u64, buf: &mut [u8]) -> deadlock_memory::Result<()> {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.read_into(addr, buf)
        }
        fn read_partial(&self, addr: u64, buf: &mut [u8]) -> usize {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.read_partial(addr, buf)
        }
        fn modules(&self) -> deadlock_memory::Result<Vec<deadlock_memory::Module>> {
            self.inner.modules()
        }
        fn regions(&self) -> deadlock_memory::Result<Vec<deadlock_memory::Region>> {
            self.inner.regions()
        }
    }

    let layout = EntityLayout::DEADLOCK;
    let stride = layout.identity_stride;
    let per_chunk = 1u64 << layout.chunk_shift;
    let (esys_global, esys, chunk) = (0x1000u64, 0x2000u64, 0x10_0000u64);
    let (class_ptr, info_ptr, name_ptr, designer_ptr) =
        (0x60_0000u64, 0x60_1000u64, 0x60_2000u64, 0x60_3000u64);

    let mut inner = MockMemory::new(7);
    inner.write_u64(esys_global, esys);
    inner.write_u64(esys + layout.chunk_table_base, chunk);
    let mut buf = vec![0u8; (per_chunk * stride) as usize];
    for slot in 0..64u64 {
        let o = (slot * stride) as usize;
        buf[o..o + 8].copy_from_slice(&(0x50_0000u64 + slot * 0x100).to_le_bytes());
        let c = o + layout.identity_class as usize;
        buf[c..c + 8].copy_from_slice(&class_ptr.to_le_bytes());
        let d = o + layout.identity_designer_name as usize;
        buf[d..d + 8].copy_from_slice(&designer_ptr.to_le_bytes());
    }
    inner.write(chunk, &buf);
    inner.write_u64(class_ptr + layout.class_info_ptr, info_ptr);
    inner.write_u64(info_ptr + layout.class_info_name, name_ptr);
    inner.write_cstr(name_ptr, "C_CitadelPlayerPawn");
    inner.write_cstr(designer_ptr, "player");

    let mem = Counting {
        inner,
        reads: Default::default(),
    };
    let globals = Globals {
        entity_identity_list: 0,
        entity_system: esys_global,
        schema_system: 0,
    };
    let mut cache = NameCache::default();

    let first = EntitySnapshot::walk_cached(&mem, &globals, layout, &mut cache, 0).unwrap();
    let after_first = mem.reads.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(first.len(), 64);
    assert!(!cache.is_empty());

    mem.reads.store(0, std::sync::atomic::Ordering::Relaxed);
    let second = EntitySnapshot::walk_cached(&mem, &globals, layout, &mut cache, 0).unwrap();
    let after_second = mem.reads.load(std::sync::atomic::Ordering::Relaxed);

    assert_eq!(second.len(), 64);
    // 64 entities sharing one class and one designer string: the warm walk must not
    // re-read any strings, only the globals and the chunk itself.
    assert!(
        after_second < after_first,
        "cache did not help: {after_first} -> {after_second}"
    );
    assert!(
        after_second <= 8,
        "warm walk should be a handful of reads, was {after_second}"
    );
}

#[test]
fn schema_walk_rejects_a_bogus_layout_instead_of_returning_garbage() {
    let mut m = MockMemory::new(1);
    m.write_u64(0x1000, 0x2000); // schema system pointer
    m.write(0x2000, &vec![0u8; 0x800]); // all zeroes: no type scopes

    let err =
        deadlock_reader::schema::SchemaIndex::walk(&m, 0x1000, SchemaLayout::DEADLOCK).unwrap_err();
    assert!(matches!(err, Error::SchemaUnresolved(_)), "{err:?}");
}

#[test]
fn null_schema_pointer_is_reported_not_dereferenced() {
    let mut m = MockMemory::new(1);
    m.write_u64(0x1000, 0);
    let err =
        deadlock_reader::schema::SchemaIndex::walk(&m, 0x1000, SchemaLayout::DEADLOCK).unwrap_err();
    match err {
        Error::SchemaUnresolved(msg) => assert!(msg.contains("null"), "{msg}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn reader_can_be_polled_from_several_threads() {
    let base = 0x1_0000_0000u64;
    let r = Arc::new(Reader::with_memory(Arc::new(synthetic_client(base)), "client.dll").unwrap());

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let r = Arc::clone(&r);
            std::thread::spawn(move || {
                for _ in 0..50 {
                    let _ = r.entities();
                    assert_eq!(r.offset_of("CCitadelPlayerController", "m_steamID"), None);
                    assert_eq!(
                        r.offset_of("C_CitadelPlayerPawn", "m_pGameSceneNode"),
                        Some(0x330)
                    );
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("no panics or deadlocks across threads");
    }
}

/// The property the whole `Abi` split exists for: what tables apply is decided by the
/// *target* image, not the host. A Proton-hosted game read from Linux is a PE, is Win64,
/// and must get the MSVC offsets.
#[test]
fn proton_pe_selects_win64_tables_regardless_of_host() {
    use deadlock_reader::Abi;

    let base = 0x1_0000_0000u64;
    let r = Reader::with_memory(Arc::new(synthetic_client(base)), "client.dll").unwrap();

    assert_eq!(r.abi(), Abi::Win64);
    assert_eq!(r.offset_of("CBaseModifier", "m_flDuration"), Some(0x34));
    assert_eq!(
        r.offset_of("CCitadelPlayerController", "m_PlayerDataGlobal"),
        None
    );
    assert_eq!(r.entity_layout().identity_stride, 0x70);
}

/// A native ELF client must be refused with a clear error rather than silently read
/// using MSVC offsets, which would produce confident nonsense.
#[test]
fn native_elf_client_is_refused_not_misread() {
    use deadlock_reader::Abi;

    let base = 0x2000_0000u64;
    let mut image = vec![0u8; 0x1000];
    image[..4].copy_from_slice(b"\x7fELF");

    let mut m = MockMemory::new(99);
    m.add_module("client.so", base, 0x1000);
    m.write(base, &image);

    let err = Reader::with_memory(Arc::new(m), "client.so").unwrap_err();
    match err {
        Error::UnsupportedAbi { abi } => assert_eq!(abi, Abi::SysV),
        other => panic!("expected UnsupportedAbi, got {other:?}"),
    }
    assert!(
        Error::UnsupportedAbi { abi: Abi::SysV }
            .to_string()
            .contains("README")
    );
}

#[test]
fn sysv_never_leaks_msvc_offsets() {
    use deadlock_reader::Abi;
    for (class, field) in [
        ("C_CitadelGameRulesProxy", "m_pGameRules"),
        ("CCitadelPlayerController", "m_steamID"),
    ] {
        assert!(Abi::Win64.fallback_offset(class, field).is_some());
        assert_eq!(Abi::SysV.fallback_offset(class, field), None);
    }
}

/// Drives the Linux discovery path end to end using a recorded maps file: parse it,
/// group the PE's VMAs into a module, and build a `Reader` over that module. Only the
/// `process_vm_readv` call itself is not covered, and this runs on any host.
#[test]
fn proton_maps_drive_module_discovery() {
    use deadlock_memory::procmaps::{modules_from_maps, parse_maps, regions_from_maps};

    let base = 0x1_0000_0000u64;
    let maps = format!(
        "\
{base:x}-{a:x} r--p 00000000 08:02 100 /steamapps/common/Deadlock/game/citadel/bin/win64/client.dll
{a:x}-{b:x} r-xp 00001000 08:02 100 /steamapps/common/Deadlock/game/citadel/bin/win64/client.dll
{b:x}-{c:x} rw-p 00003000 08:02 100 /steamapps/common/Deadlock/game/citadel/bin/win64/client.dll
7f0000000000-7f0000021000 rw-p 00000000 00:00 0 
7f1000000000-7f1000002000 r--p 00000000 00:00 0 [vvar]
",
        base = base,
        a = base + 0x1000,
        b = base + 0x3000,
        c = base + 0x4000,
    );

    let entries = parse_maps(&maps);
    let modules = modules_from_maps(&entries);
    let client = modules
        .iter()
        .find(|m| m.name == "client.dll")
        .expect("client.dll grouped from its VMAs");
    assert_eq!(client.base, base);
    assert_eq!(client.size, 0x4000);

    let regions = regions_from_maps(&entries);
    assert!(regions.iter().any(|r| r.base == 0x7f00_0000_0000));
    assert!(regions.iter().any(|r| r.base == base + 0x3000));
    assert!(!regions.iter().any(|r| r.base == 0x7f10_0000_0000));

    let mut mem = synthetic_client(base);
    mem.add_region(0x7f00_0000_0000, 0x21000);
    let r = Reader::with_memory(Arc::new(mem), &client.name).unwrap();
    assert_eq!(r.abi(), deadlock_reader::Abi::Win64);
    assert_eq!(r.client_base(), base);
}

/// A failed schema walk sends every field through the baked table, so the drift log is
/// written to on the order of five hundred times a poll for entries it already holds. The
/// index that makes the repeats free is only correct if it still tells apart the things
/// that genuinely differ.
#[test]
fn repeated_fallbacks_are_recorded_once_without_conflating_distinct_fields() {
    let r = Reader::with_memory(Arc::new(synthetic_client(0x1_0000_0000u64)), "client.dll")
        .expect("reader builds");

    let stale = ("C_CitadelPlayerPawn", "m_pGameSceneNode");
    for _ in 0..50 {
        assert_eq!(r.offset_of(stale.0, stale.1), Some(0x330));
    }
    assert_eq!(
        r.drift().len(),
        1,
        "fifty resolutions of one field is one thing worth reporting"
    );

    assert!(
        r.offset_of("C_CitadelPlayerPawn", "m_CCitadelAbilityComponent")
            .is_some()
    );
    assert_eq!(r.stale_fields().len(), 2, "distinct fields stay distinct");

    assert_eq!(
        r.offset_of("CCitadelPlayerController", "m_PlayerDataGlobal"),
        None
    );
    let drift = r.drift();
    assert_eq!(drift.len(), 3);
    assert_eq!(
        drift
            .iter()
            .filter(|d| matches!(d, deadlock_reader::Drift::RefusedFallback { .. }))
            .count(),
        1
    );
}

/// `Object`'s accessors have two ways to answer: out of the bulk-read buffer, or by going
/// back to the process for a field the buffer does not reach. Those were five copies of
/// the same logic, one per width, which is precisely the shape where the two paths get to
/// drift apart on what a field means.
///
/// The read counter is what stops this being a test that passes either way: without it,
/// an `Object` whose buffer silently did nothing would still return the right values, via
/// the fallback, and look identical from the outside.
#[test]
fn buffered_and_live_reads_of_the_same_field_agree_at_every_width() {
    const CLASS: &str = "C_CitadelPlayerPawn";
    const FIELD: &str = "m_pGameSceneNode";

    let base = 0x1_0000_0000u64;
    let mut mem = synthetic_client(base);
    let object_at = base + 0x8000;
    let mut object = vec![0u8; 0x338];
    object[0x330..0x338].copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    mem.write(object_at, &object);

    let mem = Arc::new(mem);
    let r = Reader::with_memory(mem.clone() as Arc<dyn MemoryReader>, "client.dll")
        .expect("reader builds");
    assert_eq!(r.offset_of(CLASS, FIELD), Some(0x330), "fixture assumption");

    let live = r.read_object(object_at, CLASS);
    let buffered = r.read_object_spanning(object_at, CLASS, &[FIELD]);

    mem.reset_reads();
    assert_eq!(live.u8(CLASS, FIELD), Some(0x11));
    assert_eq!(live.u32(CLASS, FIELD), Some(0x4433_2211));
    assert_eq!(live.i32(CLASS, FIELD), Some(0x4433_2211));
    assert_eq!(live.u64(CLASS, FIELD), Some(0x8877_6655_4433_2211));
    assert_eq!(live.f32(CLASS, FIELD), Some(f32::from_bits(0x4433_2211)));
    assert_eq!(live.bool(CLASS, FIELD), Some(true));
    assert_eq!(
        mem.reads(),
        6,
        "an empty buffer means one read per accessor, which is the thing worth avoiding"
    );

    mem.reset_reads();
    assert_eq!(buffered.u8(CLASS, FIELD), live.u8(CLASS, FIELD));
    assert_eq!(buffered.u32(CLASS, FIELD), live.u32(CLASS, FIELD));
    assert_eq!(buffered.i32(CLASS, FIELD), live.i32(CLASS, FIELD));
    assert_eq!(buffered.u64(CLASS, FIELD), live.u64(CLASS, FIELD));
    assert_eq!(buffered.f32(CLASS, FIELD), live.f32(CLASS, FIELD));
    assert_eq!(buffered.bool(CLASS, FIELD), live.bool(CLASS, FIELD));
    assert_eq!(
        mem.reads(),
        6,
        "the buffered object answered from its own bytes; the six reads are the live half"
    );
}

/// Nine `pub` methods on [`Reader`] had no caller anywhere in the workspace - not in the
/// CLI, the overlay, the seventeen examples, or any test. They are not dead API: they are
/// the escape hatches the crate's whole drift design leans on, the way a consumer survives
/// a game patch without waiting for a release. But an untested escape hatch is the worst
/// kind, because it is only reached on the day something has already gone wrong.
///
/// This exercises all nine.
#[test]
fn the_patch_escape_hatches_all_work() {
    use deadlock_reader::tunables::Tunables;

    const CLASS: &str = "C_CitadelPlayerPawn";
    const FIELD: &str = "m_pGameSceneNode";

    let base = 0x1_0000_0000u64;
    let mut mem = synthetic_client(base);
    let object_at = base + 0x8000;
    let mut object = vec![0u8; 0x338];
    object[0x330..0x338].copy_from_slice(&0x0123_4567_89ab_cdefu64.to_le_bytes());
    mem.write(object_at, &object);

    let mem = Arc::new(mem);
    let mut r = Reader::with_memory(mem.clone() as Arc<dyn MemoryReader>, "client.dll")
        .expect("reader builds");

    assert_eq!(r.memory_arc().pid(), r.pid());

    assert_eq!(
        r.field_u64(object_at, CLASS, FIELD),
        Some(0x0123_4567_89ab_cdef)
    );
    let obj = r.read_object_spanning(object_at, CLASS, &[FIELD]);
    assert_eq!(obj.member_addr(CLASS, FIELD), Some(object_at + 0x330));

    assert!(
        r.has_stale_offsets(),
        "a baked-offset resolution has to be reportable"
    );

    r.clear_name_cache();

    let mut t = Tunables::default();
    t.max_items = 7;
    r.set_tunables(t);
    assert_eq!(r.tunables().max_items, 7);
    r.tunables_mut().max_items = 9;
    assert_eq!(r.tunables().max_items, 9);

    let good = r.entity_layout();
    let mut fatal = good;
    fatal.identity_stride = 0;
    assert!(
        r.set_entity_layout(fatal).is_err(),
        "a zero stride divides by zero during the walk; it must be refused at the door"
    );
    assert_eq!(
        r.entity_layout().identity_stride,
        good.identity_stride,
        "a rejected layout must not have been half-applied"
    );
    assert!(r.set_entity_layout(good).is_ok());

    assert!(r.refresh_schema().is_err());
    assert!(
        r.schema_error().is_some(),
        "a failed refresh has to leave the reason behind"
    );
    assert!(r.set_schema_layout(SchemaLayout::default()).is_err());
    assert!(r.schema().is_none());
}

/// A player name that runs past the end of the bulk-read buffer used to come back cut
/// short and indistinguishable from a complete one: someone called `AlexandriaTheGreat`
/// read back as "Alexandr", which is a perfectly plausible name. The buffered path may
/// only answer when it has actually seen the terminator.
#[test]
fn a_name_running_past_the_buffer_is_read_properly_rather_than_truncated() {
    const CLASS: &str = "C_CitadelPlayerPawn";
    const FIELD: &str = "m_pGameSceneNode";
    const AT: usize = 0x330;
    const NAME: &str = "AlexandriaTheGreat";

    let base = 0x1_0000_0000u64;
    let mut mem = synthetic_client(base);
    let object_at = base + 0x8000;
    let mut object = vec![0u8; AT + 64];
    object[AT..AT + NAME.len()].copy_from_slice(NAME.as_bytes());
    object[AT + NAME.len()] = 0;
    mem.write(object_at, &object);

    let mem = Arc::new(mem);
    let r = Reader::with_memory(mem.clone() as Arc<dyn MemoryReader>, "client.dll")
        .expect("reader builds");

    let buffered = r.read_object_spanning(object_at, CLASS, &[FIELD]);
    assert_eq!(
        buffered.cstr(CLASS, FIELD, 64).as_deref(),
        Some(NAME),
        "the buffer stopped mid-name, so this had to fall through to a live read"
    );

    mem.reset_reads();
    assert_eq!(
        buffered.cstr(CLASS, FIELD, 4).as_deref(),
        Some("Alex"),
        "`max` is the caller's own limit and is not a short read"
    );
    assert_eq!(
        mem.reads(),
        0,
        "a limit the buffer covers must be served from the buffer"
    );
}

/// Every address this crate computes is a base read out of another process plus a small
/// offset. A base near the top of the address space made that a plain `+` that overflowed:
/// a panic in every debug build - which is tests, examples and `cargo run` - and a silent
/// wrap in release. A hostile or merely corrupt pointer must produce a failed read, not a
/// crash.
#[test]
fn addresses_near_the_top_of_the_space_fail_reads_rather_than_panicking() {
    let m = MockMemory::new(1);

    for addr in [u64::MAX, u64::MAX - 1, u64::MAX - 4096, u64::MAX / 2] {
        assert!(m.read_u32(addr).is_err(), "{addr:#x}");
        assert!(m.read_u64(addr).is_err(), "{addr:#x}");
        assert!(m.read_ptr(addr).is_err(), "{addr:#x}");
        assert!(m.read_cstr(addr, 128).is_err(), "{addr:#x}");
        assert_eq!(m.region_at(addr), None, "{addr:#x}");
    }

    let mut m = MockMemory::new(1);
    m.write_u64(0x1000, u64::MAX - 8);
    let walked = SchemaIndex::discover_from(&m, 0x1000, SchemaLayout::default());
    assert!(
        walked.is_err(),
        "a schema system at the top of the address space is an error, not a panic"
    );
}
