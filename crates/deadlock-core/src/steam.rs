//! Steam id conversions.

/// Steam's individual-universe offset; add to a 32-bit account id for a Steam64 id.
pub const STEAM64_BASE: u64 = 76_561_197_960_265_728;

/// A 32-bit Steam account id, as Deadlock and the GC use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct AccountId(pub u32);

/// A 64-bit Steam id, as the Steam API and `m_steamID` use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct SteamId(pub u64);

impl AccountId {
    /// The raw id.
    pub fn get(&self) -> u32 {
        self.0
    }

    /// Widen to a Steam64 id.
    pub fn to_steam64(&self) -> SteamId {
        SteamId(STEAM64_BASE + self.0 as u64)
    }
}

impl SteamId {
    /// The raw id.
    pub fn get(&self) -> u64 {
        self.0
    }

    /// Narrow to a 32-bit account id.
    ///
    /// `None` for ids below the individual-universe base, which are not player ids.
    pub fn to_account_id(&self) -> Option<AccountId> {
        self.0
            .checked_sub(STEAM64_BASE)
            .and_then(|v| u32::try_from(v).ok())
            .map(AccountId)
    }
}

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::fmt::Display for SteamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let a = AccountId(387_246_372);
        assert_eq!(a.to_steam64(), SteamId(76_561_198_347_512_100));
        assert_eq!(a.to_steam64().to_account_id(), Some(a));
    }

    #[test]
    fn ids_below_the_base_are_not_players() {
        assert_eq!(SteamId(0).to_account_id(), None);
        assert_eq!(SteamId(STEAM64_BASE - 1).to_account_id(), None);
        assert_eq!(SteamId(STEAM64_BASE).to_account_id(), Some(AccountId(0)));
    }

    #[test]
    fn overlarge_ids_do_not_wrap() {
        assert_eq!(SteamId(u64::MAX).to_account_id(), None);
    }
}
