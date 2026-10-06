//! Game, match and lobby state, as the game's own enums number it.
//!
//! Values were read from the game's own schema bindings (`EGameState`,
//! `ECitadelMatchMode`, `ECitadelGameMode`), not inferred from string order - an earlier
//! attempt at the latter was wrong twice over.
//!
//! Every enum here is declared through one `raw_enum!` macro, which puts the number and the
//! display name for a variant on one line. They used to be two separate `match` blocks per
//! enum, edited independently: the number lived in `from_raw` and the name in `name`, ten
//! times over, and nothing connected them. `crates/deadlock-core/tests/enum_tables.rs`
//! pins every pair.

/// Declare a raw-valued game enum, its `from_raw`, and its name accessor.
///
/// The shape is always the same - a fixed set of numbered variants plus an `Unknown`
/// carrying whatever was actually read - and writing it out produced two parallel `match`
/// blocks per enum with no compiler-enforced relationship between them. Ten enums meant
/// twenty such blocks and about five hundred lines, in which a renumbering could be applied
/// to one half and not the other.
///
/// Here the value and the label sit together on the variant's own line, so they cannot
/// disagree.
///
/// ```text
/// raw_enum! {
///     /// Doc for the enum itself.
///     ChatMode: u32, from "EChatMode", named name {
///         /// Doc for the variant.
///         None = 0 => "None",
///         Party = 1 => "Party",
///     }
/// }
/// ```
///
/// `named` exists because one of these predates the convention: [`ConnectionState`] calls
/// its accessor `as_str`, and renaming a public method to tidy the macro up would be a
/// breaking change made for the macro's convenience.
macro_rules! raw_enum {
    (
        $(#[$meta:meta])*
        $name:ident : $raw:ty, from $field:literal, named $accessor:ident {
            $(
                $(#[$vmeta:meta])*
                $variant:ident = $value:literal => $label:literal,
            )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize))]
        pub enum $name {
            $(
                $(#[$vmeta])*
                $variant,
            )*
            /// A value this build does not know about, kept as it was read.
            Unknown($raw),
        }

        impl $name {
            #[doc = concat!("Map a raw `", $field, "` value.")]
            ///
            /// A number this build does not know becomes `Unknown` carrying that number,
            /// rather than being collapsed into a default - a caller reporting drift needs
            /// to be able to say *which* value it did not recognise.
            pub fn from_raw(v: $raw) -> Self {
                match v {
                    $( $value => Self::$variant, )*
                    other => Self::Unknown(other),
                }
            }

            /// Short name, for display and logs.
            ///
            /// An unrecognised value reports `"Unknown"`; the number is in the `Unknown`
            /// payload.
            pub fn $accessor(&self) -> &'static str {
                match self {
                    $( Self::$variant => $label, )*
                    Self::Unknown(_) => "Unknown",
                }
            }
        }
    };
}

raw_enum! {
    /// Game phase (`C_CitadelGameRules::m_eGameState`, enum `EGameState`).
    ///
    /// Values were read from the game's own schema, not inferred. Note `Invalid = 0` shifts
    /// everything, and `WaitingForPlayersToJoin` / `WaitForMapToLoad` are **not** in the
    /// order the original binary's string pool lists them.
    GameState: u32, from "m_eGameState", named name {
        /// Nothing running.
        Invalid = 0 => "Invalid",
        /// Server starting up.
        Init = 1 => "Init",
        /// Lobby filling.
        WaitingForPlayersToJoin = 2 => "WaitingForPlayersToJoin",
        /// Hero pick phase.
        ///
        /// The game defines it and does not use it. Picking a hero is a UI menu opened
        /// before queueing, and a live client never reports this value for it. It was seen
        /// for a single tick while the Hideout loaded, which is not a signal. Do not map
        /// it to a "hero select" screen, and do not let it set a phase; read
        /// `LiveSnapshot::menu` for the menu instead.
        HeroSelection = 3 => "HeroSelection",
        /// Intro cinematic.
        MatchIntro = 4 => "MatchIntro",
        /// Loading the map.
        WaitForMapToLoad = 5 => "WaitForMapToLoad",
        /// Pre-game countdown.
        PreGameWait = 6 => "PreGameWait",
        /// Gameplay.
        GameInProgress = 7 => "GameInProgress",
        /// Match over, scoreboard up.
        PostGame = 8 => "PostGame",
        /// Play-of-the-game replay.
        PostGamePlayOfTheGame = 9 => "PostGame_PlayOfTheGame",
        /// Match abandoned.
        Abandoned = 10 => "Abandoned",
        /// Terminator in the game's enum.
        End = 11 => "End",
    }
}

impl GameState {
    /// Whether gameplay is running.
    ///
    /// True in the Hideout too - the Hideout reports [`GameState::GameInProgress`]. Use
    /// `LiveSnapshot::is_hideout` to tell them apart.
    pub fn is_live(&self) -> bool {
        matches!(self, GameState::GameInProgress)
    }

    /// Whether the match is over and the client is showing the aftermath.
    ///
    /// The scoreboard and every player row are still readable in these states, which is
    /// the only window in which final match data can be captured. Observed over 67 real
    /// match endings: every one passed through [`GameState::PostGame`], only about 40%
    /// reached [`GameState::End`], and `PostGame_PlayOfTheGame` never appeared at all.
    /// Trigger on the transition into this set rather than waiting for a specific state.
    pub fn is_over(&self) -> bool {
        matches!(
            self,
            GameState::PostGame
                | GameState::PostGamePlayOfTheGame
                | GameState::Abandoned
                | GameState::End
        )
    }
}

raw_enum! {
    /// Matchmaking mode (`C_CitadelGameRules::m_eMatchMode`, enum `ECitadelMatchMode`).
    MatchMode: u32, from "m_eMatchMode", named name {
        /// Not in a matchmade game - includes sitting in the Hideout.
        Invalid = 0 => "Invalid",
        /// Standard unranked queue.
        Unranked = 1 => "Unranked",
        /// Custom / private lobby.
        PrivateLobby = 2 => "PrivateLobby",
        /// Co-op versus bots.
        CoopBot = 3 => "CoopBot",
        /// Ranked queue.
        Ranked = 4 => "Ranked",
        /// Valve server test.
        ServerTest = 5 => "ServerTest",
        /// Tutorial.
        Tutorial = 6 => "Tutorial",
        /// Hero Labs.
        HeroLabs = 7 => "HeroLabs",
        /// New player placement matches.
        NewPlayerPlacement = 8 => "NewPlayerPlacement",
    }
}

impl MatchMode {
    /// Whether this mode affects matchmaking rating.
    pub fn is_ranked(&self) -> bool {
        matches!(self, MatchMode::Ranked)
    }

    /// Whether this is a player-made lobby rather than a queued game.
    pub fn is_custom(&self) -> bool {
        matches!(self, MatchMode::PrivateLobby)
    }
}

raw_enum! {
    /// Game mode (`C_CitadelGameRules::m_eGameMode`, enum `ECitadelGameMode`).
    GameMode: u32, from "m_eGameMode", named name {
        /// Not in a game - includes sitting in the Hideout.
        Invalid = 0 => "Invalid",
        /// Standard 6v6.
        Normal = 1 => "Normal",
        /// 1v1 test mode.
        OneVsOneTest = 2 => "1v1Test",
        /// Sandbox / practice.
        Sandbox = 3 => "Sandbox",
        /// Street Brawl.
        StreetBrawl = 4 => "StreetBrawl",
        /// Explore NYC.
        ExploreNyc = 5 => "ExploreNYC",
        /// Valve internal.
        Internal = 6 => "Internal",
    }
}

raw_enum! {
    /// Connection state of a player slot (`CBasePlayerController::m_iConnected`, enum
    /// `PlayerConnectedState`).
    ///
    /// Values read from the game's own schema. Note the enum is *signed*:
    /// `PlayerNeverConnected` is `-1`, so read `m_iConnected` as an `i32`.
    ConnectionState: i32, from "m_iConnected", named as_str {
        /// Slot exists but nobody has ever occupied it.
        NeverConnected = -1 => "PlayerNeverConnected",
        /// Present and playing.
        Connected = 0 => "PlayerConnected",
        /// Joining for the first time.
        Connecting = 1 => "PlayerConnecting",
        /// Dropped and coming back.
        Reconnecting = 2 => "PlayerReconnecting",
        /// On the way out.
        Disconnecting = 3 => "PlayerDisconnecting",
        /// Gone.
        Disconnected = 4 => "PlayerDisconnected",
        /// Slot held open, e.g. for a player who has not loaded in yet.
        Reserved = 5 => "PlayerReserved",
    }
}

impl ConnectionState {
    /// Whether the player is in the match right now.
    ///
    /// `Reconnecting` counts as connected: they are mid-drop, not gone, and a scoreboard
    /// should keep showing them.
    pub fn is_in_match(self) -> bool {
        matches!(
            self,
            ConnectionState::Connected | ConnectionState::Reconnecting
        )
    }

    /// Whether the player has left and is not coming back.
    pub fn is_gone(self) -> bool {
        matches!(
            self,
            ConnectionState::Disconnecting | ConnectionState::Disconnected
        )
    }
}

raw_enum! {
    /// Which matchmaking region a party is queueing in (`ECitadelRegionMode`).
    RegionMode: u32, from "ECitadelRegionMode", named name {
        /// Rest of world, and the default when nothing has been chosen.
        RestOfWorld = 0 => "Rest of World",
        /// Europe.
        Europe = 1 => "Europe",
        /// South-east Asia.
        SeAsia = 2 => "SE Asia",
        /// South America.
        SAmerica = 3 => "S America",
        /// Russia.
        Russia = 4 => "Russia",
        /// Oceania.
        Oceania = 5 => "Oceania",
    }
}

raw_enum! {
    /// How seriously a party wants to be matched (`ECitadelMMPreference`).
    MmPreference: u32, from "ECitadelMMPreference", named name {
        /// Not set.
        Invalid = 0 => "Invalid",
        /// Casual.
        Casual = 1 => "Casual",
        /// Serious.
        Serious = 2 => "Serious",
    }
}

raw_enum! {
    /// Which chat channel a party is talking in (`CSOCitadelParty.EChatMode`).
    ChatMode: u32, from "EChatMode", named name {
        /// No channel selected.
        None = 0 => "None",
        /// Party chat.
        Party = 1 => "Party",
        /// Team chat.
        Team = 2 => "Team",
    }
}

raw_enum! {
    /// Bot difficulty for a co-op game (`ECitadelBotDifficulty`).
    BotDifficulty: u32, from "ECitadelBotDifficulty", named name {
        /// No bots.
        None = 0 => "None",
        /// Easy.
        Easy = 1 => "Easy",
        /// Medium.
        Medium = 2 => "Medium",
        /// Hard.
        Hard = 3 => "Hard",
        /// Nightmare.
        Nightmare = 4 => "Nightmare",
        /// Guided.
        Guided = 5 => "Guided",
    }
}

raw_enum! {
    /// Which ranked ladder a score belongs to (`ECitadelRankedType`).
    RankedType: u32, from "ECitadelRankedType", named name {
        /// Not a ranked score.
        Invalid = 0 => "Invalid",
        /// The normal ladder.
        Normal = 1 => "Normal",
    }
}

raw_enum! {
    /// Whether a party member is playing or spectating (`CSOCitadelParty.EPlayerType`).
    PlayerType: u32, from "EPlayerType", named name {
        /// An ordinary player.
        Player = 0 => "Player",
        /// A spectator.
        Spectator = 1 => "Spectator",
    }
}

raw_enum! {
    /// Which client a party member is playing from (`EGCPlatform`).
    ///
    /// Unlike the enums above, these numbers could not be read back off a running client:
    /// `EGCPlatform` is a Game Coordinator enum and is absent from the client's runtime
    /// schema. `Protobufs/steammessages.proto` is the only source, so a renumbering there
    /// would go unnoticed until a member reported an `Unknown`.
    ///
    /// Decodes `valveprotos::deadlock::cso_citadel_party::Member::platform`.
    Platform: u32, from "EGCPlatform", named name {
        /// Not reported.
        None = 0 => "None",
        /// Windows.
        Pc = 1 => "PC",
        /// macOS.
        Mac = 2 => "Mac",
        /// Linux.
        Linux = 3 => "Linux",
        /// Android.
        Android = 4 => "Android",
        /// Apple `iOS`.
        Ios = 5 => "iOS",
    }
}

raw_enum! {
    /// Which side a member has been put on inside a private lobby (`ECitadelLobbyTeam`).
    ///
    /// **This is not [`Team`](crate::Team).** The two number teams differently and share
    /// no value: a lobby's `Team1` is 1, which is [`Team::SPECTATOR`](crate::Team), and a
    /// lobby's `Spectator` is 16, which [`Team`](crate::Team) does not define at all.
    /// `Team0` and `Team1` are lobby slots, and which of Amber and Sapphire each becomes
    /// is settled by the server when the match starts, so there is no conversion between
    /// the two types here to reach for by mistake.
    ///
    /// Like [`Platform`] these numbers come from the published proto only; the client's
    /// runtime schema does not carry `ECitadelLobbyTeam`. The gap between 1 and 16 is what
    /// the proto declares, not a transcription loss.
    ///
    /// Decodes `valveprotos::deadlock::cso_citadel_party::Member::team`.
    LobbyTeam: u32, from "ECitadelLobbyTeam", named name {
        /// First lobby side.
        Team0 = 0 => "Team0",
        /// Second lobby side.
        Team1 = 1 => "Team1",
        /// Watching rather than playing.
        Spectator = 16 => "Spectator",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::Team;

    /// Assert one enum's whole value-to-name table, in the style of
    /// `tests/enum_tables.rs`.
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

    /// The numbers come from `Protobufs/steammessages.proto` alone: `EGCPlatform` is a
    /// Game Coordinator enum and does not appear in the client's runtime schema, so there
    /// is nothing to read them back off.
    #[test]
    fn platform_names_every_value_the_gc_proto_publishes() {
        check!(
            Platform,
            name,
            0u32..8,
            &[
                (0, "None"),
                (1, "PC"),
                (2, "Mac"),
                (3, "Linux"),
                (4, "Android"),
                (5, "iOS"),
                (6, "Unknown"),
                (7, "Unknown"),
            ]
        );
    }

    /// The gap between `Team1` and `Spectator` is real: `ECitadelLobbyTeam` jumps from 1
    /// to 16 and defines nothing in between, so every value in the gap is unknown.
    #[test]
    fn lobby_team_names_every_value_it_knows_and_leaves_the_gap_unknown() {
        check!(
            LobbyTeam,
            name,
            0u32..19,
            &[
                (0, "Team0"),
                (1, "Team1"),
                (2, "Unknown"),
                (3, "Unknown"),
                (4, "Unknown"),
                (5, "Unknown"),
                (6, "Unknown"),
                (7, "Unknown"),
                (8, "Unknown"),
                (9, "Unknown"),
                (10, "Unknown"),
                (11, "Unknown"),
                (12, "Unknown"),
                (13, "Unknown"),
                (14, "Unknown"),
                (15, "Unknown"),
                (16, "Spectator"),
                (17, "Unknown"),
                (18, "Unknown"),
            ]
        );
    }

    /// Two "team number as a u32" namespaces sit one crate apart and disagree on every
    /// value they share. Reading a lobby team as a [`Team`] silently mislabels a side.
    #[test]
    fn lobby_team_numbering_disagrees_with_the_in_match_team_numbering() {
        assert_eq!(LobbyTeam::from_raw(1), LobbyTeam::Team1);
        assert_eq!(Team(1), Team::SPECTATOR);

        assert_eq!(LobbyTeam::from_raw(16), LobbyTeam::Spectator);
        assert_eq!(Team(16).default_name(), "Unknown");

        assert_eq!(LobbyTeam::from_raw(2), LobbyTeam::Unknown(2));
        assert_eq!(Team(2), Team::AMBER);
        assert_eq!(LobbyTeam::from_raw(3), LobbyTeam::Unknown(3));
        assert_eq!(Team(3), Team::SAPPHIRE);
    }

    #[test]
    fn matches_the_games_schema_values() {
        assert_eq!(
            ConnectionState::from_raw(-1),
            ConnectionState::NeverConnected
        );
        assert_eq!(ConnectionState::from_raw(0), ConnectionState::Connected);
        assert_eq!(ConnectionState::from_raw(5), ConnectionState::Reserved);
        assert_eq!(ConnectionState::from_raw(9), ConnectionState::Unknown(9));
    }

    #[test]
    fn reconnecting_still_counts_as_present() {
        assert!(ConnectionState::Reconnecting.is_in_match());
        assert!(!ConnectionState::Reconnecting.is_gone());
        assert!(ConnectionState::Disconnected.is_gone());
        assert!(!ConnectionState::Reserved.is_in_match());
    }

    #[test]
    fn unknown_enum_values_are_kept_rather_than_collapsed() {
        assert_eq!(RegionMode::from_raw(1), RegionMode::Europe);
        assert_eq!(RegionMode::from_raw(99), RegionMode::Unknown(99));
        assert_eq!(MmPreference::from_raw(1), MmPreference::Casual);
        assert_eq!(ChatMode::from_raw(2), ChatMode::Team);
        assert_eq!(BotDifficulty::from_raw(0), BotDifficulty::None);
        assert_eq!(RankedType::from_raw(1), RankedType::Normal);
        assert_eq!(PlayerType::from_raw(0), PlayerType::Player);
        assert_eq!(PlayerType::from_raw(7), PlayerType::Unknown(7));
    }
}
