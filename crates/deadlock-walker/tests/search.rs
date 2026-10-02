//! Heap search tests: find objects by vtable in a fake address space.

mod support;

use deadlock_memory::mem::{MemoryReader, Region};
use deadlock_walker::{SearchConfig, find_in, find_instances, find_many};
use prost::Message;
use support::*;

const ROOT: &str = "CMsgMatchMetaDataContents";
const PLAYERS: &str = "CMsgMatchMetaDataContents.Players";
const OBJECTIVE: &str = "CMsgMatchMetaDataContents.Objective";

fn world() -> (World, u64) {
    let mut w = World::new(ROOT);
    let root = w.build(ROOT, &sample_match().encode_to_vec());
    (w, root)
}

#[test]
fn finds_every_instance_of_a_class_and_nothing_else() {
    let (w, root) = world();
    let players = find_instances(&w.mem, w.vtables[PLAYERS], &SearchConfig::default()).unwrap();
    assert_eq!(players.len(), 4);
    let objectives =
        find_instances(&w.mem, w.vtables[OBJECTIVE], &SearchConfig::default()).unwrap();
    assert_eq!(objectives.len(), 2);
    let roots = find_instances(&w.mem, w.vtables[ROOT], &SearchConfig::default()).unwrap();
    assert_eq!(roots, [root]);
    assert!(
        players.windows(2).all(|p| p[0] < p[1]),
        "hits are in address order"
    );
}

#[test]
fn a_vtable_nobody_uses_finds_nothing() {
    let (w, _) = world();
    let hits = find_instances(&w.mem, 0x7ffa_dead_0000, &SearchConfig::default()).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn static_default_instances_in_the_module_are_excluded_on_request() {
    let (mut w, _) = world();
    let default_instance = w.add_default_instance(PLAYERS);
    let vt = w.vtables[PLAYERS];

    let all = find_instances(&w.mem, vt, &SearchConfig::default()).unwrap();
    assert!(all.contains(&default_instance));

    let module = w.mem.module("client.dll").unwrap();
    let config = SearchConfig::default().excluding_module(&module);
    let heap_only = find_instances(&w.mem, vt, &config).unwrap();
    assert_eq!(heap_only.len(), 4);
    assert!(!heap_only.contains(&default_instance));
}

#[test]
fn small_chunks_lose_nothing() {
    let (w, _) = world();
    let vt = w.vtables[PLAYERS];
    let whole = find_instances(&w.mem, vt, &SearchConfig::default()).unwrap();
    for chunk in [8, 24, 64, 4096] {
        let config = SearchConfig {
            chunk,
            ..SearchConfig::default()
        };
        assert_eq!(
            find_instances(&w.mem, vt, &config).unwrap(),
            whole,
            "chunk {chunk}"
        );
    }
}

#[test]
fn a_region_that_runs_past_the_mapped_pages_is_not_fatal() {
    let (mut w, _) = world();
    let used = w.heap.bytes.len();
    // Declared far larger than what is mapped, as when pages are decommitted after listing.
    w.mem
        .add_region(HEAP_BASE + used as u64 + 0x10_0000, 0x4000_0000);
    let hits = find_instances(&w.mem, w.vtables[PLAYERS], &SearchConfig::default()).unwrap();
    assert_eq!(hits.len(), 4);
}

#[test]
fn the_limit_caps_the_result() {
    let (w, _) = world();
    let config = SearchConfig {
        limit: 2,
        ..SearchConfig::default()
    };
    assert_eq!(
        find_instances(&w.mem, w.vtables[PLAYERS], &config)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn one_pass_finds_each_class_like_separate_searches() {
    let (w, root) = world();
    let vts = [w.vtables[PLAYERS], w.vtables[OBJECTIVE], w.vtables[ROOT]];
    let sweep = find_many(&w.mem, &vts, &SearchConfig::default()).unwrap();
    assert!(sweep.regions >= 2, "{} regions", sweep.regions);
    assert_eq!(sweep.hits.len(), 3);
    for (vt, hits) in vts.iter().zip(&sweep.hits) {
        assert_eq!(
            hits,
            &find_instances(&w.mem, *vt, &SearchConfig::default()).unwrap()
        );
    }
    assert_eq!(sweep.hits[0].len(), 4);
    assert_eq!(sweep.hits[2], [root]);
}

#[test]
fn a_restricted_search_sees_only_the_given_regions() {
    let (w, _) = world();
    let vt = w.vtables[PLAYERS];
    let heap = Region {
        base: HEAP_BASE,
        size: w.heap.bytes.len(),
    };
    let inside = find_in(&w.mem, &[heap], &[vt], &SearchConfig::default());
    assert_eq!(inside[0].len(), 4);
    let elsewhere = Region {
        base: HEAP_BASE + 0x4000_0000,
        size: 0x1000,
    };
    let outside = find_in(&w.mem, &[elsewhere], &[vt], &SearchConfig::default());
    assert!(outside[0].is_empty());
}

#[test]
fn the_exclusion_and_limit_apply_per_class_in_a_one_pass_search() {
    let (mut w, _) = world();
    let default_instance = w.add_default_instance(PLAYERS);
    let module = w.mem.module("client.dll").unwrap();
    let config = SearchConfig {
        limit: 2,
        ..SearchConfig::default().excluding_module(&module)
    };
    let vts = [w.vtables[PLAYERS], w.vtables[OBJECTIVE]];
    let sweep = find_many(&w.mem, &vts, &config).unwrap();
    assert_eq!(sweep.hits[0].len(), 2);
    assert_eq!(sweep.hits[1].len(), 2);
    assert!(!sweep.hits[0].contains(&default_instance));
}
