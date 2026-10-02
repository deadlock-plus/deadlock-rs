//! Resolver tests: RTTI name to vtable over a fake PE image.

mod support;

use deadlock_memory::mem::MemoryReader;
use deadlock_walker::{Error, PeImage, VtableResolver, mangled_class_name};
use support::*;

const ROOT: &str = "CMsgMatchMetaDataContents";
const INFO: &str = "CMsgMatchMetaDataContents.MatchInfo";
const PLAYERS: &str = "CMsgMatchMetaDataContents.Players";

fn image(w: &World) -> PeImage {
    let module = w.mem.module("client.dll").unwrap();
    PeImage::read(&w.mem, &module).unwrap()
}

#[test]
fn mangled_names_match_the_strings_found_in_client_dll() {
    assert_eq!(
        mangled_class_name(PLAYERS),
        ".?AVCMsgMatchMetaDataContents_Players@@"
    );
    assert_eq!(
        mangled_class_name("CMsgClientWelcome"),
        ".?AVCMsgClientWelcome@@"
    );
    assert_eq!(
        mangled_class_name("pkg.sub.Outer.Inner"),
        ".?AVOuter_Inner@sub@pkg@@"
    );
}

#[test]
fn the_pe_reader_finds_sections_and_code() {
    let w = World::new(ROOT);
    let img = image(&w);
    let names: Vec<_> = img.sections().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, [".text", ".rdata", ".data"]);
    assert!(img.is_code_address(MODULE_BASE + 0x1010));
    assert!(!img.is_code_address(MODULE_BASE + 0x2010));
    assert!(!img.is_code_address(0x10));
}

#[test]
fn each_class_resolves_to_its_primary_vtable() {
    let w = World::new(ROOT);
    let img = image(&w);
    let r = VtableResolver::new(&img);
    for name in [ROOT, INFO, PLAYERS] {
        assert_eq!(r.vtable(name).unwrap(), w.vtables[name], "{name}");
        assert_eq!(
            r.vtables(name),
            [w.vtables[name]],
            "{name}: secondary vtable leaked"
        );
    }
}

#[test]
fn a_class_is_not_confused_with_one_whose_name_extends_it() {
    let w = World::new(ROOT);
    let img = image(&w);
    let r = VtableResolver::new(&img);
    assert_ne!(r.vtable(ROOT).unwrap(), r.vtable(INFO).unwrap());
}

#[test]
fn a_class_with_no_rtti_is_an_error() {
    let w = World::new(ROOT);
    let img = image(&w);
    let r = VtableResolver::new(&img);
    assert!(matches!(
        r.vtable("CMsgNotInThisImage"),
        Err(Error::ClassNotFound(_))
    ));
}

#[test]
fn a_vtable_whose_first_slot_is_not_code_is_rejected() {
    let mut w = World::new(ROOT);
    w.corrupt_first_slot(PLAYERS);
    let img = image(&w);
    let r = VtableResolver::new(&img);
    assert!(r.vtable(PLAYERS).is_err());
    assert!(r.vtable(INFO).is_ok());
}

#[test]
fn a_truncated_image_is_refused() {
    assert!(matches!(
        PeImage::from_bytes(0x1000, vec![0u8; 64]),
        Err(Error::BadImage(_))
    ));
    assert!(matches!(
        PeImage::from_bytes(0x1000, b"MZ\0\0".repeat(64)),
        Err(Error::BadImage(_))
    ));
}
