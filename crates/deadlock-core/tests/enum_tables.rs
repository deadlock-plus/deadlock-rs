//! Every raw value these enums understand, and the name each one reports.
//!
//! This is a characterisation test: it was generated from the implementation as it stood
//! *before* the ten hand-written `from_raw`/`name` pairs were collapsed into a macro, so it
//! pins the behaviour the refactor had to preserve rather than restating the new code.
//!
//! It keeps earning its place afterwards. The value and the name live in one table now, but
//! a game update that renumbers an enum still shows up here as a changed expectation rather
//! than as a scoreboard quietly reporting the wrong phase - which is the failure this
//! crate's `Drift` machinery exists to catch elsewhere.
//!
//! Each table runs past the last known variant on purpose: the `Unknown` fallback is part
//! of the contract, and an off-by-one at the end of a match arm is exactly what a
//! hand-written version gets wrong.

use deadlock_core::{
    BotDifficulty, ChatMode, ConnectionState, GameMode, GameState, MatchMode, MmPreference,
    PlayerType, RankedType, RegionMode,
};

/// Assert one enum's whole value-to-name table.
macro_rules! check {
    ($ty:ident, $method:ident, $range:expr, $expected:expr) => {{
        let expected: &[(_, &str)] = $expected;
        let got: Vec<_> = $range
            .map(|v| (v, $ty::from_raw(v).$method()))
            .collect::<Vec<_>>();
        let want: Vec<_> = expected.iter().map(|&(v, n)| (v, n)).collect();
        assert_eq!(got, want, "{} table changed", stringify!($ty));
    }};
}

#[test]
fn game_state_names_every_value_it_knows() {
    check!(
        GameState,
        name,
        0u32..14,
        &[
            (0, "Invalid"),
            (1, "Init"),
            (2, "WaitingForPlayersToJoin"),
            (3, "HeroSelection"),
            (4, "MatchIntro"),
            (5, "WaitForMapToLoad"),
            (6, "PreGameWait"),
            (7, "GameInProgress"),
            (8, "PostGame"),
            (9, "PostGame_PlayOfTheGame"),
            (10, "Abandoned"),
            (11, "End"),
            (12, "Unknown"),
            (13, "Unknown"),
        ]
    );
}

#[test]
fn match_mode_names_every_value_it_knows() {
    check!(
        MatchMode,
        name,
        0u32..10,
        &[
            (0, "Invalid"),
            (1, "Unranked"),
            (2, "PrivateLobby"),
            (3, "CoopBot"),
            (4, "Ranked"),
            (5, "ServerTest"),
            (6, "Tutorial"),
            (7, "HeroLabs"),
            (8, "NewPlayerPlacement"),
            (9, "Unknown"),
        ]
    );
}

#[test]
fn game_mode_names_every_value_it_knows() {
    check!(
        GameMode,
        name,
        0u32..12,
        &[
            (0, "Invalid"),
            (1, "Normal"),
            (2, "1v1Test"),
            (3, "Sandbox"),
            (4, "StreetBrawl"),
            (5, "ExploreNYC"),
            (6, "Internal"),
            (7, "Unknown"),
            (8, "Unknown"),
            (9, "Unknown"),
            (10, "Unknown"),
            (11, "Unknown"),
        ]
    );
}

#[test]
fn region_mode_names_every_value_it_knows() {
    check!(
        RegionMode,
        name,
        0u32..10,
        &[
            (0, "Rest of World"),
            (1, "Europe"),
            (2, "SE Asia"),
            (3, "S America"),
            (4, "Russia"),
            (5, "Oceania"),
            (6, "Unknown"),
            (7, "Unknown"),
            (8, "Unknown"),
            (9, "Unknown"),
        ]
    );
}

#[test]
fn mm_preference_names_every_value_it_knows() {
    check!(
        MmPreference,
        name,
        0u32..6,
        &[
            (0, "Invalid"),
            (1, "Casual"),
            (2, "Serious"),
            (3, "Unknown"),
            (4, "Unknown"),
            (5, "Unknown"),
        ]
    );
}

#[test]
fn chat_mode_names_every_value_it_knows() {
    check!(
        ChatMode,
        name,
        0u32..6,
        &[
            (0, "None"),
            (1, "Party"),
            (2, "Team"),
            (3, "Unknown"),
            (4, "Unknown"),
            (5, "Unknown"),
        ]
    );
}

#[test]
fn bot_difficulty_names_every_value_it_knows() {
    check!(
        BotDifficulty,
        name,
        0u32..10,
        &[
            (0, "None"),
            (1, "Easy"),
            (2, "Medium"),
            (3, "Hard"),
            (4, "Nightmare"),
            (5, "Guided"),
            (6, "Unknown"),
            (7, "Unknown"),
            (8, "Unknown"),
            (9, "Unknown"),
        ]
    );
}

#[test]
fn ranked_type_names_every_value_it_knows() {
    check!(
        RankedType,
        name,
        0u32..6,
        &[
            (0, "Invalid"),
            (1, "Normal"),
            (2, "Unknown"),
            (3, "Unknown"),
            (4, "Unknown"),
            (5, "Unknown"),
        ]
    );
}

#[test]
fn player_type_names_every_value_it_knows() {
    check!(
        PlayerType,
        name,
        0u32..6,
        &[
            (0, "Player"),
            (1, "Spectator"),
            (2, "Unknown"),
            (3, "Unknown"),
            (4, "Unknown"),
            (5, "Unknown"),
        ]
    );
}

/// The only signed one, and the only one whose accessor is `as_str` rather than `name`.
/// `-1` being a real variant is what makes the signed type necessary.
#[test]
fn connection_state_names_every_value_it_knows() {
    check!(
        ConnectionState,
        as_str,
        -2i32..8,
        &[
            (-2, "Unknown"),
            (-1, "PlayerNeverConnected"),
            (0, "PlayerConnected"),
            (1, "PlayerConnecting"),
            (2, "PlayerReconnecting"),
            (3, "PlayerDisconnecting"),
            (4, "PlayerDisconnected"),
            (5, "PlayerReserved"),
            (6, "Unknown"),
            (7, "Unknown"),
        ]
    );
}

/// An unrecognised value has to survive the round trip. Reporting the name "Unknown" while
/// losing the number would leave a caller unable to say *what* it did not recognise, which
/// is the only thing that makes such a report actionable.
#[test]
fn unknown_values_keep_the_number_they_came_from() {
    assert_eq!(GameState::from_raw(99), GameState::Unknown(99));
    assert_eq!(MatchMode::from_raw(99), MatchMode::Unknown(99));
    assert_eq!(GameMode::from_raw(99), GameMode::Unknown(99));
    assert_eq!(RegionMode::from_raw(99), RegionMode::Unknown(99));
    assert_eq!(MmPreference::from_raw(99), MmPreference::Unknown(99));
    assert_eq!(ChatMode::from_raw(99), ChatMode::Unknown(99));
    assert_eq!(BotDifficulty::from_raw(99), BotDifficulty::Unknown(99));
    assert_eq!(RankedType::from_raw(99), RankedType::Unknown(99));
    assert_eq!(PlayerType::from_raw(99), PlayerType::Unknown(99));
    assert_eq!(
        ConnectionState::from_raw(-99),
        ConnectionState::Unknown(-99)
    );
}

/// The predicates are the part with real logic behind them, and the part a mechanical
/// refactor of the surrounding code is most likely to disturb without any name changing.
#[test]
fn the_predicates_still_pick_out_the_same_values() {
    let live: Vec<u32> = (0..14)
        .filter(|&v| GameState::from_raw(v).is_live())
        .collect();
    assert_eq!(live, vec![7], "only GameInProgress is live");

    let over: Vec<u32> = (0..14)
        .filter(|&v| GameState::from_raw(v).is_over())
        .collect();
    assert_eq!(
        over,
        vec![8, 9, 10, 11],
        "PostGame, PlayOfTheGame, Abandoned, End"
    );

    let ranked: Vec<u32> = (0..10)
        .filter(|&v| MatchMode::from_raw(v).is_ranked())
        .collect();
    assert_eq!(ranked, vec![4]);

    let custom: Vec<u32> = (0..10)
        .filter(|&v| MatchMode::from_raw(v).is_custom())
        .collect();
    assert_eq!(custom, vec![2]);

    let in_match: Vec<i32> = (-2..8)
        .filter(|&v| ConnectionState::from_raw(v).is_in_match())
        .collect();
    assert_eq!(in_match, vec![0, 2], "Connected, and Reconnecting mid-drop");

    let gone: Vec<i32> = (-2..8)
        .filter(|&v| ConnectionState::from_raw(v).is_gone())
        .collect();
    assert_eq!(gone, vec![3, 4]);
}
