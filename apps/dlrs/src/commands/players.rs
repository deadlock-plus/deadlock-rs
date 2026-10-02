//! `dlrs players` and `dlrs watch`: the scoreboard, once or refreshed live.

use deadlock_core::HeroNames;
use deadlock_data::HeroCatalog;
use deadlock_reader::snapshot::{LiveSnapshot, MidbossSchedule};

use super::{attach, with_snapshot};
use crate::catalog::hero_catalog;
use crate::output::{opt, trunc};

pub fn players() -> i32 {
    with_snapshot(|r, s| {
        let mut buf = String::new();
        print_scoreboard(&mut buf, s, &hero_catalog(Some(r)));
        outln!("{}", buf.trim_end());
        0
    })
}

pub fn watch() -> i32 {
    use std::io::Write as _;

    let Some(r) = attach() else { return 1 };
    let heroes = hero_catalog(Some(&r));

    let mut out = std::io::stdout();
    let mut body = String::new();

    loop {
        body.clear();
        match r.live_snapshot() {
            Ok(Some(s)) => print_scoreboard(&mut body, &s, &heroes),
            Ok(None) => bufln!(body, "not in a match"),
            Err(e) => bufln!(body, "{e}"),
        }

        // Redraw without clearing the screen first. ESC[2J blanks everything, and with
        // the redraw arriving as dozens of separate writes the terminal gets a chance
        // to paint that blank state, which is the flicker. Instead: home the cursor,
        // overwrite each line and erase only that line's tail, then erase whatever is
        // left below in case this frame is shorter than the last.
        let mut frame = String::with_capacity(body.len() + 128);
        frame.push_str("\x1b[H");
        for line in body.lines() {
            frame.push_str(line);
            frame.push_str("\x1b[K\n");
        }
        frame.push_str("\x1b[J");

        // One write, one flush: the terminal never sees a half-drawn frame.
        if write!(out, "{frame}").and_then(|_| out.flush()).is_err() {
            return 0;
        }

        // 100 ms, not 1 s: a snapshot is well under a millisecond in release, and
        // polling once a second made the clock look up to a second behind the game.
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

pub(crate) fn print_scoreboard(buf: &mut String, s: &LiveSnapshot, heroes: &HeroCatalog) {
    print_match_header(buf, s, heroes);
    print_team_totals(buf, s);
    bufln!(buf, "");
    print_player_table(buf, s, heroes);
}

/// Match identity, pause state, midboss, objective timers, who you are, and the clock.
fn print_match_header(buf: &mut String, s: &LiveSnapshot, heroes: &HeroCatalog) {
    bufln!(
        buf,
        "{}  match {:?}  state={}",
        s.describe(),
        s.match_id,
        s.game_state.map(|g| g.name()).unwrap_or("?")
    );
    bufln!(
        buf,
        "paused={}  (game={:?} server={:?} start_tick={:?} total_ticks={:?})",
        s.paused.unwrap_or(false),
        s.pause.game_paused,
        s.pause.server_paused,
        s.pause.pause_start_tick,
        s.pause.total_paused_ticks
    );
    bufln!(
        buf,
        "midboss: killed {}x, currently {}{}",
        s.midboss_kills.unwrap_or(0),
        if s.midboss_alive() {
            "alive"
        } else {
            "not on map"
        },
        match s.midboss_schedule() {
            MidbossSchedule::In(d) => {
                format!(", respawns in {}:{:02}", d as u32 / 60, d as u32 % 60)
            }
            // Said rather than shown as a countdown at zero, which claims an imminent
            // spawn: live, that claim held for a minute while the Midboss came and went.
            MidbossSchedule::Overdue(by) => {
                format!(
                    ", respawn overdue by {}:{:02}",
                    by as u32 / 60,
                    by as u32 % 60
                )
            }
            MidbossSchedule::Alive | MidbossSchedule::Unscheduled => String::new(),
        }
    );
    print_timers(buf, s);
    bufln!(
        buf,
        "you:     {} ({:?})",
        s.current_player()
            .map(|p| {
                format!(
                    "{} slot {} - {}",
                    p.team_name.clone().unwrap_or_else(|| "?".into()),
                    p.slot.map(|x| x.to_string()).unwrap_or_else(|| "-".into()),
                    p.hero_id
                        .map(|h| heroes.display_name(h))
                        .unwrap_or_else(|| "?".into())
                )
            })
            .unwrap_or_else(|| "not identified".into()),
        s.perspective
    );
    bufln!(
        buf,
        "clock:   {} match time ({} wall, {} paused ticks)",
        s.timers.display().unwrap_or_else(|| "?".into()),
        s.clock.elapsed_display().unwrap_or_else(|| "?".into()),
        s.clock.total_paused_ticks.unwrap_or(0)
    );
}

/// Both sides' totals, and who is ahead on souls.
fn print_team_totals(buf: &mut String, s: &LiveSnapshot) {
    for t in &s.teams {
        bufln!(
            buf,
            "{:<9} {:>7} souls  {:>3}/{:>3}/{:>3} k/d/a  {:>4} lh  {:>3} den  rejuv {}  {} structures",
            s.team_name(t.team),
            t.souls,
            t.kills,
            t.deaths,
            t.assists,
            t.last_hits,
            t.denies,
            t.rejuvenators.unwrap_or(0),
            t.structures_alive
        );
        bufln!(
            buf,
            "{:<9} {:>7} hero dmg  {:>6} obj dmg  {:>6} healing  {:>4} AP",
            "",
            t.hero_damage,
            t.objective_damage,
            t.healing,
            t.ability_points
        );
    }
    if let Some((team, lead)) = s.soul_lead() {
        if lead > 0 {
            bufln!(buf, "soul lead: {} +{lead}", s.team_name(team));
        } else {
            bufln!(buf, "soul lead: dead even");
        }
    }
}

/// One row per player, plus a note when spectators are present but not listed.
fn print_player_table(buf: &mut String, s: &LiveSnapshot, heroes: &HeroCatalog) {
    bufln!(
        buf,
        "{:<10} {:>4} {:<20} {:<14} {:>4} {:>4} {:>4} {:>4} {:>4} {:>8} {:>8} {:>8} {:>7}",
        "team",
        "slot",
        "player",
        "hero",
        "lvl",
        "k",
        "d",
        "a",
        "lh",
        "networth",
        "herodmg",
        "objdmg",
        "hp"
    );
    for p in s.scoreboard() {
        // Mark anyone who has left; a bare scoreboard cannot otherwise show it.
        let label = if p.has_abandoned() {
            format!("{} (left)", p.display_name())
        } else {
            p.display_name()
        };
        bufln!(
            buf,
            "{:<10} {:>4} {:<20} {:<14} {:>4} {:>4} {:>4} {:>4} {:>4} {:>8} {:>8} {:>8} {:>7}",
            p.team_name.clone().unwrap_or_else(|| "?".into()),
            opt(p.slot),
            trunc(&label, 20),
            p.hero_id
                .map(|h| heroes.display_name(h))
                .unwrap_or_else(|| "-".into()),
            opt(p.level),
            opt(p.kills),
            opt(p.deaths),
            opt(p.assists),
            opt(p.last_hits),
            opt(p.net_worth),
            opt(p.hero_damage),
            opt(p.objective_damage),
            p.health
                .map(|h| h.to_string())
                .unwrap_or_else(|| "-".into()),
        );
    }
    let spec = s.spectators().count();
    if spec > 0 {
        bufln!(buf, "({spec} spectator(s) not shown)");
    }
}

/// One line per objective timer, tagged with where the number came from so a schedule
/// estimate is never mistaken for something the game said.
fn print_timers(buf: &mut String, s: &LiveSnapshot) {
    use deadlock_reader::timers::{ObjectiveTimer, TimerSource};

    let row = |buf: &mut String, label: &str, t: &Option<ObjectiveTimer>| {
        let Some(t) = t else { return };
        let state = match (t.present, t.count) {
            (Some(true), Some(n)) if n > 1 => format!("up x{n}"),
            (Some(true), _) => "up".to_string(),
            (Some(false), _) => "down".to_string(),
            (None, _) => "?".to_string(),
        };
        let when = match (t.display(), t.source) {
            (Some(d), TimerSource::Game) => format!(", next in {d}"),
            (Some(d), TimerSource::Schedule) => format!(", next in ~{d} (estimated)"),
            (Some(d), TimerSource::Unknown) => format!(", next in {d}?"),
            (None, TimerSource::Unknown) => ", next spawn not published to the client".to_string(),
            (None, _) => String::new(),
        };
        bufln!(buf, "  {label:<13} {state}{when}");
    };
    bufln!(
        buf,
        "timers:  {} match time",
        s.timers.display().unwrap_or_else(|| "?".into())
    );
    row(buf, "midboss", &s.timers.midboss);
    row(buf, "urn", &s.timers.urn);
    row(buf, "unstable rift", &s.timers.rift);
    row(buf, "bridge buffs", &s.timers.bridge_buffs);
    if s.timers.rift_state.contested(s.clock.now) {
        bufln!(
            buf,
            "  rift contested by {:?}{}",
            s.timers.rift_state.scoring_team,
            s.timers
                .rift_state
                .seconds_until_give_up(s.clock.now)
                .map(|d| format!(", lapses in {d:.0}s"))
                .unwrap_or_default()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deadlock_core::{ConnectionState, GameMode, GameState, HeroId, MatchMode, Team};
    use deadlock_reader::snapshot::{
        Context, MatchClock, Objective, ObjectiveKind, PlayerRow, StructureState, TeamStats,
    };
    use deadlock_reader::timers::Timers;

    /// A snapshot is `Default` with public fields, so the whole scoreboard can be
    /// exercised against a synthetic one with no game running. That is the only reason
    /// these four functions have any coverage at all.
    fn snapshot() -> LiveSnapshot {
        LiveSnapshot {
            context: Context::Match,
            match_id: Some(42),
            match_mode: Some(MatchMode::Ranked),
            game_mode: Some(GameMode::Normal),
            game_state: Some(GameState::GameInProgress),
            clock: MatchClock {
                now: Some(600.0),
                ..MatchClock::default()
            },
            timers: Timers {
                match_time: Some(605.0),
                ..Timers::default()
            },
            teams: vec![team(Team::AMBER, 12_000), team(Team::SAPPHIRE, 10_000)],
            players: vec![
                player(1, Team::AMBER, "alice"),
                player(7, Team::SAPPHIRE, "bob"),
            ],
            ..LiveSnapshot::default()
        }
    }

    fn team(team: Team, souls: u32) -> TeamStats {
        TeamStats {
            team,
            souls,
            ..TeamStats::default()
        }
    }

    fn player(slot: u32, team: Team, name: &str) -> PlayerRow {
        PlayerRow {
            slot: Some(slot),
            team: Some(team),
            team_name: Some(team.default_name().to_string()),
            name: Some(name.to_string()),
            hero_id: Some(HeroId(1)),
            level: Some(12),
            health: Some(800),
            ..PlayerRow::default()
        }
    }

    fn midboss() -> Objective {
        Objective {
            kind: ObjectiveKind::Midboss,
            team_name: None,
            class: "C_NPC_MidBoss".into(),
            address: 0,
            team: None,
            health: Some(5000),
            max_health: Some(5000),
            lane: None,
            state: StructureState::Standing,
            #[cfg(feature = "positions")]
            position: None,
        }
    }

    fn render(s: &LiveSnapshot) -> String {
        let mut buf = String::new();
        print_scoreboard(&mut buf, s, &HeroCatalog::bundled());
        buf
    }

    #[test]
    fn every_scoreboard_player_gets_a_row_and_nobody_else_does() {
        let mut s = snapshot();
        s.players.push(PlayerRow {
            team: Some(Team::SPECTATOR),
            is_spectator: true,
            name: Some("watcher".into()),
            ..PlayerRow::default()
        });
        let out = render(&s);
        assert!(out.contains("alice"), "{out}");
        assert!(out.contains("bob"), "{out}");
        assert!(!out.contains("watcher"), "{out}");
        assert!(out.contains("(1 spectator(s) not shown)"), "{out}");
    }

    #[test]
    fn the_spectator_footer_is_absent_when_there_are_none() {
        assert!(!render(&snapshot()).contains("spectator(s)"));
    }

    /// Hero ids resolve through the catalog, and a row with no hero still renders.
    #[test]
    fn heroes_are_named_and_a_missing_one_is_a_dash() {
        let heroes = HeroCatalog::bundled();
        let mut s = snapshot();
        s.players[1].hero_id = None;
        let out = render(&s);
        assert!(out.contains(&heroes.display_name(HeroId(1))), "{out}");
        let bob = out
            .lines()
            .find(|l| l.contains("bob"))
            .expect("bob has a row");
        assert!(bob.contains(" - "), "{bob}");
    }

    /// User-supplied Steam names are cut to the column, never past it.
    #[test]
    fn an_over_wide_name_is_truncated_to_its_column() {
        let mut s = snapshot();
        s.players[0].name = Some("a".repeat(60));
        let out = render(&s);
        assert!(out.contains(&format!("{}...", "a".repeat(17))), "{out}");
        assert!(!out.contains(&"a".repeat(21)), "{out}");
    }

    #[test]
    fn a_player_who_left_is_marked() {
        let mut s = snapshot();
        s.players[0].connected = Some(ConnectionState::Disconnected);
        s.players[1].connected = Some(ConnectionState::Connected);
        let out = render(&s);
        assert!(out.contains("alice (left)"), "{out}");
        assert!(!out.contains("bob (left)"), "{out}");
    }

    /// `Reconnecting` is mid-drop, not gone, and must not be labelled as having left.
    #[test]
    fn a_reconnecting_player_is_not_marked_as_having_left() {
        let mut s = snapshot();
        s.players[0].connected = Some(ConnectionState::Reconnecting);
        assert!(!render(&s).contains("(left)"));
    }

    #[test]
    fn the_pause_line_follows_the_pause_state() {
        let mut s = snapshot();
        assert!(render(&s).contains("paused=false"));
        s.paused = Some(true);
        s.pause.pause_start_tick = Some(9000);
        let out = render(&s);
        assert!(out.contains("paused=true"), "{out}");
        assert!(out.contains("start_tick=Some(9000)"), "{out}");
    }

    #[test]
    fn a_live_midboss_reads_as_alive_and_offers_no_countdown() {
        let mut s = snapshot();
        s.objectives.push(midboss());
        s.midboss_next_spawn_time = Some(900.0);
        s.midboss_kills = Some(2);
        let out = render(&s);
        assert!(out.contains("midboss: killed 2x, currently alive"), "{out}");
        assert!(!out.contains("respawns in"), "{out}");
    }

    #[test]
    fn a_dead_midboss_counts_down_to_its_respawn() {
        let mut s = snapshot();
        s.midboss_next_spawn_time = Some(600.0 + 125.0);
        let out = render(&s);
        assert!(
            out.contains("currently not on map, respawns in 2:05"),
            "{out}"
        );
    }

    /// A schedule that has lapsed says so instead of showing a countdown at zero.
    ///
    /// Live, `next spawn 0:00` held for a full minute of match time while
    /// `m_iMidbossKillCount` went from 1 to 2 - so the Midboss spawned and died while the
    /// countdown still claimed it was about to arrive. Zero is also what a spawn happening
    /// this instant looks like, and a reader could not tell the two apart.
    #[test]
    fn a_lapsed_midboss_schedule_says_so_rather_than_counting_down_to_zero() {
        let mut s = snapshot();
        s.midboss_next_spawn_time = Some(600.0 - 95.0);
        let out = render(&s);
        assert!(out.contains("respawn overdue by 1:35"), "{out}");
        assert!(
            !out.contains("respawns in"),
            "an overdue spawn must not read as a countdown: {out}"
        );
    }

    /// No scheduled spawn is a countdown that cannot be computed, not a zero.
    #[test]
    fn a_midboss_with_no_scheduled_spawn_shows_no_countdown() {
        let out = render(&snapshot());
        assert!(out.contains("currently not on map"), "{out}");
        assert!(!out.contains("respawns in"), "{out}");
    }

    #[test]
    fn the_soul_lead_names_the_side_ahead() {
        let mut s = snapshot();
        assert!(render(&s).contains("soul lead: Amber +2000"));
        s.teams = vec![team(Team::AMBER, 10_000), team(Team::SAPPHIRE, 12_000)];
        assert!(render(&s).contains("soul lead: Sapphire +2000"));
    }

    /// A dead heat is its own wording: "+0" would read as a lead of nothing for a side
    /// that is not actually ahead.
    #[test]
    fn a_dead_heat_says_so() {
        let mut s = snapshot();
        s.teams = vec![team(Team::AMBER, 10_000), team(Team::SAPPHIRE, 10_000)];
        let out = render(&s);
        assert!(out.contains("soul lead: dead even"), "{out}");
        assert!(!out.contains("+0"), "{out}");
    }

    /// `soul_lead` needs both sides. One-sided team stats happen early in a match, and
    /// the line is dropped rather than guessed at.
    #[test]
    fn one_sided_team_stats_print_no_soul_lead() {
        let mut s = snapshot();
        s.teams = vec![team(Team::AMBER, 10_000)];
        assert!(!render(&s).contains("soul lead"));
    }

    #[test]
    fn the_you_line_names_whoever_the_client_is_being() {
        let heroes = HeroCatalog::bundled();
        let mut s = snapshot();
        assert!(render(&s).contains("you:     not identified"));

        s.players[0].is_local = Some(true);
        let out = render(&s);
        assert!(
            out.contains(&format!(
                "you:     Amber slot 1 - {}",
                heroes.display_name(HeroId(1))
            )),
            "{out}"
        );
    }

    /// While spectating, your own controller is not a scoreboard row, so "you" is
    /// whoever the camera is following.
    #[test]
    fn while_spectating_the_you_line_follows_the_camera() {
        let mut s = snapshot();
        s.players.push(PlayerRow {
            team: Some(Team::SPECTATOR),
            is_spectator: true,
            is_local: Some(true),
            ..PlayerRow::default()
        });
        s.players[1].is_observed = true;
        let out = render(&s);
        assert!(out.contains("you:     Sapphire slot 7 - "), "{out}");
    }

    /// The header, the totals and the table are three separate helpers writing into one
    /// buffer; this is what pins that they all still run, and in that order.
    #[test]
    fn the_frame_is_header_then_totals_then_table() {
        let out = render(&snapshot());
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("{needle} missing:\n{out}"))
        };
        assert!(at("Ranked / Normal") < at("12000 souls"));
        assert!(at("12000 souls") < at("soul lead:"));
        assert!(at("soul lead:") < at("networth"));
        assert!(at("networth") < at("alice"));
    }
}
