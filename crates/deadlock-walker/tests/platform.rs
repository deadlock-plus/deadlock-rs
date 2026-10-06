//! What differs between a Windows client and the same PE image under Wine or Proton: how
//! the module is named and sized, and where the heap sits in the address space.

mod support;

use deadlock_memory::mem::MemoryReader;
use deadlock_memory::mock::MockMemory;
use deadlock_walker::{GcSession, SearchConfig, find_instances};
use support::*;

const ROOT: &str = "CMsgMatchMetaDataContents";
const ME: u32 = 1;

fn client_world() -> World {
    let mut w = World::with_roots(&["CSOCitadelParty"]);
    w.emit_real_tables();
    w
}

/// The world's memory re-registered the way `/proc/<pid>/maps` reports it.
fn as_wine_sees_it(w: &World, name: &str, size: usize) -> MockMemory {
    let mut m = MockMemory::new(4242);
    m.write(
        MODULE_BASE,
        &w.mem.read_bytes(MODULE_BASE, MODULE_SIZE).unwrap(),
    );
    m.add_module(name, MODULE_BASE, size);
    m.write(HEAP_BASE, &w.mem.read_bytes(HEAP_BASE, 4096).unwrap());
    m.add_region(HEAP_BASE, 4096);
    m
}

#[test]
fn the_client_is_found_under_whatever_case_the_platform_reports() {
    let w = client_world();
    let m = as_wine_sees_it(&w, "Client.DLL", MODULE_SIZE);
    assert!(GcSession::new(&m, ME).is_ok());
}

#[test]
fn a_module_reported_shorter_than_its_image_is_an_error_not_a_panic() {
    let w = client_world();
    assert!(GcSession::new(&as_wine_sees_it(&w, "client.dll", MODULE_SIZE), ME).is_ok());
    for size in [0x10, 0x1000, MODULE_SIZE / 2] {
        let m = as_wine_sees_it(&w, "client.dll", size);
        assert!(GcSession::new(&m, ME).is_err(), "size {size:#x}");
    }
}

#[test]
fn an_object_in_the_last_page_of_the_address_space_is_found() {
    let w = World::new(ROOT);
    let vtable = w.vtables[ROOT];
    let top = 0x7fff_ffff_f000u64;
    let mut m = MockMemory::new(1);
    let mut page = vec![0u8; 4096];
    page[4088..].copy_from_slice(&vtable.to_le_bytes());
    m.write(top, &page);
    m.add_region(top, 4096);
    let hits = find_instances(&m, vtable, &SearchConfig::default()).unwrap();
    assert_eq!(hits, [top + 4088]);
}

#[test]
fn a_region_that_does_not_start_on_a_qword_is_searched_from_the_next_one() {
    let w = World::new(ROOT);
    let vtable = w.vtables[ROOT];
    let base = 0x7f00_0000_1004u64;
    let mut m = MockMemory::new(1);
    let mut bytes = vec![0u8; 64];
    bytes[4..12].copy_from_slice(&vtable.to_le_bytes());
    bytes[14..22].copy_from_slice(&vtable.to_le_bytes());
    m.write(base, &bytes);
    m.add_region(base, bytes.len());
    let hits = find_instances(&m, vtable, &SearchConfig::default()).unwrap();
    assert_eq!(
        hits,
        [base + 4],
        "the unaligned copy at +14 is not an object"
    );
}
