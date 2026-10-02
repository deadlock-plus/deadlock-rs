//! Per-team aggregates and team names.

use std::collections::HashMap;

use deadlock_core::Team;

use crate::entity::EntitySnapshot;
use crate::reader::Reader;
use crate::snapshot::{BASE_ENTITY, Objective, PlayerRow, StructureState};
use deadlock_memory::mem::MemoryReader;

/// Aggregate stats for one team.
///
/// Souls and AP are summed over the team's players; the game exposes no team-level
/// total. `score` comes from `m_iScore`, which was observed to equal the team's summed
/// kills exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TeamStats {
    /// Which team.
    pub team: Team,
    /// Summed `m_iGoldNetWorth`.
    pub souls: u32,
    /// Summed `m_iAPNetWorth`.
    pub ability_points: u32,
    /// Summed kills.
    pub kills: u32,
    /// Summed deaths.
    pub deaths: u32,
    /// Summed assists.
    pub assists: u32,
    /// Summed last hits.
    pub last_hits: u32,
    /// Summed hero damage.
    pub hero_damage: u32,
    /// Summed objective damage.
    pub objective_damage: u32,
    /// Summed healing done.
    pub healing: u32,
    /// Summed denies.
    pub denies: u32,
    /// Rejuvenators this side has taken (`m_iAmberRejuvCount` / `m_iSapphireRejuvCount`).
    pub rejuvenators: Option<u32>,
    /// Map structures known to be standing for this side.
    ///
    /// A structure counts only when the game says it is up; see
    /// [`TeamStats::structures_known`] for what an unreadable one does to this.
    pub structures_alive: u32,
    /// `m_iScore`, which `C_Team` declares and `C_CitadelTeam` inherits.
    ///
    /// Attributing it to `C_CitadelTeam` invites confusion with that class's own
    /// `m_nStreetBrawlScore` and `m_nStreetBrawlScoreLastRound`, which are separate
    /// fields with a separate meaning. The read passes `"C_CitadelTeam"` and resolves
    /// through the base chain, so the address is the same either way.
    pub score: Option<i32>,
    /// How many players were counted.
    pub players: u32,
    /// Whether every counted player reported every summed statistic.
    ///
    /// `false` means the totals above are understated, not that the side has scored
    /// nothing. The usual cause is `m_PlayerDataGlobal` failing to resolve, which drops
    /// all of a player's statistics at once - without this flag the resulting zeros are
    /// indistinguishable from a side that genuinely has none.
    pub complete: bool,
    /// Whether every one of this side's structures answered.
    ///
    /// `false` makes [`TeamStats::structures_alive`] a floor: at least one structure
    /// reported neither a destruction flag, nor a life state, nor a health, and an
    /// unreadable structure is left out of the count rather than assumed to be standing.
    ///
    /// Separate from [`TeamStats::complete`] deliberately. One flag over both would make
    /// a refused `m_PlayerDataGlobal` indistinguishable from a structure that could not
    /// be read, and the two point at different things having gone wrong.
    pub structures_known: bool,
}

/// Read team numbers and names from the game's own `C_CitadelTeam` entities.
/// The `C_CitadelTeam` entities found in one walk, indexed by team.
///
/// Both halves come out of the same pass, so `summarise_teams` can look a team's entity up
/// by number instead of re-scanning the entity list and re-reading `m_iTeamNum` off every
/// `C_CitadelTeam` once per playing side.
pub(crate) struct Teams {
    /// Entity instance address, for the fields that still need a live read.
    instances: HashMap<Team, u64>,
    /// Display name, or the default when the game did not give one.
    pub names: HashMap<Team, String>,
}

/// Extent of `C_Team::m_szTeamname`, which the schema declares as `char[129]`.
///
/// The schema index records class sizes but not array extents, so the declared length
/// cannot be resolved at runtime and is pinned here instead.
const TEAM_NAME_CAPACITY: usize = 129;

/// Read the inline `C_Team::m_szTeamname`, which is a `char` array rather than a pointer.
///
/// `class_size` is `sizeof(C_Team)` when the schema could report it, and clamps the read
/// to the room the class actually leaves after the field, so a patch that shrinks the
/// class cannot send the read off the end of the entity.
///
/// `None` when the read failed or the game left the name blank, so the caller can fall
/// back to the built-in name for the side.
fn read_team_name(
    mem: &dyn MemoryReader,
    instance: u64,
    name_off: u32,
    class_size: Option<u32>,
) -> Option<String> {
    let cap = class_size.map_or(TEAM_NAME_CAPACITY, |size| {
        (size.saturating_sub(name_off) as usize).min(TEAM_NAME_CAPACITY)
    });
    mem.read_cstr(instance.wrapping_add(u64::from(name_off)), cap)
        .ok()
        .filter(|s| !s.is_empty())
}

pub(crate) fn read_teams(reader: &Reader, entities: &EntitySnapshot) -> Teams {
    let mut teams = Teams {
        instances: HashMap::new(),
        names: HashMap::new(),
    };
    let name_off = reader.offset_of("C_Team", "m_szTeamname");
    let class_size = reader
        .schema()
        .and_then(|s| s.classes.get("C_Team"))
        .map(|c| c.size);
    for t in entities.of_class("C_CitadelTeam") {
        let Some(num) = reader.field_u8(t.instance, BASE_ENTITY, "m_iTeamNum") else {
            continue;
        };
        let team = Team(num as u32);
        let name = name_off
            .and_then(|o| read_team_name(reader.memory(), t.instance, o, class_size))
            .unwrap_or_else(|| team.default_name().to_string());
        teams.instances.insert(team, t.instance);
        teams.names.insert(team, name);
    }
    teams
}

/// One team's totals, everything except `score`, which needs a live read.
///
/// Only rows whose team matches are counted, so spectators and neutrals fall out, and
/// only structures owned by `team` reach its counts.
///
/// Every objective already carries the state its read settled on, so this is a pure
/// summation over values the snapshot holds.
fn tally(
    team: Team,
    players: &[PlayerRow],
    structures: &[Objective],
    rejuvenators: Option<u32>,
) -> TeamStats {
    let mut s = TeamStats {
        team,
        rejuvenators,
        complete: true,
        structures_known: true,
        ..Default::default()
    };
    for p in players.iter().filter(|p| p.team == Some(team)) {
        s.players = s.players.saturating_add(1);
        // Saturating throughout: every operand is a number read out of another process,
        // so a corrupt value must not wrap a total or panic a debug build.
        s.souls = s.souls.saturating_add(p.net_worth.unwrap_or(0));
        s.ability_points = s
            .ability_points
            .saturating_add(p.ability_points.unwrap_or(0));
        s.kills = s.kills.saturating_add(p.kills.unwrap_or(0));
        s.deaths = s.deaths.saturating_add(p.deaths.unwrap_or(0));
        s.assists = s.assists.saturating_add(p.assists.unwrap_or(0));
        s.last_hits = s.last_hits.saturating_add(p.last_hits.unwrap_or(0));
        s.hero_damage = s.hero_damage.saturating_add(p.hero_damage.unwrap_or(0));
        s.objective_damage = s
            .objective_damage
            .saturating_add(p.objective_damage.unwrap_or(0));
        s.healing = s.healing.saturating_add(p.healing.unwrap_or(0));
        s.denies = s.denies.saturating_add(p.denies.unwrap_or(0));

        // One missing statistic makes the whole side's totals understated.
        let reported = [
            p.net_worth,
            p.ability_points,
            p.kills,
            p.deaths,
            p.assists,
            p.last_hits,
            p.hero_damage,
            p.objective_damage,
            p.healing,
            p.denies,
        ];
        if reported.iter().any(Option::is_none) {
            s.complete = false;
        }
    }
    for o in structures.iter().filter(|o| o.team == Some(team)) {
        match o.state {
            StructureState::Standing => {
                s.structures_alive = s.structures_alive.saturating_add(1);
            }
            StructureState::Destroyed => {}
            // Counted as neither standing nor lost, which is what the flag reports.
            StructureState::Unknown => s.structures_known = false,
        }
    }
    s
}

/// Sum player stats per team. The game exposes no team-level souls total.
pub(crate) fn summarise_teams(
    reader: &Reader,
    teams: &Teams,
    players: &[PlayerRow],
    objectives: &[Objective],
    rejuv: (Option<u32>, Option<u32>),
) -> Vec<TeamStats> {
    let mut out = Vec::new();
    for team in Team::playing() {
        let rejuvenators = if team == Team::AMBER {
            rejuv.0
        } else {
            rejuv.1
        };
        let mut s = tally(team, players, objectives, rejuvenators);
        // The address is already known, so this is one read rather than a scan of every
        // entity plus a `m_iTeamNum` read per team entity passed.
        s.score = teams
            .instances
            .get(&team)
            .and_then(|&addr| reader.field_i32(addr, "C_CitadelTeam", "m_iScore"));
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{ObjectiveKind, PlayerRow};
    use deadlock_memory::mock::MockMemory;

    /// Live `C_Team` on build 6683: 0x6a8 bytes total, with `m_szTeamname` at 0x624 and
    /// `m_iScore` immediately before it at 0x620.
    const C_TEAM_SIZE: u32 = 0x6a8;
    const TEAMNAME_OFF: u32 = 0x0624;
    const INSTANCE: u64 = 0x1_0000;

    /// A `C_Team` object carrying `name` in its inline `m_szTeamname`, mapped as one
    /// segment so a read running past the end of the object fails rather than sliding
    /// into whatever is next.
    fn team_object(name: &[u8]) -> MockMemory {
        let mut obj = vec![0u8; C_TEAM_SIZE as usize];
        let at = TEAMNAME_OFF as usize;
        obj[at..at + name.len()].copy_from_slice(name);
        let mut mem = MockMemory::new(1);
        mem.write(INSTANCE, &obj);
        mem
    }

    /// The failure this guards: a Street Brawl lobby names its own teams, and a name past
    /// 64 bytes comes back cut at 64 if the read is capped there rather than at the
    /// `char[129]` the schema declares. 0x6a8 - 0x624 is 132 bytes of room, so the whole
    /// array sits inside the object.
    #[test]
    fn a_custom_team_name_longer_than_sixty_four_bytes_is_read_whole() {
        let name = "The Astonishingly Long Street Brawl Team Name Of Nineteen Ninety Nine";
        assert!(name.len() > 64, "the name has to exceed 64 bytes");
        assert!(name.len() < 129, "and still fit the declared array");

        let mem = team_object(name.as_bytes());
        assert_eq!(
            read_team_name(&mem, INSTANCE, TEAMNAME_OFF, Some(C_TEAM_SIZE)).as_deref(),
            Some(name)
        );
    }

    /// An unterminated name must not read past the array. The full `char[129]` comes back
    /// when the class is its live size, and only what the class leaves room for when the
    /// schema reports a smaller one.
    #[test]
    fn an_unterminated_team_name_is_capped_at_the_room_the_class_leaves() {
        let mem = team_object(&[b'x'; 129]);
        let full = read_team_name(&mem, INSTANCE, TEAMNAME_OFF, Some(C_TEAM_SIZE));
        assert_eq!(full.as_deref().map(str::len), Some(129));

        let shrunk = read_team_name(&mem, INSTANCE, TEAMNAME_OFF, Some(TEAMNAME_OFF + 8));
        assert_eq!(shrunk.as_deref().map(str::len), Some(8));
    }

    /// A blank name is not a name; the caller substitutes the side's built-in one.
    #[test]
    fn a_blank_team_name_reads_as_absent() {
        let mem = team_object(b"");
        assert_eq!(read_team_name(&mem, INSTANCE, TEAMNAME_OFF, None), None);
    }

    /// A player every statistic was read from. Every summed field is `Some`, because a
    /// `None` now means "could not be read" and clears `TeamStats::complete`.
    fn player(team: Team, souls: u32, kills: u32, deaths: u32) -> PlayerRow {
        PlayerRow {
            team: Some(team),
            net_worth: Some(souls),
            kills: Some(kills),
            deaths: Some(deaths),
            ability_points: Some(10),
            assists: Some(0),
            last_hits: Some(0),
            hero_damage: Some(0),
            objective_damage: Some(0),
            healing: Some(0),
            denies: Some(0),
            ..Default::default()
        }
    }

    /// The failure this guards: a refused `m_PlayerDataGlobal` leaves every statistic
    /// `None`, and the totals were `unwrap_or(0)`ed into a scoreboard of zeros that a
    /// consumer had no way to tell from a real one.
    #[test]
    fn a_side_missing_statistics_says_so_instead_of_reporting_zeros() {
        let mut blind = player(Team::AMBER, 0, 0, 0);
        blind.kills = None;
        blind.hero_damage = None;

        let s = tally(
            Team::AMBER,
            &[player(Team::AMBER, 1000, 3, 1), blind],
            &[],
            None,
        );
        assert!(!s.complete, "one unread statistic understates the side");
        assert_eq!(s.players, 2, "the player is still counted");
        assert_eq!(s.kills, 3, "and what was read is still summed");

        let clean = tally(Team::AMBER, &[player(Team::AMBER, 1000, 3, 1)], &[], None);
        assert!(clean.complete, "a fully-read side reports complete");
    }

    /// Totals are built from numbers read out of another process, so a corrupt value has
    /// to saturate rather than wrap a release build or panic a debug one.
    #[test]
    fn totals_saturate_rather_than_overflow() {
        let mut huge = player(Team::AMBER, u32::MAX, u32::MAX, 0);
        huge.hero_damage = Some(u32::MAX);
        let s = tally(Team::AMBER, &[huge.clone(), huge], &[], None);
        assert_eq!(s.souls, u32::MAX);
        assert_eq!(s.kills, u32::MAX);
        assert_eq!(s.hero_damage, u32::MAX);
    }

    #[test]
    fn team_totals_sum_only_their_own_side() {
        let players = [
            player(Team::AMBER, 1000, 3, 1),
            player(Team::AMBER, 2000, 4, 2),
            player(Team::SAPPHIRE, 500, 1, 7),
            player(Team::SPECTATOR, 999_999, 99, 99),
        ];

        let amber = tally(Team::AMBER, &players, &[], Some(2));
        assert_eq!(amber.players, 2);
        assert_eq!(amber.souls, 3000);
        assert_eq!(amber.kills, 7);
        assert_eq!(amber.deaths, 3);
        assert_eq!(amber.ability_points, 20);
        assert_eq!(amber.rejuvenators, Some(2));

        let sapphire = tally(Team::SAPPHIRE, &players, &[], None);
        assert_eq!(sapphire.players, 1);
        assert_eq!(sapphire.souls, 500);
        assert_eq!(sapphire.rejuvenators, None);

        assert_eq!(amber.souls + sapphire.souls, 3500);
    }

    /// Kills on one side must equal deaths on the other; a mismatch means the
    /// aggregation picked up spectators or neutrals.
    #[test]
    fn team_kills_and_deaths_are_symmetric() {
        let players = [
            player(Team::AMBER, 0, 5, 2),
            player(Team::AMBER, 0, 3, 1),
            player(Team::SAPPHIRE, 0, 3, 8),
            player(Team::SPECTATOR, 0, 99, 99),
            player(Team::NEUTRAL, 0, 40, 40),
        ];
        let a = tally(Team::AMBER, &players, &[], None);
        let s = tally(Team::SAPPHIRE, &players, &[], None);
        assert_eq!((a.kills, a.deaths), (8, 3));
        assert_eq!((s.kills, s.deaths), (3, 8));
        assert_eq!(a.kills, s.deaths, "amber's kills are sapphire's deaths");
        assert_eq!(s.kills, a.deaths, "sapphire's kills are amber's deaths");
    }

    /// A structure on `team` whose state the read settled as `state`.
    fn structure(team: Team, state: StructureState) -> Objective {
        Objective {
            kind: ObjectiveKind::Walker,
            team_name: None,
            class: "C_NPC_Boss_Tier2".into(),
            address: 0x2000,
            team: Some(team),
            health: Some(4000),
            max_health: Some(4000),
            lane: Some(1),
            state,
            #[cfg(feature = "positions")]
            position: None,
        }
    }

    /// A destroyed structure belongs to neither count, and one side's structures must
    /// not land on the other's.
    #[test]
    fn only_standing_structures_count_toward_a_side() {
        let structures = [
            structure(Team::AMBER, StructureState::Standing),
            structure(Team::AMBER, StructureState::Destroyed),
            structure(Team::SAPPHIRE, StructureState::Standing),
        ];

        let amber = tally(Team::AMBER, &[], &structures, None);
        assert_eq!(amber.structures_alive, 1);
        assert!(amber.structures_known);

        let sapphire = tally(Team::SAPPHIRE, &[], &structures, None);
        assert_eq!(sapphire.structures_alive, 1);
        assert!(sapphire.structures_known);
    }

    /// The failure this guards: a structure nothing could be read from was counted as
    /// destroyed, so a side that still holds it reported one fewer, and the snapshot said
    /// nothing to distinguish that from a structure the other side had actually taken.
    #[test]
    fn a_structure_that_could_not_be_read_makes_the_count_a_floor_rather_than_shrinking_it() {
        let structures = [
            structure(Team::AMBER, StructureState::Standing),
            structure(Team::AMBER, StructureState::Unknown),
            structure(Team::SAPPHIRE, StructureState::Destroyed),
        ];

        let amber = tally(Team::AMBER, &[], &structures, None);
        assert_eq!(
            amber.structures_alive, 1,
            "an unread structure is neither claimed as standing nor written off"
        );
        assert!(
            !amber.structures_known,
            "and the count is reported as a floor"
        );

        let sapphire = tally(Team::SAPPHIRE, &[], &structures, None);
        assert_eq!(sapphire.structures_alive, 0);
        assert!(sapphire.structures_known);
    }

    /// `complete` is about the summed player statistics. One flag covering both would
    /// make a refused `m_PlayerDataGlobal` indistinguishable from a structure whose state
    /// could not be read, which is the confusion the second flag exists to prevent.
    #[test]
    fn an_unreadable_structure_and_an_unreadable_statistic_are_reported_separately() {
        let seen = tally(
            Team::AMBER,
            &[player(Team::AMBER, 1000, 3, 1)],
            &[structure(Team::AMBER, StructureState::Unknown)],
            None,
        );
        assert!(seen.complete, "every statistic was read");
        assert!(!seen.structures_known);

        let mut blind = player(Team::AMBER, 0, 0, 0);
        blind.kills = None;
        let counted = tally(
            Team::AMBER,
            &[blind],
            &[structure(Team::AMBER, StructureState::Standing)],
            None,
        );
        assert!(!counted.complete);
        assert!(counted.structures_known, "the structures were all read");
    }
}
