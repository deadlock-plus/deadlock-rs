//! The session API against a fake client: party, account and build objects on a fake heap,
//! found by RTTI and read back through the layouts the fake's compiled tables describe.

mod support;

use std::time::Duration;

use deadlock_walker::ext::PartyExt;
use deadlock_walker::{Error, GcSession, Kind};
use prost::Message;
use support::*;
use valveprotos::deadlock::c_msg_match_meta_data_contents::{MatchInfo, Players};
use valveprotos::deadlock::c_msg_post_game_progress_data::PlayerData;
use valveprotos::deadlock::cso_citadel_hideout_lobby::Member as HideoutMember;
use valveprotos::deadlock::cso_citadel_party::{Invite, LeftMember, Member};
use valveprotos::deadlock::{
    CMsgAccountHeroStats, CMsgAccountStats, CMsgHeroBuild, CMsgMatchMetaDataContents,
    CMsgPostGameProgressData, CsoAccountHeroInfo, CsoCitadelHideoutLobby, CsoCitadelLobby,
    CsoCitadelParty, CsoGameAccountClient,
};

const ME: u32 = 387_246_372;
const OTHER: u32 = 1_234;

const PARTY: &str = "CSOCitadelParty";
const LOBBY: &str = "CSOCitadelLobby";
const ACCOUNT: &str = "CSOGameAccountClient";
const STATS: &str = "CMsgAccountStats";
const HEROES: &str = "CSOAccountHeroInfo";
const HIDEOUT: &str = "CSOCitadelHideoutLobby";
const BUILD: &str = "CMsgHeroBuild";
const POST_GAME: &str = "CMsgPostGameProgressData";
const META: &str = "CMsgMatchMetaDataContents";

fn world_of(roots: &[&str]) -> World {
    let mut w = World::with_roots(roots);
    w.emit_real_tables();
    w
}

fn world() -> World {
    world_of(&[
        PARTY, LOBBY, ACCOUNT, STATS, HEROES, HIDEOUT, BUILD, POST_GAME, META,
    ])
}

fn member(id: u32, name: &str) -> Member {
    Member {
        account_id: Some(id),
        persona_name: Some(name.into()),
        ..Default::default()
    }
}

fn party(id: u64, members: Vec<Member>) -> CsoCitadelParty {
    CsoCitadelParty {
        party_id: Some(id),
        members,
        join_code: Some(5_444_319),
        ..Default::default()
    }
}

fn session(w: &World) -> GcSession {
    GcSession::new(&w.mem, ME)
        .unwrap()
        .every(Duration::from_secs(3600))
        .refind_every(Duration::ZERO)
}

fn kill(w: &mut World, obj: u64) {
    w.patch(obj, &0u64.to_le_bytes());
}

fn revive(w: &mut World, class: &str, obj: u64) {
    let vt = w.vtables[class];
    w.patch(obj, &vt.to_le_bytes());
}

#[test]
fn a_party_listing_the_local_account_is_found_and_decoded() {
    let mut w = world();
    let mine = party(7, vec![member(ME, "me"), member(OTHER, "bob")]);
    w.build(PARTY, &mine.encode_to_vec());
    let found = session(&w).party(&w.mem).unwrap().unwrap();
    assert_eq!(found, mine);
    assert_eq!(found.display_code().as_deref(), Some("J1ZKN"));
}

#[test]
fn the_party_is_chosen_by_the_account_anchor() {
    let mut w = world();
    let elsewhere = party(6, vec![member(OTHER, "bob"), member(OTHER + 1, "carol")]);
    let mine = party(7, vec![member(OTHER, "bob"), member(ME, "me")]);
    w.build(PARTY, &elsewhere.encode_to_vec());
    w.build(PARTY, &mine.encode_to_vec());
    let mut s = session(&w);
    assert_eq!(s.party(&w.mem).unwrap().unwrap().party_id, Some(7));
    assert_eq!(s.parties(&w.mem).unwrap().len(), 1);
}

#[test]
fn a_party_that_does_not_list_the_local_account_is_not_ours() {
    let mut w = world();
    w.build(PARTY, &party(6, vec![member(OTHER, "bob")]).encode_to_vec());
    assert_eq!(session(&w).party(&w.mem).unwrap(), None);
}

#[test]
fn an_absent_party_is_none_not_an_error() {
    let w = world();
    assert_eq!(session(&w).party(&w.mem).unwrap(), None);
}

#[test]
fn a_session_needs_the_clients_descriptor_tables() {
    let w = World::new(PARTY);
    let err = GcSession::new(&w.mem, ME).unwrap_err();
    assert!(matches!(err, Error::NoTables), "{err}");
}

#[test]
fn a_class_the_client_lacks_is_an_error_for_that_kind_only() {
    let mut w = world_of(&[PARTY]);
    w.build(PARTY, &party(7, vec![member(ME, "me")]).encode_to_vec());
    let mut s = session(&w);
    assert!(matches!(
        s.lobby(&w.mem).unwrap_err(),
        Error::ClassNotFound(_)
    ));
    assert!(s.party(&w.mem).unwrap().is_some());
}

#[test]
fn a_second_read_reuses_the_pin_instead_of_sweeping() {
    let mut w = world();
    w.build(PARTY, &party(7, vec![member(ME, "me")]).encode_to_vec());
    let mut s = session(&w);
    s.party(&w.mem).unwrap();
    assert_eq!(s.stats(), (1, 0));
    s.party(&w.mem).unwrap();
    assert_eq!(s.stats(), (1, 1));
    assert_eq!(s.pins(Kind::Party).len(), 1);
}

#[test]
fn the_sweep_interval_gates_repeat_sweeps() {
    let w = world();
    let mut s = session(&w);
    s.party(&w.mem).unwrap();
    s.party(&w.mem).unwrap();
    assert_eq!(
        s.stats().0,
        1,
        "an absent object must not be swept for twice"
    );
}

#[test]
fn invalidate_forces_another_sweep() {
    let mut w = world();
    w.build(PARTY, &party(7, vec![member(ME, "me")]).encode_to_vec());
    let mut s = session(&w);
    s.party(&w.mem).unwrap();
    s.invalidate();
    assert!(s.pins(Kind::Party).is_empty());
    assert!(s.hints(Kind::Party).is_empty());
    assert!(s.party(&w.mem).unwrap().is_some());
    assert_eq!(s.stats().0, 2);
}

#[test]
fn a_dead_pin_is_dropped_rather_than_returning_stale_data() {
    let mut w = world();
    let obj = w.build(PARTY, &party(7, vec![member(ME, "me")]).encode_to_vec());
    let mut s = session(&w);
    assert!(s.party(&w.mem).unwrap().is_some());

    kill(&mut w, obj);
    assert_eq!(s.party(&w.mem).unwrap(), None);
    assert!(s.pins(Kind::Party).is_empty());
    assert_eq!(
        s.hints(Kind::Party),
        [obj],
        "the address stays worth probing"
    );
}

#[test]
fn a_replacement_at_the_same_address_is_found_without_sweeping() {
    let mut w = world();
    let obj = w.build(PARTY, &party(7, vec![member(ME, "me")]).encode_to_vec());
    let mut s = session(&w);
    s.party(&w.mem).unwrap();

    kill(&mut w, obj);
    assert_eq!(s.party(&w.mem).unwrap(), None);
    revive(&mut w, PARTY, obj);
    assert!(s.party(&w.mem).unwrap().is_some());
    assert_eq!(s.stats().0, 1);
}

#[test]
fn a_replacement_that_moved_within_the_region_is_found_without_sweeping() {
    let mut w = world();
    let old = w.build(PARTY, &party(7, vec![member(ME, "me")]).encode_to_vec());
    let mut s = session(&w);
    s.party(&w.mem).unwrap();

    kill(&mut w, old);
    w.build(
        PARTY,
        &party(7, vec![member(ME, "me"), member(OTHER, "bob")]).encode_to_vec(),
    );
    let found = s.party(&w.mem).unwrap().unwrap();
    assert_eq!(found.members.len(), 2);
    assert_eq!(s.stats().0, 1, "the region re-find, not a whole-heap sweep");
}

#[test]
fn every_place_the_object_has_lived_stays_probeable() {
    let mut w = world();
    let a = w.build(PARTY, &party(7, vec![member(ME, "me")]).encode_to_vec());
    let mut s = session(&w);
    s.party(&w.mem).unwrap();
    kill(&mut w, a);
    let b = w.build(PARTY, &party(8, vec![member(ME, "me")]).encode_to_vec());
    s.party(&w.mem).unwrap();
    assert_eq!(s.hints(Kind::Party), [b, a]);
}

#[test]
fn generations_of_one_party_are_ranked_newest_first() {
    let old = CsoCitadelParty {
        invites: vec![Invite {
            account_id: Some(OTHER),
            ..Default::default()
        }],
        ..party(7, vec![member(ME, "me")])
    };
    let new = party(7, vec![member(ME, "me"), member(OTHER, "bob")]);
    for newest_first in [true, false] {
        let mut w = world();
        let pair = if newest_first {
            [&new, &old]
        } else {
            [&old, &new]
        };
        for p in pair {
            w.build(PARTY, &p.encode_to_vec());
        }
        let mut s = session(&w);
        let all = s.parties(&w.mem).unwrap();
        assert_eq!(
            all,
            [new.clone(), old.clone()],
            "newest_first={newest_first}"
        );
        assert_eq!(s.party(&w.mem).unwrap().unwrap(), new);
    }
}

#[test]
fn a_departure_ranks_the_party_that_records_it_first() {
    let old = party(7, vec![member(ME, "me"), member(OTHER, "bob")]);
    let new = CsoCitadelParty {
        left_members: vec![LeftMember {
            account_id: Some(OTHER),
            ..Default::default()
        }],
        ..party(7, vec![member(ME, "me")])
    };
    let mut w = world();
    w.build(PARTY, &new.encode_to_vec());
    w.build(PARTY, &old.encode_to_vec());
    let all = session(&w).parties(&w.mem).unwrap();
    assert_eq!(all, [new, old]);
}

#[test]
fn an_object_whose_walk_fails_is_skipped_not_fatal() {
    let mut w = world();
    let bad = w.build(
        PARTY,
        &party(6, vec![member(ME, "me"), member(OTHER, "bob")]).encode_to_vec(),
    );
    let members_field = w.field_addr(PARTY, bad, 2);
    w.patch(members_field + 16, &0x1000u64.to_le_bytes());
    let good = party(7, vec![member(ME, "me")]);
    w.build(PARTY, &good.encode_to_vec());

    let mut s = session(&w);
    assert_eq!(s.parties(&w.mem).unwrap(), [good]);
    assert_eq!(s.pins(Kind::Party).len(), 1);
}

#[test]
fn static_default_instances_are_not_objects() {
    let mut w = world();
    w.add_default_instance(PARTY);
    assert_eq!(session(&w).party(&w.mem).unwrap(), None);
}

#[test]
fn a_cached_copy_of_an_object_is_still_an_object() {
    let mut w = world();
    let p = party(7, vec![member(ME, "me")]);
    w.build(PARTY, &p.encode_to_vec());
    w.build(PARTY, &p.encode_to_vec());
    assert_eq!(session(&w).parties(&w.mem).unwrap().len(), 2);
}

#[test]
fn a_sweep_pins_every_kind_and_reports_it() {
    let mut w = world();
    w.build(PARTY, &party(7, vec![member(ME, "me")]).encode_to_vec());
    w.build(PARTY, &party(6, vec![member(OTHER, "bob")]).encode_to_vec());
    w.build(
        ACCOUNT,
        &CsoGameAccountClient {
            account_id: Some(ME),
            wins: Some(3),
            ..Default::default()
        }
        .encode_to_vec(),
    );
    let mut s = session(&w);
    let report = s.sweep(&w.mem).unwrap();
    assert!(report.regions >= 2);
    assert_eq!(report.hits, 3);
    assert_eq!(report.pinned, 2, "the other account's party is not pinned");

    s.party(&w.mem).unwrap();
    s.game_account(&w.mem).unwrap();
    assert_eq!(s.stats(), (1, 2));
}

#[test]
fn the_account_object_is_the_local_accounts() {
    let mut w = world();
    let account = |id, wins| {
        CsoGameAccountClient {
            account_id: Some(id),
            wins: Some(wins),
            priority_tokens: Some(2),
            ..Default::default()
        }
        .encode_to_vec()
    };
    w.build(ACCOUNT, &account(OTHER, 1));
    assert_eq!(session(&w).game_account(&w.mem).unwrap(), None);
    w.build(ACCOUNT, &account(ME, 9));
    let found = session(&w).game_account(&w.mem).unwrap().unwrap();
    assert_eq!((found.account_id, found.wins), (Some(ME), Some(9)));
}

#[test]
fn every_hero_info_object_of_the_account_is_read() {
    let mut w = world();
    for (account, hero) in [(ME, 1), (ME, 2), (OTHER, 3), (ME, 4)] {
        w.build(
            HEROES,
            &CsoAccountHeroInfo {
                account_id: Some(account),
                hero_id: Some(hero),
                wins: Some(5),
                ..Default::default()
            }
            .encode_to_vec(),
        );
    }
    let heroes = session(&w).account_heroes(&w.mem).unwrap();
    let ids: Vec<_> = heroes.iter().filter_map(|h| h.hero_id).collect();
    assert_eq!(ids, [1, 2, 4], "by address, and not another account's");
}

#[test]
fn every_build_the_account_authored_is_read() {
    let mut w = world();
    for (author, id, name) in [(ME, 1, "one"), (OTHER, 2, "theirs"), (ME, 3, "three")] {
        w.build(
            BUILD,
            &CMsgHeroBuild {
                hero_build_id: Some(id),
                hero_id: Some(id),
                author_account_id: Some(author),
                name: Some(name.into()),
                ..Default::default()
            }
            .encode_to_vec(),
        );
    }
    let builds = session(&w).hero_builds(&w.mem).unwrap();
    let names: Vec<_> = builds.iter().filter_map(|b| b.name.as_deref()).collect();
    assert_eq!(names, ["one", "three"]);
}

#[test]
fn the_statistics_of_the_local_account_are_read() {
    let mut w = world();
    let stats = |account| CMsgAccountStats {
        account_id: Some(account),
        stats: vec![
            CMsgAccountHeroStats {
                hero_id: Some(0),
                stat_id: vec![1, 2],
                total_value: vec![10, 20],
                ..Default::default()
            },
            CMsgAccountHeroStats {
                hero_id: Some(15),
                ..Default::default()
            },
        ],
    };
    w.build(STATS, &stats(OTHER).encode_to_vec());
    let mut s = session(&w);
    assert_eq!(s.account_stats(&w.mem).unwrap(), None);
    w.build(STATS, &stats(ME).encode_to_vec());
    let mut s = session(&w);
    assert_eq!(s.account_stats(&w.mem).unwrap().unwrap(), stats(ME));
}

#[test]
fn a_hideout_is_ours_when_it_lists_the_local_account() {
    let mut w = world();
    let hideout = |ids: &[u32]| CsoCitadelHideoutLobby {
        hideout_lobby_id: Some(11),
        party_id: Some(7),
        members: ids
            .iter()
            .map(|&id| HideoutMember {
                account_id: Some(id),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    w.build(HIDEOUT, &hideout(&[OTHER]).encode_to_vec());
    assert_eq!(session(&w).hideout(&w.mem).unwrap(), None);
    w.build(HIDEOUT, &hideout(&[OTHER, ME]).encode_to_vec());
    assert_eq!(
        session(&w).hideout(&w.mem).unwrap().unwrap(),
        hideout(&[OTHER, ME])
    );
}

#[test]
fn the_lobby_is_read_when_resident() {
    let mut w = world();
    let lobby = CsoCitadelLobby {
        lobby_id: Some(99),
        match_id: Some(1_000),
        ..Default::default()
    };
    assert_eq!(session(&w).lobby(&w.mem).unwrap(), None);
    w.build(LOBBY, &lobby.encode_to_vec());
    assert_eq!(session(&w).lobby(&w.mem).unwrap().unwrap(), lobby);
}

#[test]
fn post_game_progress_is_ours_when_the_local_account_played() {
    let mut w = world();
    let progress = |id| CMsgPostGameProgressData {
        match_id: Some(5),
        local_player: Some(PlayerData {
            account_id: Some(id),
            ..Default::default()
        }),
        ..Default::default()
    };
    w.build(POST_GAME, &progress(OTHER).encode_to_vec());
    assert_eq!(session(&w).post_game_progress(&w.mem).unwrap(), None);
    w.build(POST_GAME, &progress(ME).encode_to_vec());
    assert_eq!(
        session(&w).post_game_progress(&w.mem).unwrap().unwrap(),
        progress(ME)
    );
}

#[test]
fn a_replacement_is_found_while_other_objects_of_the_kind_still_live() {
    let mut w = world();
    let gone = w.build(PARTY, &party(6, vec![member(ME, "me")]).encode_to_vec());
    w.build(PARTY, &party(7, vec![member(ME, "me")]).encode_to_vec());
    let mut s = session(&w);
    assert_eq!(s.parties(&w.mem).unwrap().len(), 2);

    kill(&mut w, gone);
    w.build(PARTY, &party(8, vec![member(ME, "me")]).encode_to_vec());
    let mut ids: Vec<_> = s
        .parties(&w.mem)
        .unwrap()
        .iter()
        .filter_map(|p| p.party_id)
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, [7, 8]);
    assert_eq!(s.stats().0, 1, "found by the region re-find");
}

#[test]
fn a_session_is_bound_to_the_process_it_was_built_on() {
    let mut w = world();
    w.build(PARTY, &party(7, vec![member(ME, "me")]).encode_to_vec());
    let mut s = session(&w);
    assert!(s.party(&w.mem).unwrap().is_some());

    let restarted = deadlock_memory::mock::MockMemory::new(9);
    assert!(matches!(
        s.party(&restarted).unwrap_err(),
        Error::WrongProcess
    ));
    assert!(matches!(
        s.sweep(&restarted).unwrap_err(),
        Error::WrongProcess
    ));
    assert!(
        s.party(&w.mem).unwrap().is_some(),
        "the right process still works"
    );
}

fn meta(match_id: u64, players: u32) -> CMsgMatchMetaDataContents {
    CMsgMatchMetaDataContents {
        match_info: Some(MatchInfo {
            match_id: Some(match_id),
            duration_s: Some(600),
            players: (0..players)
                .map(|slot| Players {
                    account_id: Some(5_000 + slot),
                    player_slot: Some(slot),
                    kills: Some(slot),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }),
    }
}

fn match_ids(found: &[CMsgMatchMetaDataContents]) -> Vec<u64> {
    let mut ids: Vec<_> = found
        .iter()
        .filter_map(|m| m.match_info.as_ref()?.match_id)
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn match_metadata_is_selected_by_match_id() {
    let mut w = world();
    for (id, players) in [(100, 2), (200, 3), (300, 4)] {
        w.build(META, &meta(id, players).encode_to_vec());
    }
    let mut s = session(&w);
    for (id, players) in [(300, 4), (100, 2), (200, 3)] {
        assert_eq!(
            s.match_metadata(&w.mem, id).unwrap(),
            Some(meta(id, players))
        );
    }
    assert_eq!(s.match_metadata(&w.mem, 400).unwrap(), None);
}

#[test]
fn match_metadata_all_lists_every_resident_match() {
    let mut w = world();
    for id in [300, 100, 200] {
        w.build(META, &meta(id, 2).encode_to_vec());
    }
    let found = session(&w).match_metadata_all(&w.mem).unwrap();
    assert_eq!(match_ids(&found), [100, 200, 300]);
}

#[test]
fn match_metadata_does_not_depend_on_the_local_account() {
    let mut w = world();
    w.build(META, &meta(100, 2).encode_to_vec());
    let mut stranger = GcSession::new(&w.mem, OTHER).unwrap();
    assert!(stranger.match_metadata(&w.mem, 100).unwrap().is_some());
}

#[test]
fn a_half_built_match_is_not_complete() {
    let mut w = world();
    w.build(META, &CMsgMatchMetaDataContents::default().encode_to_vec());
    w.build(META, &meta(0, 3).encode_to_vec());
    w.build(META, &meta(100, 0).encode_to_vec());
    let mut s = session(&w);
    assert_eq!(s.match_metadata(&w.mem, 100).unwrap(), None);
    assert_eq!(s.match_metadata(&w.mem, 0).unwrap(), None);
    assert!(s.match_metadata_all(&w.mem).unwrap().is_empty());
    assert!(s.pins(Kind::MatchMetaData).is_empty());
}

#[test]
fn a_complete_copy_wins_over_a_half_built_one_of_the_same_match() {
    let mut w = world();
    w.build(META, &meta(100, 0).encode_to_vec());
    w.build(META, &meta(100, 3).encode_to_vec());
    let found = session(&w).match_metadata(&w.mem, 100).unwrap().unwrap();
    assert_eq!(found, meta(100, 3));
}

#[test]
fn the_fuller_copy_of_a_match_is_chosen() {
    let mut w = world();
    w.build(META, &meta(100, 2).encode_to_vec());
    w.build(META, &meta(100, 5).encode_to_vec());
    w.build(META, &meta(100, 4).encode_to_vec());
    let found = session(&w).match_metadata(&w.mem, 100).unwrap().unwrap();
    assert_eq!(found, meta(100, 5));
}

#[test]
fn a_match_freed_mid_walk_is_none_not_an_error() {
    let mut w = world();
    let a = w.build(META, &meta(100, 2).encode_to_vec());
    w.build(META, &meta(200, 2).encode_to_vec());
    let mut s = session(&w);
    assert!(s.match_metadata(&w.mem, 100).unwrap().is_some());
    assert_eq!(s.pins(Kind::MatchMetaData).len(), 2);

    kill(&mut w, a);
    assert_eq!(s.match_metadata(&w.mem, 100).unwrap(), None);
    assert!(s.match_metadata(&w.mem, 200).unwrap().is_some());
    assert_eq!(s.pins(Kind::MatchMetaData).len(), 1);

    revive(&mut w, META, a);
    assert!(s.match_metadata(&w.mem, 100).unwrap().is_some());
}

#[test]
fn a_match_that_appears_after_the_pins_were_taken_is_found() {
    let mut w = world();
    w.build(META, &meta(100, 2).encode_to_vec());
    let mut s = session(&w);
    assert!(s.match_metadata(&w.mem, 100).unwrap().is_some());
    assert_eq!(s.match_metadata(&w.mem, 200).unwrap(), None);

    w.build(META, &meta(200, 3).encode_to_vec());
    assert_eq!(s.match_metadata(&w.mem, 200).unwrap(), Some(meta(200, 3)));
    assert_eq!(s.stats().0, 1, "found by the region re-find");
}

#[test]
fn a_known_match_is_served_from_its_pin() {
    let mut w = world();
    w.build(META, &meta(100, 2).encode_to_vec());
    w.build(META, &meta(200, 2).encode_to_vec());
    let mut s = session(&w);
    s.match_metadata(&w.mem, 100).unwrap();
    s.match_metadata(&w.mem, 200).unwrap();
    assert_eq!(s.stats(), (1, 1));
}

#[test]
fn a_missing_match_does_not_sweep_again_within_the_interval() {
    let w = world();
    let mut s = session(&w);
    s.match_metadata(&w.mem, 1).unwrap();
    s.match_metadata(&w.mem, 1).unwrap();
    assert_eq!(s.stats().0, 1);
}

#[test]
fn the_metadata_kind_names_its_message() {
    assert_eq!(Kind::MatchMetaData.message(), META);
    assert!(Kind::ALL.contains(&Kind::MatchMetaData));
}
