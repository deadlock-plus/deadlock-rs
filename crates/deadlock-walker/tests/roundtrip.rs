//! End to end: fake C++ object graph -> RTTI -> heap search -> walker -> wire bytes ->
//! `valveprotos` -> compare with the original message.

mod support;

use deadlock_memory::mem::MemoryReader;
use deadlock_walker::{
    Error, Limits, PeImage, SearchConfig, VtableResolver, Walker, find_instances,
};
use prost::Message;
use support::*;
use valveprotos::deadlock::CMsgMatchMetaDataContents as Root;

const ROOT: &str = "CMsgMatchMetaDataContents";
const INFO: &str = "CMsgMatchMetaDataContents.MatchInfo";

/// Resolve the root class by RTTI, find its one live instance on the heap and walk it, using
/// nothing from the builder but the layouts (which a live run derives from the client).
fn capture(w: &World) -> deadlock_walker::Result<Vec<u8>> {
    let module = w.mem.module("client.dll")?;
    let image = PeImage::read(&w.mem, &module)?;
    let vtable = VtableResolver::new(&image).vtable(ROOT)?;
    let config = SearchConfig::default().excluding_module(&module);
    let found = find_instances(&w.mem, vtable, &config)?;
    assert_eq!(found.len(), 1, "exactly one live root object");
    Walker::new(&w.mem, &w.schema, &w.layouts).serialize_checked(ROOT, found[0], vtable)
}

fn decode(wire: &[u8]) -> Root {
    Root::decode(wire).expect("walker output is valid protobuf")
}

#[test]
fn a_hand_made_match_survives_the_trip() {
    let original = sample_match();
    let mut w = World::new(ROOT);
    w.build(ROOT, &original.encode_to_vec());
    assert_eq!(decode(&capture(&w).unwrap()), original);
}

#[test]
fn the_wire_bytes_are_canonical() {
    let original = sample_match();
    let mut w = World::new(ROOT);
    w.build(ROOT, &original.encode_to_vec());
    assert_eq!(capture(&w).unwrap(), original.encode_to_vec());
}

#[test]
fn the_statlocker_capture_survives_the_trip() {
    let Some(raw) = statlocker_fixture() else {
        eprintln!("statlocker cache not found; set DEADLOCK_WALKER_FIXTURE to run this test");
        return;
    };
    let original = Root::decode(&raw[..]).unwrap();
    let info = original.match_info.as_ref().unwrap();
    assert_eq!(info.match_id, Some(109_598_353));
    assert_eq!(info.duration_s, Some(788));
    assert_eq!(info.players.len(), 8);
    assert_eq!(info.objectives.len(), 12);

    let mut w = World::new(ROOT);
    w.build(ROOT, &raw);
    let wire = capture(&w).unwrap();
    assert_eq!(decode(&wire), original);

    // Same pipeline, one player's kills changed in memory: must not compare equal.
    let players = "CMsgMatchMetaDataContents.Players";
    let first = find_instances(&w.mem, w.vtables[players], &SearchConfig::default()).unwrap()[0];
    let kills = w.field_addr(players, first, 8);
    let before = w.mem.read_u32(kills).unwrap();
    w.patch(kills, &(before + 1).to_le_bytes());
    assert_ne!(decode(&capture(&w).unwrap()), original);
}

/// The discriminating half: the same pipeline on memory that differs by one scalar must not
/// decode equal, and must differ in exactly that field.
#[test]
fn one_changed_field_in_memory_shows_up_in_the_output() {
    let original = sample_match();
    let mut w = World::new(ROOT);
    w.build(ROOT, &original.encode_to_vec());

    let info_vt = w.vtables[INFO];
    let info_obj = find_instances(&w.mem, info_vt, &SearchConfig::default()).unwrap()[0];
    let duration = w.field_addr(INFO, info_obj, 1);
    w.patch(duration, &789u32.to_le_bytes());

    let got = decode(&capture(&w).unwrap());
    assert_ne!(got, original);
    let mut expected = original.clone();
    expected.match_info.as_mut().unwrap().duration_s = Some(789);
    assert_eq!(got, expected);
}

#[test]
fn a_cleared_has_bit_hides_the_field() {
    let original = sample_match();
    let mut w = World::new(ROOT);
    w.build(ROOT, &original.encode_to_vec());

    let info_obj = find_instances(&w.mem, w.vtables[INFO], &SearchConfig::default()).unwrap()[0];
    let layout = &w.layouts[INFO];
    let bit = layout.fields[&1].has_bit.unwrap();
    let word_addr = info_obj + u64::from(layout.has_bits_offset) + u64::from(bit / 32) * 4;
    let word = w.mem.read_u32(word_addr).unwrap() & !(1 << (bit % 32));
    w.patch(word_addr, &word.to_le_bytes());

    let got = decode(&capture(&w).unwrap());
    assert_eq!(got.match_info.unwrap().duration_s, None);
}

#[test]
fn an_object_freed_mid_walk_is_reported() {
    let mut w = World::new(ROOT);
    let root = w.build(ROOT, &sample_match().encode_to_vec());
    let vtable = w.vtables[ROOT];
    // The allocator overwrites the first qword of a freed block with its own bookkeeping.
    w.patch(root, &0x0000_0000_dead_beefu64.to_le_bytes());
    let walker = Walker::new(&w.mem, &w.schema, &w.layouts);
    assert!(matches!(
        walker.serialize_checked(ROOT, root, vtable),
        Err(Error::Changed)
    ));
}

#[test]
fn a_dangling_sub_object_pointer_is_a_read_error() {
    let mut w = World::new(ROOT);
    let root = w.build(ROOT, &sample_match().encode_to_vec());
    let ptr_at = w.field_addr(ROOT, root, 2);
    w.patch(ptr_at, &0x0000_5555_0000_0000u64.to_le_bytes());
    let walker = Walker::new(&w.mem, &w.schema, &w.layouts);
    assert!(matches!(
        walker.serialize(ROOT, root),
        Err(Error::Memory(_))
    ));
}

#[test]
fn a_garbage_repeated_count_hits_the_limit_instead_of_allocating() {
    let mut w = World::new(ROOT);
    w.build(ROOT, &sample_match().encode_to_vec());
    let info_obj = find_instances(&w.mem, w.vtables[INFO], &SearchConfig::default()).unwrap()[0];
    // `players` is field 4: RepeatedPtrField, size int at +8.
    let size_at = w.field_addr(INFO, info_obj, 4) + 8;
    w.patch(size_at, &0x7fff_ffffu32.to_le_bytes());
    let walker = Walker::new(&w.mem, &w.schema, &w.layouts);
    let root = find_instances(&w.mem, w.vtables[ROOT], &SearchConfig::default()).unwrap()[0];
    assert!(matches!(walker.serialize(ROOT, root), Err(Error::Limit(_))));
}

#[test]
fn nesting_beyond_the_depth_limit_is_refused() {
    let mut w = World::new(ROOT);
    let root = w.build(ROOT, &sample_match().encode_to_vec());
    let shallow = Walker::new(&w.mem, &w.schema, &w.layouts).with_limits(Limits {
        max_depth: 1,
        ..Limits::default()
    });
    assert!(matches!(
        shallow.serialize(ROOT, root),
        Err(Error::Limit(_))
    ));
}

#[test]
fn a_message_with_no_layout_is_an_error() {
    let mut w = World::new(ROOT);
    let root = w.build(ROOT, &sample_match().encode_to_vec());
    w.layouts.remove(INFO);
    let walker = Walker::new(&w.mem, &w.schema, &w.layouts);
    assert!(matches!(
        walker.serialize(ROOT, root),
        Err(Error::NoLayout(_))
    ));
}
