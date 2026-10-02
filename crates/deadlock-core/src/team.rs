//! Teams.
//!
//! Values observed in a live match; the game also names them itself via
//! `C_Team::m_szTeamname`, which `deadlock-memory` reads and prefers.

/// A team number as the game uses it.
///
/// Note that a spectating client's own controller sits on [`Team::SPECTATOR`], which is
/// why a raw player list is 13 entries in a 12-player match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Team(pub u32);

impl Team {
    /// Entities not assigned to a side.
    pub const UNASSIGNED: Team = Team(0);
    /// Spectators and observers.
    pub const SPECTATOR: Team = Team(1);
    /// Amber Hand.
    pub const AMBER: Team = Team(2);
    /// Sapphire Flame.
    pub const SAPPHIRE: Team = Team(3);
    /// Neutral camps and jungle creeps.
    pub const NEUTRAL: Team = Team(4);

    /// The raw number.
    pub fn get(&self) -> u32 {
        self.0
    }

    /// Whether this is one of the two playing sides.
    pub fn is_playing(&self) -> bool {
        *self == Team::AMBER || *self == Team::SAPPHIRE
    }

    /// Whether this is the spectator team.
    pub fn is_spectator(&self) -> bool {
        *self == Team::SPECTATOR
    }

    /// The opposing side, for a playing team.
    pub fn opponent(&self) -> Option<Team> {
        match *self {
            Team::AMBER => Some(Team::SAPPHIRE),
            Team::SAPPHIRE => Some(Team::AMBER),
            _ => None,
        }
    }

    /// Built-in name, used when the game's own `m_szTeamname` is unavailable.
    ///
    /// Spelled as the game's `Citadel_TeamName_*` localisation strings spell them. The
    /// mapping is not one-to-one: the game also has a `Citadel_TeamName_Invalid`
    /// ("Invalid") separate from `Citadel_TeamName_Unassigned`, but no published enum
    /// gives it a number, so there is no constant to hang it on and such a value falls
    /// through to "Unknown".
    pub fn default_name(&self) -> &'static str {
        match *self {
            Team::UNASSIGNED => "Unassigned",
            Team::SPECTATOR => "Spectators",
            Team::AMBER => "Amber",
            Team::SAPPHIRE => "Sapphire",
            Team::NEUTRAL => "Neutrals",
            _ => "Unknown",
        }
    }

    /// The two playing sides.
    pub fn playing() -> [Team; 2] {
        [Team::AMBER, Team::SAPPHIRE]
    }
}

impl From<u32> for Team {
    fn from(v: u32) -> Self {
        Team(v)
    }
}

impl From<Team> for u32 {
    fn from(v: Team) -> Self {
        v.0
    }
}

impl std::fmt::Display for Team {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.default_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playing_sides() {
        assert!(Team::AMBER.is_playing() && Team::SAPPHIRE.is_playing());
        assert!(!Team::SPECTATOR.is_playing());
        assert!(!Team::NEUTRAL.is_playing());
        assert!(!Team::UNASSIGNED.is_playing());
        assert!(Team::SPECTATOR.is_spectator());
    }

    #[test]
    fn opponents() {
        assert_eq!(Team::AMBER.opponent(), Some(Team::SAPPHIRE));
        assert_eq!(Team::SAPPHIRE.opponent(), Some(Team::AMBER));
        assert_eq!(Team::NEUTRAL.opponent(), None);
    }

    /// Non-playing sides are spelled the way the game's own localisation spells them:
    /// `Citadel_TeamName_Invalid`, `Citadel_TeamName_Unassigned`,
    /// `Citadel_TeamName_Spectator` and `Citadel_TeamName_Neutral`.
    #[test]
    fn default_names_are_spelled_the_way_the_game_spells_them() {
        assert_eq!(Team::UNASSIGNED.default_name(), "Unassigned");
        assert_eq!(Team::SPECTATOR.default_name(), "Spectators");
        assert_eq!(Team::AMBER.default_name(), "Amber");
        assert_eq!(Team::SAPPHIRE.default_name(), "Sapphire");
        assert_eq!(Team::NEUTRAL.default_name(), "Neutrals");
        assert_eq!(Team(99).default_name(), "Unknown");
        assert_eq!(Team::AMBER.to_string(), "Amber");
        assert_eq!(Team::NEUTRAL.to_string(), "Neutrals");
    }
}
