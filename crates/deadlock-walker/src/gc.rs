//! A session over the Game Coordinator objects resident in a running client.
//!
//! The client keeps the Game Coordinator's shared objects (party, account, hideout, ...) and
//! some replies (account statistics, hero builds, post-game progress) as live C++ protobuf
//! objects. [`GcSession`] finds them by RTTI vtable, reads them with the layouts derived from
//! the client's own tables and returns `valveprotos` messages.
//!
//! # What a poll costs
//!
//! A heap search reads every writable region, about a second on a live client. Polling
//! cannot afford that, so each kind of object is found in four steps, cheapest first:
//!
//! | Step | Cost | When |
//! |---|---|---|
//! | re-read pinned objects | microseconds | every call |
//! | probe remembered addresses | microseconds | after a pin died |
//! | search the regions those addresses sit in | tens of ms | after a pin died, rate limited |
//! | search the regions other kinds sit in | tens of ms | kind never seen, rate limited |
//! | search the whole heap | about a second | no object known, rate limited |
//!
//! The client allocates its Game Coordinator objects from one heap, so a party that has
//! just appeared is usually in a region that already holds the account object. The
//! neighbour step finds it there within [`DEFAULT_NEIGHBOUR_INTERVAL`], where only the
//! whole-heap step would find it, once every [`DEFAULT_SWEEP_INTERVAL`]. A whole-heap sweep
//! looks for every kind that has nothing pinned at once, since the read dominates its cost.
//!
//! The heap search sees only what [`MemoryReader::regions`] lists. A backend that drops
//! very large regions hides every object that lives in one, and the kinds that live there
//! read as absent; see the live checks in the crate README.
//!
//! A pin is only an address. Memory is freed and reused, so every re-read checks the vtable
//! before and after the walk and drops the pin when the object is gone or does not walk.
//!
//! # Which objects count
//!
//! A vtable hit is a live C++ object of that class, but not necessarily the one wanted: the
//! client holds copies, caches and other accounts' objects. Each kind therefore also has to
//! name the local account (a party lists it as a member, a build is authored by it, ...).
//! Copies that satisfy that are all returned; objects that fail to walk are skipped.
//!
//! Match metadata is the exception. The client holds one object per match the player has
//! opened, none of them tied to an account, so a copy counts when it is *complete*: a
//! non-zero match id and at least one player. Every complete copy is pinned, and
//! [`GcSession::match_metadata`] picks among them by match id on each call. A pinned copy
//! that was freed or reused fails the vtable check or the completeness check and is dropped.

use std::time::{Duration, Instant};

use deadlock_memory::mem::{MemoryReader, Region};
use prost::Message;
use valveprotos::deadlock::{
    CMsgAccountStats, CMsgHeroBuild, CMsgMatchMetaDataContents, CMsgPostGameProgressData,
    CsoAccountHeroInfo, CsoCitadelHideoutLobby, CsoCitadelLobby, CsoCitadelParty,
    CsoGameAccountClient,
};

use crate::error::{Error, Result};
use crate::ext::PartyExt;
use crate::pe::PeImage;
use crate::rtti::VtableResolver;
use crate::schema::Schema;
use crate::search::{SearchConfig, find_in, find_many};
use crate::tables::{ClientTables, client_schema};
use crate::walk::{Limits, Walker};

/// How often a whole-heap search is allowed by default, per [`Kind`].
///
/// A ceiling on how often that price is paid, not a polling interval; pinned re-reads
/// happen on every call regardless.
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(10);

/// How soon the cheap re-find of a region may be retried.
pub const DEFAULT_REFIND_INTERVAL: Duration = Duration::from_millis(200);

/// Sweep gate for a while after an object was last alive.
///
/// A pin that died means the object was replaced a moment ago, so a search is chasing
/// something that probably exists. Once [`LOST_GRACE`] has passed the relaxed interval
/// takes over.
pub const LOST_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

/// How long the faster post-loss sweep cadence lasts.
pub const LOST_GRACE: Duration = Duration::from_secs(30);

/// How often the regions around other kinds of object may be searched for a missing one.
///
/// Looks at a few regions instead of the whole heap, so it can run far more often than a
/// sweep and is what notices an object that has just appeared.
pub const DEFAULT_NEIGHBOUR_INTERVAL: Duration = Duration::from_secs(1);

/// How many past locations are remembered per kind.
pub const MAX_HINTS: usize = 8;

const CLIENT_MODULE: &str = "client.dll";

/// More than any one object needs: a hero build holds a few hundred mods.
const LIMITS: Limits = Limits {
    max_depth: 16,
    max_elements: 1 << 16,
    max_output: 4 * 1024 * 1024,
};

/// Which Game Coordinator object a pin or hint refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `CSOCitadelParty`.
    Party = 0,
    /// `CSOCitadelLobby`.
    Lobby = 1,
    /// `CSOGameAccountClient`.
    GameAccount = 2,
    /// `CMsgAccountStats`.
    AccountStats = 3,
    /// `CSOAccountHeroInfo`.
    AccountHeroes = 4,
    /// `CSOCitadelHideoutLobby`.
    Hideout = 5,
    /// `CMsgHeroBuild`.
    HeroBuilds = 6,
    /// `CMsgPostGameProgressData`.
    PostGameProgress = 7,
    /// `CMsgMatchMetaDataContents`.
    MatchMetaData = 8,
}

impl Kind {
    /// Every kind.
    pub const ALL: [Kind; 9] = [
        Kind::Party,
        Kind::Lobby,
        Kind::GameAccount,
        Kind::AccountStats,
        Kind::AccountHeroes,
        Kind::Hideout,
        Kind::HeroBuilds,
        Kind::PostGameProgress,
        Kind::MatchMetaData,
    ];

    /// The protobuf message this kind is.
    pub fn message(self) -> &'static str {
        match self {
            Kind::Party => "CSOCitadelParty",
            Kind::Lobby => "CSOCitadelLobby",
            Kind::GameAccount => "CSOGameAccountClient",
            Kind::AccountStats => "CMsgAccountStats",
            Kind::AccountHeroes => "CSOAccountHeroInfo",
            Kind::Hideout => "CSOCitadelHideoutLobby",
            Kind::HeroBuilds => "CMsgHeroBuild",
            Kind::PostGameProgress => "CMsgPostGameProgressData",
            Kind::MatchMetaData => "CMsgMatchMetaDataContents",
        }
    }
}

/// What a sweep found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Regions searched.
    pub regions: usize,
    /// Heap objects of the wanted classes found.
    pub hits: usize,
    /// Of those, the ones that walked cleanly and belong to the local account.
    pub pinned: usize,
}

/// A message type the session reads, and what makes an instance the local account's.
trait Object: Message + Default {
    const KIND: Kind;

    fn ours(&self, account: u32) -> bool;
}

impl Object for CsoCitadelParty {
    const KIND: Kind = Kind::Party;

    /// The anchor: a start offset, or a stale copy, that dropped the local member is not
    /// this player's party. A zero party id is a half-initialised object.
    fn ours(&self, account: u32) -> bool {
        self.party_id.is_some_and(|id| id != 0) && self.contains(account)
    }
}

impl Object for CsoCitadelLobby {
    const KIND: Kind = Kind::Lobby;

    /// The lobby names no account, so any initialised one is taken.
    fn ours(&self, _account: u32) -> bool {
        self.lobby_id.is_some_and(|id| id != 0)
    }
}

impl Object for CsoGameAccountClient {
    const KIND: Kind = Kind::GameAccount;

    fn ours(&self, account: u32) -> bool {
        self.account_id == Some(account)
    }
}

impl Object for CMsgAccountStats {
    const KIND: Kind = Kind::AccountStats;

    fn ours(&self, account: u32) -> bool {
        self.account_id == Some(account)
    }
}

impl Object for CsoAccountHeroInfo {
    const KIND: Kind = Kind::AccountHeroes;

    fn ours(&self, account: u32) -> bool {
        self.account_id == Some(account)
    }
}

impl Object for CsoCitadelHideoutLobby {
    const KIND: Kind = Kind::Hideout;

    fn ours(&self, account: u32) -> bool {
        self.members.iter().any(|m| m.account_id == Some(account))
    }
}

impl Object for CMsgHeroBuild {
    const KIND: Kind = Kind::HeroBuilds;

    fn ours(&self, account: u32) -> bool {
        self.author_account_id == Some(account)
    }
}

impl Object for CMsgPostGameProgressData {
    const KIND: Kind = Kind::PostGameProgress;

    fn ours(&self, account: u32) -> bool {
        let is_me = |p: &valveprotos::deadlock::c_msg_post_game_progress_data::PlayerData| {
            p.account_id == Some(account)
        };
        self.local_player.as_ref().is_some_and(is_me) || self.all_players.iter().any(is_me)
    }
}

impl Object for CMsgMatchMetaDataContents {
    const KIND: Kind = Kind::MatchMetaData;

    /// Match metadata is anchored on the match, not on an account: the client holds the
    /// matches the player has opened, and any of them is wanted. Only completeness is
    /// checked here; the match is chosen by the caller.
    fn ours(&self, _account: u32) -> bool {
        match_id_of(self).is_some()
    }
}

/// The id of a complete match metadata object.
///
/// Complete means the id is set and non-zero and at least one player has been written. The
/// client builds the object field by field, so a copy read mid-construction is missing its
/// players, and a recycled block can carry an id of zero. A copy that passes can still be
/// missing later fields; [`GcSession::match_metadata`] prefers the fullest copy for that
/// reason.
fn match_id_of(m: &CMsgMatchMetaDataContents) -> Option<u64> {
    let info = m.match_info.as_ref()?;
    info.match_id
        .filter(|&id| id != 0 && !info.players.is_empty())
}

/// Whether a read wants one object or every one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Extent {
    First,
    All,
}

/// Which step of the search answered a read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tier {
    Pinned,
    Found,
    None,
}

#[derive(Debug, Default)]
struct KindState {
    vtable: Option<u64>,
    /// Addresses last seen holding a valid object of this kind.
    pins: Vec<u64>,
    /// Addresses this kind has been found at, most recent first.
    hot: Vec<u64>,
    swept_at: Option<Instant>,
    lost_at: Option<Instant>,
    refound_at: Option<Instant>,
    neighboured_at: Option<Instant>,
}

/// Finds Game Coordinator objects and keeps their addresses between polls.
///
/// Built once per attached client: the construction parses `client.dll` and its compiled
/// protobuf tables, which is the expensive part. Every call after takes the reader, so the
/// session can outlive a borrow of it. Owned by the caller so two pollers do not share pins.
#[derive(Debug)]
pub struct GcSession {
    account_id: u32,
    pid: u32,
    schema: Schema,
    tables: ClientTables,
    search: SearchConfig,
    kinds: [KindState; Kind::ALL.len()],
    /// Addresses in the order they were first seen holding an object, newest last.
    ///
    /// Two generations of one party are often impossible to order from their contents. A
    /// generation that appeared while we were watching is newer than one that was already
    /// there.
    first_seen: Vec<(u64, u64)>,
    seq: u64,
    sweep_interval: Duration,
    refind_interval: Duration,
    neighbour_interval: Duration,
    sweeps: u64,
    reuses: u64,
}

impl GcSession {
    /// Build a session over the client in `mem`, anchored on the local Steam account id.
    ///
    /// The id has to be the account whose objects are wanted: it is how a party, account or
    /// build is told from another player's. Get it from
    /// `deadlock_reader::steam::active_account_id`.
    ///
    /// The session belongs to that process: vtable addresses and pins are meaningless in a
    /// restarted game, and every read against another process fails with
    /// [`Error::WrongProcess`], on which the caller builds a new session.
    ///
    /// # Errors
    ///
    /// The client module cannot be read, or carries no compiled protobuf tables
    /// ([`Error::NoTables`]). A class the client lacks does not fail construction; reading
    /// that kind returns [`Error::ClassNotFound`].
    pub fn new(mem: &dyn MemoryReader, account_id: u32) -> Result<Self> {
        let client = mem.module(CLIENT_MODULE)?;
        let image = PeImage::read(mem, &client)?;
        let schema = client_schema(&image)?;
        let tables = ClientTables::read(&image, &schema)?;
        let resolver = VtableResolver::new(&image);
        let mut kinds: [KindState; Kind::ALL.len()] = Default::default();
        for kind in Kind::ALL {
            kinds[kind as usize].vtable = resolver.vtable(kind.message()).ok();
        }
        Ok(GcSession {
            account_id,
            pid: mem.pid(),
            schema,
            tables,
            search: SearchConfig::default().excluding_module(&client),
            kinds,
            first_seen: Vec::new(),
            seq: 0,
            sweep_interval: DEFAULT_SWEEP_INTERVAL,
            refind_interval: DEFAULT_REFIND_INTERVAL,
            neighbour_interval: DEFAULT_NEIGHBOUR_INTERVAL,
            sweeps: 0,
            reuses: 0,
        })
    }

    /// Set how often a whole-heap sweep may run per kind.
    #[must_use]
    pub fn every(mut self, interval: Duration) -> Self {
        self.sweep_interval = interval;
        self
    }

    /// Set how often the cheap region re-find may be retried per kind. This is what
    /// bounds how fast a replaced object is noticed.
    #[must_use]
    pub fn refind_every(mut self, interval: Duration) -> Self {
        self.refind_interval = interval;
        self
    }

    /// Set how often the regions around other kinds of object may be searched for a kind
    /// that has no known location. Pass [`Duration::ZERO`] to search on every miss.
    #[must_use]
    pub fn neighbours_every(mut self, interval: Duration) -> Self {
        self.neighbour_interval = interval;
        self
    }

    /// The account id being anchored on.
    pub fn account_id(&self) -> u32 {
        self.account_id
    }

    /// Whole-heap sweeps run and reads served straight from a live pin.
    pub fn stats(&self) -> (u64, u64) {
        (self.sweeps, self.reuses)
    }

    /// Addresses currently pinned for `kind`.
    pub fn pins(&self, kind: Kind) -> &[u64] {
        &self.kinds[kind as usize].pins
    }

    /// Where `kind` has been found, most recent first.
    pub fn hints(&self, kind: Kind) -> &[u64] {
        &self.kinds[kind as usize].hot
    }

    /// Drop every pin and hint, forcing the next call to search.
    pub fn invalidate(&mut self) {
        for state in &mut self.kinds {
            *state = KindState {
                vtable: state.vtable,
                ..KindState::default()
            };
        }
        self.first_seen.clear();
    }

    /// Search the whole heap once for every kind and pin what belongs to the local account.
    ///
    /// Expensive; called automatically when nothing is pinned and the interval allows, so
    /// most callers never need it. Useful to pay the cost up front and to report it.
    ///
    /// # Errors
    ///
    /// The target's regions cannot be listed.
    pub fn sweep(&mut self, mem: &dyn MemoryReader) -> Result<SweepReport> {
        self.check_process(mem)?;
        self.sweep_kinds(mem, &Kind::ALL)
    }

    /// The current party, re-reading a pin when one is live.
    ///
    /// `Ok(None)` is the normal answer for someone playing solo. Where several generations
    /// of the party are resident, this is the most recent; see
    /// [`parties`](Self::parties).
    ///
    /// # Errors
    ///
    /// The class is unknown to the client, or the target's regions cannot be listed.
    pub fn party(&mut self, mem: &dyn MemoryReader) -> Result<Option<CsoCitadelParty>> {
        Ok(self.parties(mem)?.into_iter().next())
    }

    /// Every resident party object naming the local account, most recent first.
    ///
    /// The client keeps more than one: a party is written afresh when it changes, and the
    /// previous object lives until its memory is reused. Nothing in the message dates it,
    /// so they are ordered by what the contents prove (an accepted invite, a recorded
    /// departure), then by which turned up first while this session was watching, then by
    /// roster size.
    ///
    /// # Errors
    ///
    /// As [`party`](Self::party).
    pub fn parties(&mut self, mem: &dyn MemoryReader) -> Result<Vec<CsoCitadelParty>> {
        let found = self.read::<CsoCitadelParty>(mem, Extent::All, None)?;
        let mut ranked: Vec<(CsoCitadelParty, u64)> = found
            .into_iter()
            .map(|(addr, p)| (p, self.seen_at(addr)))
            .collect();
        rank_parties(&mut ranked);
        Ok(ranked.into_iter().map(|(p, _)| p).collect())
    }

    /// The current lobby.
    ///
    /// # Errors
    ///
    /// As [`party`](Self::party).
    pub fn lobby(&mut self, mem: &dyn MemoryReader) -> Result<Option<CsoCitadelLobby>> {
        self.read_one(mem)
    }

    /// The local account's `CSOGameAccountClient`: rank, ban expiries, priority tokens.
    ///
    /// # Errors
    ///
    /// As [`party`](Self::party).
    pub fn game_account(&mut self, mem: &dyn MemoryReader) -> Result<Option<CsoGameAccountClient>> {
        self.read_one(mem)
    }

    /// The local account's lifetime statistics broken down by hero.
    ///
    /// A reply rather than a shared object, so it is resident only while the client holds
    /// on to it.
    ///
    /// # Errors
    ///
    /// As [`party`](Self::party).
    pub fn account_stats(&mut self, mem: &dyn MemoryReader) -> Result<Option<CMsgAccountStats>> {
        self.read_one(mem)
    }

    /// The `CMsgPostGameProgressData` the client holds for the match that just ended.
    ///
    /// # Errors
    ///
    /// As [`party`](Self::party).
    pub fn post_game_progress(
        &mut self,
        mem: &dyn MemoryReader,
    ) -> Result<Option<CMsgPostGameProgressData>> {
        self.read_one(mem)
    }

    /// Every `CSOAccountHeroInfo` of the local account, one per hero, ordered by address.
    ///
    /// # Errors
    ///
    /// As [`party`](Self::party).
    pub fn account_heroes(&mut self, mem: &dyn MemoryReader) -> Result<Vec<CsoAccountHeroInfo>> {
        self.read_list(mem)
    }

    /// The hideout the local account is in; `Ok(None)` whenever it is not.
    ///
    /// # Errors
    ///
    /// As [`party`](Self::party).
    pub fn hideout(&mut self, mem: &dyn MemoryReader) -> Result<Option<CsoCitadelHideoutLobby>> {
        self.read_one(mem)
    }

    /// Every `CMsgHeroBuild` the local account authored, ordered by address.
    ///
    /// # Errors
    ///
    /// As [`party`](Self::party).
    pub fn hero_builds(&mut self, mem: &dyn MemoryReader) -> Result<Vec<CMsgHeroBuild>> {
        self.read_list(mem)
    }

    /// The `CMsgMatchMetaDataContents` resident for `match_id`.
    ///
    /// `Ok(None)` when no complete copy of that match is resident, including when the object
    /// was freed or reused mid-walk; ask again later. Complete means a non-zero
    /// `match_info.match_id` and at least one entry in `match_info.players`; see
    /// [`match_metadata_all`](Self::match_metadata_all). The client may hold several copies
    /// of one match, and a copy can pass that check while still being built, so the one with
    /// the most data is returned, the lowest address on a tie. A match that is not yet
    /// pinned is searched for under the usual rate limits.
    ///
    /// # Errors
    ///
    /// As [`party`](Self::party).
    pub fn match_metadata(
        &mut self,
        mem: &dyn MemoryReader,
        match_id: u64,
    ) -> Result<Option<CMsgMatchMetaDataContents>> {
        let is_it = |m: &CMsgMatchMetaDataContents| match_id_of(m) == Some(match_id);
        let found = self.read::<CMsgMatchMetaDataContents>(mem, Extent::All, Some(&is_it))?;
        Ok(found
            .into_iter()
            .filter(|(_, m)| is_it(m))
            .min_by_key(|(addr, m)| (std::cmp::Reverse(m.encoded_len()), *addr))
            .map(|(_, m)| m))
    }

    /// Every complete match metadata object resident, ordered by address.
    ///
    /// Anchored on the match, not the local account: the client keeps the metadata of each
    /// match the player has opened, other players' included. Objects that fail to walk or are
    /// incomplete are skipped.
    ///
    /// # Errors
    ///
    /// As [`party`](Self::party).
    pub fn match_metadata_all(
        &mut self,
        mem: &dyn MemoryReader,
    ) -> Result<Vec<CMsgMatchMetaDataContents>> {
        self.read_list(mem)
    }

    fn read_one<M: Object>(&mut self, mem: &dyn MemoryReader) -> Result<Option<M>> {
        Ok(self
            .read::<M>(mem, Extent::First, None)?
            .into_iter()
            .next()
            .map(|(_, m)| m))
    }

    fn read_list<M: Object>(&mut self, mem: &dyn MemoryReader) -> Result<Vec<M>> {
        let mut found = self.read::<M>(mem, Extent::All, None)?;
        found.sort_by_key(|(addr, _)| *addr);
        Ok(found.into_iter().map(|(_, m)| m).collect())
    }

    /// Walk the search steps until `extent` is satisfied.
    ///
    /// When some pins survive but others died, a replacement may exist for the dead ones, so
    /// a read of every object keeps looking and merges what it finds.
    fn read<M: Object>(
        &mut self,
        mem: &dyn MemoryReader,
        extent: Extent,
        wanted: Option<&dyn Fn(&M) -> bool>,
    ) -> Result<Vec<(u64, M)>> {
        self.check_process(mem)?;
        let kind = M::KIND;
        let vtable = self.vtable(kind)?;

        let pinned_before = self.pins(kind).len();
        let mut found = self.reread::<M>(mem, vtable, extent);
        let intact = found.len() == pinned_before;
        let has_wanted =
            |found: &[(u64, M)]| wanted.is_none_or(|want| found.iter().any(|(_, m)| want(m)));
        if !found.is_empty() && (extent == Extent::First || intact) && has_wanted(&found) {
            self.settle(kind, Tier::Pinned);
            return Ok(found);
        }

        if (found.is_empty() || !intact)
            && self.state(kind).lost_at.is_none()
            && !self.state(kind).hot.is_empty()
        {
            self.state_mut(kind).lost_at = Some(Instant::now());
        }

        let mut tier = if found.is_empty() {
            Tier::None
        } else {
            Tier::Pinned
        };
        let survivors: Vec<u64> = found.iter().map(|(a, _)| *a).collect();
        let satisfied = |found: &[(u64, M)]| match (wanted, extent) {
            (Some(_), _) => has_wanted(found),
            (None, Extent::First) => !found.is_empty(),
            (None, Extent::All) => found.iter().any(|(a, _)| !survivors.contains(a)),
        };

        // The addresses this kind has occupied before: the allocator often hands a freed
        // block straight back, and a replacement tends to land where the object has lived.
        let hot = self.state(kind).hot.clone();
        self.add_pins(kind, &hot);
        found = self.reread::<M>(mem, vtable, extent);
        if satisfied(&found) {
            self.settle(kind, Tier::Found);
            return Ok(found);
        }

        if self.may_refind(kind) {
            self.refind(mem, kind, vtable);
            found = self.reread::<M>(mem, vtable, extent);
            if satisfied(&found) {
                self.settle(kind, Tier::Found);
                return Ok(found);
            }
        }

        if self.may_neighbour(kind) {
            self.search_neighbours(mem, kind, vtable);
            found = self.reread::<M>(mem, vtable, extent);
            if satisfied(&found) {
                self.settle(kind, Tier::Found);
                return Ok(found);
            }
        }

        if self.may_sweep(kind) {
            let kinds = self.sweepable_with(kind);
            self.sweep_kinds(mem, &kinds)?;
            found = self.reread::<M>(mem, vtable, extent);
            if !found.is_empty() {
                tier = Tier::Found;
            }
        }
        self.settle(kind, if found.is_empty() { Tier::None } else { tier });
        Ok(found)
    }

    fn settle(&mut self, kind: Kind, tier: Tier) {
        if tier != Tier::None {
            self.state_mut(kind).lost_at = None;
        }
        if tier == Tier::Pinned {
            self.reuses += 1;
        }
    }

    fn state(&self, kind: Kind) -> &KindState {
        &self.kinds[kind as usize]
    }

    fn state_mut(&mut self, kind: Kind) -> &mut KindState {
        &mut self.kinds[kind as usize]
    }

    fn check_process(&self, mem: &dyn MemoryReader) -> Result<()> {
        if mem.pid() == self.pid {
            Ok(())
        } else {
            Err(Error::WrongProcess)
        }
    }

    fn vtable(&self, kind: Kind) -> Result<u64> {
        let vtable = self
            .state(kind)
            .vtable
            .ok_or_else(|| Error::ClassNotFound(kind.message().to_string()))?;
        if crate::layout::LayoutSource::layout(&self.tables, kind.message()).is_none() {
            return Err(Error::NoLayout(kind.message().to_string()));
        }
        Ok(vtable)
    }

    /// Read and validate the object at `addr`: it must still carry the vtable, walk cleanly
    /// and belong to the local account.
    fn read_at<M: Object>(&self, mem: &dyn MemoryReader, vtable: u64, addr: u64) -> Option<M> {
        let wire = Walker::new(mem, &self.schema, &self.tables)
            .with_limits(LIMITS)
            .serialize_checked(M::KIND.message(), addr, vtable)
            .ok()?;
        M::decode(&wire[..])
            .ok()
            .filter(|m| m.ours(self.account_id))
    }

    /// Re-read the pins of `M`'s kind, dropping the ones that no longer validate.
    fn reread<M: Object>(
        &mut self,
        mem: &dyn MemoryReader,
        vtable: u64,
        extent: Extent,
    ) -> Vec<(u64, M)> {
        let kind = M::KIND;
        let mut found = Vec::new();
        let mut live = Vec::new();
        let pins = std::mem::take(&mut self.state_mut(kind).pins);
        for (i, &addr) in pins.iter().enumerate() {
            if extent == Extent::First && !found.is_empty() {
                // Pins after the first answer were not shown dead, only left unread.
                live.extend_from_slice(&pins[i..]);
                break;
            }
            if let Some(m) = self.read_at::<M>(mem, vtable, addr) {
                live.push(addr);
                found.push((addr, m));
            }
        }
        for &(addr, _) in &found {
            self.note_seen(addr);
        }
        for &addr in found.iter().map(|(a, _)| a).rev() {
            self.remember(kind, addr);
        }
        self.state_mut(kind).pins = live;
        found
    }

    fn add_pins(&mut self, kind: Kind, addrs: &[u64]) {
        let pins = &mut self.state_mut(kind).pins;
        for &a in addrs {
            if !pins.contains(&a) {
                pins.push(a);
            }
        }
    }

    /// Search the regions this kind was last found in.
    fn refind(&mut self, mem: &dyn MemoryReader, kind: Kind, vtable: u64) {
        self.state_mut(kind).refound_at = Some(Instant::now());
        let mut regions: Vec<Region> = Vec::new();
        for &addr in &self.state(kind).hot {
            if let Some(r) = mem.region_at(addr)
                && !regions.iter().any(|x| x.base == r.base)
            {
                regions.push(r);
            }
        }
        if regions.is_empty() {
            return;
        }
        let hits = find_in(mem, &regions, &[vtable], &self.search);
        self.add_pins(kind, &hits[0]);
    }

    /// Search the regions where other kinds of object live.
    ///
    /// The client allocates its Game Coordinator objects from the same heap, so one that
    /// has just appeared is usually beside the ones already known. A few regions cost tens
    /// of milliseconds where the whole heap costs about a second.
    fn search_neighbours(&mut self, mem: &dyn MemoryReader, kind: Kind, vtable: u64) {
        self.state_mut(kind).neighboured_at = Some(Instant::now());
        let mut regions: Vec<Region> = Vec::new();
        for other in Kind::ALL.into_iter().filter(|&k| k != kind) {
            for &addr in &self.state(other).hot {
                if let Some(r) = mem.region_at(addr)
                    && !regions.iter().any(|x| x.base == r.base)
                {
                    regions.push(r);
                }
            }
        }
        if regions.is_empty() {
            return;
        }
        let hits = find_in(mem, &regions, &[vtable], &self.search);
        self.add_pins(kind, &hits[0]);
    }

    /// `kind` plus every other kind that has nothing pinned and may be swept now.
    ///
    /// A sweep reads the whole heap whatever it looks for, and looking for more vtables
    /// adds little, so a cold start pays for one sweep instead of one per kind.
    fn sweepable_with(&self, kind: Kind) -> Vec<Kind> {
        Kind::ALL
            .into_iter()
            .filter(|&k| {
                k == kind
                    || (self.state(k).pins.is_empty()
                        && self.state(k).vtable.is_some()
                        && self.may_sweep(k))
            })
            .collect()
    }

    fn sweep_kinds(&mut self, mem: &dyn MemoryReader, kinds: &[Kind]) -> Result<SweepReport> {
        let wanted: Vec<(Kind, u64)> = kinds
            .iter()
            .filter_map(|&k| Some((k, self.state(k).vtable?)))
            .collect();
        let vtables: Vec<u64> = wanted.iter().map(|(_, v)| *v).collect();
        let sweep = find_many(mem, &vtables, &self.search)?;

        let mut report = SweepReport {
            regions: sweep.regions,
            ..SweepReport::default()
        };
        let now = Instant::now();
        for (&(kind, vtable), hits) in wanted.iter().zip(&sweep.hits) {
            report.hits += hits.len();
            let valid: Vec<u64> = hits
                .iter()
                .copied()
                .filter(|&addr| self.validates(mem, kind, vtable, addr))
                .collect();
            report.pinned += valid.len();
            for &addr in &valid {
                self.note_seen(addr);
            }
            for &addr in valid.iter().rev() {
                self.remember(kind, addr);
            }
            let state = self.state_mut(kind);
            state.pins = valid;
            state.swept_at = Some(now);
        }
        self.sweeps += 1;
        Ok(report)
    }

    fn validates(&self, mem: &dyn MemoryReader, kind: Kind, vtable: u64, addr: u64) -> bool {
        match kind {
            Kind::Party => self.read_at::<CsoCitadelParty>(mem, vtable, addr).is_some(),
            Kind::Lobby => self.read_at::<CsoCitadelLobby>(mem, vtable, addr).is_some(),
            Kind::GameAccount => self
                .read_at::<CsoGameAccountClient>(mem, vtable, addr)
                .is_some(),
            Kind::AccountStats => self
                .read_at::<CMsgAccountStats>(mem, vtable, addr)
                .is_some(),
            Kind::AccountHeroes => self
                .read_at::<CsoAccountHeroInfo>(mem, vtable, addr)
                .is_some(),
            Kind::Hideout => self
                .read_at::<CsoCitadelHideoutLobby>(mem, vtable, addr)
                .is_some(),
            Kind::HeroBuilds => self.read_at::<CMsgHeroBuild>(mem, vtable, addr).is_some(),
            Kind::PostGameProgress => self
                .read_at::<CMsgPostGameProgressData>(mem, vtable, addr)
                .is_some(),
            Kind::MatchMetaData => self
                .read_at::<CMsgMatchMetaDataContents>(mem, vtable, addr)
                .is_some(),
        }
    }

    fn remember(&mut self, kind: Kind, addr: u64) {
        let hot = &mut self.state_mut(kind).hot;
        hot.retain(|&a| a != addr);
        hot.insert(0, addr);
        hot.truncate(MAX_HINTS);
    }

    fn note_seen(&mut self, addr: u64) {
        if self.first_seen.iter().any(|(a, _)| *a == addr) {
            return;
        }
        self.seq += 1;
        self.first_seen.push((addr, self.seq));
        if self.first_seen.len() > 64 {
            self.first_seen.remove(0);
        }
    }

    fn seen_at(&self, addr: u64) -> u64 {
        self.first_seen
            .iter()
            .find(|(a, _)| *a == addr)
            .map_or(0, |(_, s)| *s)
    }

    fn may_sweep(&self, kind: Kind) -> bool {
        let recently_lost = self
            .state(kind)
            .lost_at
            .is_some_and(|t| t.elapsed() < LOST_GRACE);
        let interval = if recently_lost {
            self.sweep_interval.min(LOST_SWEEP_INTERVAL)
        } else {
            self.sweep_interval
        };
        self.state(kind)
            .swept_at
            .is_none_or(|t| t.elapsed() >= interval)
    }

    fn may_neighbour(&self, kind: Kind) -> bool {
        Kind::ALL
            .into_iter()
            .any(|k| k != kind && !self.state(k).hot.is_empty())
            && self
                .state(kind)
                .neighboured_at
                .is_none_or(|t| t.elapsed() >= self.neighbour_interval)
    }

    fn may_refind(&self, kind: Kind) -> bool {
        !self.state(kind).hot.is_empty()
            && self
                .state(kind)
                .refound_at
                .is_none_or(|t| t.elapsed() >= self.refind_interval)
    }
}

/// Order parties most recent first, given how recently each was first seen.
///
/// Three rules, strongest evidence first: what the contents prove (how many others each
/// supersedes, less how many supersede it); when it turned up, since a generation that
/// appeared while watching is newer than one already resident; and finally the larger
/// roster, which is a guess and so has the last word.
fn rank_parties(found: &mut Vec<(CsoCitadelParty, u64)>) {
    if found.len() < 2 {
        return;
    }
    let score = |i: usize| -> i32 {
        (0..found.len())
            .filter(|&j| j != i)
            .map(|j| {
                if found[i].0.supersedes(&found[j].0) {
                    1
                } else if found[j].0.supersedes(&found[i].0) {
                    -1
                } else {
                    0
                }
            })
            .sum()
    };
    let scores: Vec<i32> = (0..found.len()).map(score).collect();
    let mut order: Vec<usize> = (0..found.len()).collect();
    order.sort_by_key(|&i| {
        (
            -scores[i],
            std::cmp::Reverse(found[i].1),
            std::cmp::Reverse(found[i].0.members.len()),
        )
    });
    let mut slots: Vec<Option<(CsoCitadelParty, u64)>> = found.drain(..).map(Some).collect();
    found.extend(order.into_iter().filter_map(|i| slots[i].take()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use valveprotos::deadlock::cso_citadel_party::{Invite, LeftMember, Member};

    fn member(id: u32) -> Member {
        Member {
            account_id: Some(id),
            ..Default::default()
        }
    }

    fn party(id: u64, members: &[u32]) -> CsoCitadelParty {
        CsoCitadelParty {
            party_id: Some(id),
            members: members.iter().map(|&m| member(m)).collect(),
            ..Default::default()
        }
    }

    fn ids(ranked: &[(CsoCitadelParty, u64)]) -> Vec<(u64, usize)> {
        ranked
            .iter()
            .map(|(p, seen)| (*seen, p.members.len()))
            .collect()
    }

    #[test]
    fn proof_outranks_recency_and_roster_size() {
        let mut old = party(7, &[1, 2, 3]);
        old.invites = vec![Invite {
            account_id: Some(4),
            ..Default::default()
        }];
        let mut new = party(7, &[1, 2]);
        new.members.push(member(4));
        new.left_members = vec![LeftMember {
            account_id: Some(3),
            ..Default::default()
        }];
        let mut found = vec![(old.clone(), 9), (new.clone(), 1)];
        rank_parties(&mut found);
        assert_eq!(found[0].0, new);
        assert_eq!(found[1].0, old);
    }

    #[test]
    fn the_later_sighting_outranks_the_larger_roster_when_nothing_is_proved() {
        let mut found = vec![(party(7, &[1, 2, 3]), 1), (party(7, &[1, 2]), 2)];
        rank_parties(&mut found);
        assert_eq!(ids(&found), [(2, 2), (1, 3)]);
    }

    #[test]
    fn with_no_other_evidence_the_larger_roster_wins() {
        let mut found = vec![(party(7, &[1]), 0), (party(7, &[1, 2]), 0)];
        rank_parties(&mut found);
        assert_eq!(ids(&found), [(0, 2), (0, 1)]);
    }

    #[test]
    fn different_parties_are_ordered_by_sighting_not_by_content() {
        let mut found = vec![(party(6, &[1, 2, 3]), 1), (party(7, &[1]), 5)];
        rank_parties(&mut found);
        assert_eq!(found[0].0.party_id, Some(7));
    }
}
