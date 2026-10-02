//! Statlocker PP rating lookups, behind a facade small enough to keep honest.
//!
//! Statlocker's **PP** ("performance points") rating is its own ladder, invented before
//! Valve shipped ranked matchmaking, and it is what this module fetches. It is a
//! different measurement from the game's own badge, which the overlay already reads
//! straight out of memory as `m_unPackedRank` and does not need a network call for.
//!
//! # What is actually known about Statlocker's API
//!
//! **Statlocker publishes no documented public API.** Searched, on 2026-08-21:
//! `statlocker.gg/api` (serves the site's React shell, not documentation), the site root,
//! `api.statlocker.gg` (does not resolve), and web searches for Statlocker API docs, an
//! API key request process and any developer portal. Nothing documents an endpoint, a
//! response shape, an auth scheme or a rate limit.
//!
//! What *was* observed, by reading the site's own shipped bundle
//! (`https://statlocker.gg/static/js/main.0e2233e6.js`):
//!
//! - The site calls its API same-origin, at `window.location.origin` + `/api/...`.
//! - Profile objects carry `accountId`, `name`, `avatarUrl`, `ppScore` and
//!   `calibrationMatches`. The site gets one for a single account from
//!   `/api/profile/{accountId}`, and lists of the same shape from
//!   `/api/profile/favourites` and `/api/profile/search-profiles/{query}`.
//! - **`ppScore` is the only rating the server sends.** The rank number the site draws is
//!   computed in the browser from it - see [`pp_badge`] - so there is no rank field to
//!   ask for and no endpoint that would return one.
//! - `/api/profile/{accountId}/valve-rank` exists too, and is Statlocker's mirror of the
//!   game's own badge. Deliberately not used: `packed_rank` from memory is authoritative
//!   and free.
//!
//! Calling any of those unauthenticated returns `401 {"error": "Missing API key",
//! "message": "Provide a valid API key in the X-API-Key header"}`. No key, so **the
//! response body has never been seen**. Which of a profile's fields a *single*-profile
//! fetch actually returns is therefore inferred from the shapes the site's list views
//! consume; [`parse_pp`] says so, and refuses to guess when it finds nothing.
//!
//! Consequently this module does nothing at all unless an API key is supplied through
//! [`API_KEY_ENV`]. With no key every player resolves to [`PpLookup::Unavailable`], which
//! the window is expected to draw as "not available" rather than as "no rating".
//!
//! `api.deadlock-api.com` is a different service, already used by `deadlock-data`. It is
//! deliberately not involved here.
//!
//! # Threading
//!
//! The scoreboard poll loop runs at 250ms and must never wait on a socket. [`Statlocker`]
//! is therefore a cache with a worker thread behind it: [`Statlocker::pp`] only ever
//! reads the cache and queues, and results land on some later frame.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use deadlock_core::RankBadge;

use crate::model::{StatlockerPpState, StatlockerPpView};

/// Origin the Statlocker web app makes its own API calls against.
///
/// **Unverified as an API base.** Observed only as the origin the site's bundle builds
/// request URLs from; see the module docs for where that was read and what was searched.
pub const STATLOCKER_BASE: &str = "https://statlocker.gg";

/// Environment variable holding the `X-API-Key` value.
///
/// Absent by default, and absent means no request is ever made.
pub const API_KEY_ENV: &str = "DEADRS_STATLOCKER_API_KEY";

/// Smallest gap between two outbound requests.
///
/// Statlocker documents no rate limit, so this is a self-imposed one: a full lobby is
/// twelve lookups, which fills in about three seconds and then never repeats for the rest
/// of the match.
pub const REQUEST_GAP: Duration = Duration::from_millis(250);

/// How long a single request may take before it counts as failed.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// PP per subrank on Statlocker's ladder.
///
/// Read from the bundle's own conversion: the rank number is `floor(ppScore / 100)`
/// re-packed as a tier and subrank, capped at the 66th step.
pub const PP_PER_SUBRANK: f64 = 100.0;

/// How many matches Statlocker wants before it calls a PP rating calibrated.
///
/// Read from the bundle, where the badge component defaults `calibrationThreshold` to 50
/// and an uncalibrated profile draws "Play 50 games to calibrate" instead of a rank.
pub const CALIBRATION_MATCHES: u32 = 50;

/// Statlocker's own tier names, indexed by tier, for tiers 1 to 11.
///
/// Read verbatim from the bundle's PP tier table. **Not** the game's tier names, and not
/// interchangeable with them: Statlocker calls tiers 3, 4 and 7 `Alchemist`, `Arcanist`
/// and `Archon`, which are not names on Valve's ladder at all. Naming a PP tier with
/// [`deadlock_data::RankNames`] would therefore print the wrong word.
pub const PP_TIER_NAMES: [&str; 11] = [
    "Initiate",
    "Seeker",
    "Alchemist",
    "Arcanist",
    "Ritualist",
    "Emissary",
    "Archon",
    "Oracle",
    "Phantom",
    "Ascendant",
    "Eternus",
];

/// The URL a PP lookup would be made against.
///
/// Path shape observed in the site's bundle, never successfully called - see the module
/// docs. `account_id` is the 32-bit Steam account id, not a Steam64 id.
pub fn profile_url(account_id: u32) -> String {
    format!("{STATLOCKER_BASE}/api/profile/{account_id}")
}

/// Turn a PP score into the packed tier/subrank badge Statlocker draws for it.
///
/// Ported from the bundle's own conversion, which the site applies client-side because
/// the server only ever sends the score: six subranks per tier, eleven tiers, one subrank
/// per 100 PP, and everything above the 66th step pinned there. A score below 100 still
/// draws as the first step rather than as nothing.
///
/// Returns an unranked [`RankBadge`] for a score of zero or less, which is the bundle's
/// `null` - no rating rather than the bottom of the ladder.
pub fn pp_badge(score: f64) -> RankBadge {
    // Written as an ordering rather than `!(score > 0.0)` so a NaN score - which is
    // neither greater nor less - lands on unranked instead of on the ladder.
    if !matches!(score.partial_cmp(&0.0), Some(std::cmp::Ordering::Greater)) {
        return RankBadge(0);
    }
    // Below the first step the site still shows step one, so a new account reads as
    // Initiate 1 rather than as unrated.
    let step = if score < PP_PER_SUBRANK {
        1
    } else {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 1..=66 immediately below; the cast cannot overflow or go negative"
        )]
        let raw = (score / PP_PER_SUBRANK).floor().min(66.0) as u32;
        raw.clamp(1, 66)
    };
    let zero_based = step - 1;
    let tier = (zero_based / 6 + 1).clamp(1, 11);
    let subrank = (zero_based % 6 + 1).clamp(1, 6);
    RankBadge(tier * 10 + subrank)
}

/// Statlocker's name for a badge's tier, or `None` for a tier off its ladder.
pub fn pp_tier_name(badge: RankBadge) -> Option<&'static str> {
    let tier = usize::try_from(badge.tier()).ok()?;
    PP_TIER_NAMES.get(tier.checked_sub(1)?).copied()
}

/// A PP rating Statlocker holds for an account.
///
/// The score is the measurement; everything else is derived from it exactly the way the
/// site derives it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PpRating {
    /// Statlocker's performance-points score.
    pub score: f64,
    /// The packed tier/subrank badge that score draws as. See [`pp_badge`].
    pub badge: RankBadge,
    /// Matches played towards calibration, when the answer carried a count.
    ///
    /// Below [`CALIBRATION_MATCHES`] the site draws a placeholder instead of the badge,
    /// so a window that ignores this will show a confident rank for a rating Statlocker
    /// itself does not consider settled.
    pub calibration_matches: Option<u32>,
}

impl PpRating {
    /// Whether Statlocker considers this rating settled.
    ///
    /// A profile that reported no count at all is treated as calibrated: the absence is
    /// not evidence of a partial calibration, and inventing one would understate a real
    /// rating on every row.
    pub fn is_calibrated(&self) -> bool {
        self.calibration_matches
            .is_none_or(|played| played >= CALIBRATION_MATCHES)
    }
}

/// Where one account's PP lookup stands.
///
/// The three ways a lookup can fail to produce a rating are deliberately three variants
/// and not one `None`: a player nobody has ever tracked, a request that fell over, and a
/// lookup that simply has not answered yet are different facts and must not paint the
/// same.
#[derive(Clone, Debug, PartialEq)]
pub enum PpLookup {
    /// No lookup is possible - no API key, so nothing was or will be asked.
    Unavailable(String),
    /// Queued or in flight. An answer is expected on a later frame.
    Pending,
    /// Statlocker answered with a PP rating.
    Known(PpRating),
    /// Statlocker answered, and holds no PP rating for this account.
    Absent,
    /// The request failed. Carries whatever the transport could say about why.
    Failed(String),
}

/// What a single request came back with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fetched {
    /// A response body to parse.
    Body(String),
    /// The service said it has no such account, which is not an error.
    Absent,
}

/// The HTTP call, injectable so tests never touch the network.
pub trait Transport: Send + Sync + 'static {
    /// `GET url` with `X-API-Key: api_key`. `Err` is a short human-readable reason.
    fn get(&self, url: &str, api_key: &str) -> Result<Fetched, String>;
}

/// `ureq` against the real service.
///
/// Never reached without an API key, so in a default build this is dead weight rather
/// than traffic.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        UreqTransport {
            agent: ureq::AgentBuilder::new()
                .timeout(TIMEOUT)
                .user_agent(concat!("deadrs-overlay/", env!("CARGO_PKG_VERSION")))
                .build(),
        }
    }
}

impl Transport for UreqTransport {
    fn get(&self, url: &str, api_key: &str) -> Result<Fetched, String> {
        match self.agent.get(url).set("X-API-Key", api_key).call() {
            Ok(response) => response
                .into_string()
                .map(Fetched::Body)
                .map_err(|e| e.to_string()),
            // A 404 is the service answering "no such account", not a broken request.
            Err(ureq::Error::Status(404, _)) => Ok(Fetched::Absent),
            Err(ureq::Error::Status(code, _)) => Err(format!("HTTP {code}")),
            Err(other) => Err(other.to_string()),
        }
    }
}

/// Read a PP rating out of a profile response body.
///
/// **The accepted shape is inferred, not observed.** The body has never been seen: the
/// endpoint is key-gated and no key was available. `ppScore` and `calibrationMatches` are
/// the names the site's own list views read off profile objects, so those are looked for
/// at the top level and under a `profile` or `statlocker` object. A body that parses as
/// JSON but carries no score is read as [`PpLookup::Absent`] - Statlocker answered, and
/// had no rating to give - which is the reading that stays honest if the real shape turns
/// out to be something else entirely.
///
/// A score of zero or less is no rating rather than a rating of nothing, so it is
/// `Absent` too.
pub fn parse_pp(body: &str) -> PpLookup {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return PpLookup::Failed("response was not JSON".into());
    };
    // An `error` key is how the service reports trouble in a 200 body; the site's own
    // client throws on it rather than treating it as data.
    if let Some(message) = json.get("error").and_then(serde_json::Value::as_str) {
        return PpLookup::Failed(message.to_owned());
    }

    let scope = json
        .get("profile")
        .or_else(|| json.get("statlocker"))
        .unwrap_or(&json);
    let score = ["ppScore", "pp_score", "pp"]
        .iter()
        .find_map(|k| scope.get(*k).and_then(serde_json::Value::as_f64));
    let Some(score) = score.filter(|s| *s > 0.0) else {
        return PpLookup::Absent;
    };
    let calibration_matches = ["calibrationMatches", "calibration_matches"]
        .iter()
        .find_map(|k| scope.get(*k).and_then(serde_json::Value::as_u64))
        .and_then(|n| u32::try_from(n).ok());
    PpLookup::Known(PpRating {
        score,
        badge: pp_badge(score),
        calibration_matches,
    })
}

/// Cached Statlocker PP ratings for the match currently on screen.
///
/// Cheap to clone; every clone shares one cache and one worker.
#[derive(Clone)]
pub struct Statlocker {
    shared: Arc<Shared>,
}

struct Shared {
    ratings: Mutex<HashMap<u32, PpLookup>>,
    /// The match the cache belongs to. A different match empties it.
    match_id: Mutex<Option<u64>>,
    /// `Some` when nothing can be looked up, carrying the reason to show.
    unavailable: Option<String>,
    /// `None` when unavailable, because there is no worker to talk to.
    queue: Option<Sender<u32>>,
}

/// Take a lock, ignoring poisoning.
///
/// A worker thread that panicked mid-request must not take the scoreboard down with it;
/// the cache it left behind is still a valid cache.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Statlocker {
    /// The real thing: `ureq`, with the key from [`API_KEY_ENV`].
    ///
    /// With no key set - the default - nothing is spawned and every lookup answers
    /// [`PpLookup::Unavailable`].
    pub fn from_env() -> Self {
        match std::env::var(API_KEY_ENV) {
            Ok(key) if !key.trim().is_empty() => {
                Self::with_transport(Arc::new(UreqTransport::default()), Some(key), REQUEST_GAP)
            }
            _ => Self::unavailable(format!(
                "no Statlocker API key: set {API_KEY_ENV} to enable lookups"
            )),
        }
    }

    /// A facade that will never look anything up, and says why.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Statlocker {
            shared: Arc::new(Shared {
                ratings: Mutex::new(HashMap::new()),
                match_id: Mutex::new(None),
                unavailable: Some(reason.into()),
                queue: None,
            }),
        }
    }

    /// Build over a given transport. Tests use this to keep the network out of it.
    ///
    /// `gap` is the smallest spacing between two outbound requests.
    pub fn with_transport(
        transport: Arc<dyn Transport>,
        api_key: Option<String>,
        gap: Duration,
    ) -> Self {
        let Some(key) = api_key.filter(|k| !k.trim().is_empty()) else {
            return Self::unavailable("no Statlocker API key");
        };
        let (tx, rx) = mpsc::channel::<u32>();
        let shared = Arc::new(Shared {
            ratings: Mutex::new(HashMap::new()),
            match_id: Mutex::new(None),
            unavailable: None,
            queue: Some(tx),
        });
        let worker = Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut last: Option<Instant> = None;
            // Ends when the last `Statlocker` is dropped and the sender goes with it.
            while let Ok(account) = rx.recv() {
                // A queued id whose entry has gone - the match changed under it - is not
                // worth a request; the next frame will queue it again if it still matters.
                if !matches!(lock(&worker.ratings).get(&account), Some(PpLookup::Pending)) {
                    continue;
                }
                if let Some(previous) = last {
                    let waited = previous.elapsed();
                    if waited < gap {
                        std::thread::sleep(gap - waited);
                    }
                }
                last = Some(Instant::now());
                let outcome = match transport.get(&profile_url(account), &key) {
                    Ok(Fetched::Body(body)) => parse_pp(&body),
                    Ok(Fetched::Absent) => PpLookup::Absent,
                    Err(why) => PpLookup::Failed(why),
                };
                // Only overwrite the `Pending` this request was made for. If the match
                // changed while it was in flight the answer belongs to a cache that no
                // longer exists, and re-inserting it would resurrect a cleared entry.
                let mut ratings = lock(&worker.ratings);
                if let Some(slot @ PpLookup::Pending) = ratings.get_mut(&account) {
                    *slot = outcome;
                }
            }
        });
        Statlocker { shared }
    }

    /// Point the cache at a match, emptying it when that is a different match.
    ///
    /// Called from the poll loop every frame. Ratings are cached for as long as one match
    /// is on screen and no longer, so a stale rating cannot follow a player into the next
    /// one.
    pub fn observe_match(&self, match_id: Option<u64>) {
        let mut current = lock(&self.shared.match_id);
        if *current == match_id {
            return;
        }
        *current = match_id;
        lock(&self.shared.ratings).clear();
    }

    /// This account's PP rating, and never a wait.
    ///
    /// Returns whatever the cache holds right now. The first call for an account queues
    /// the request and answers [`PpLookup::Pending`]; the answer arrives on some later
    /// frame. Nothing here touches a socket.
    pub fn pp(&self, account_id: u32) -> PpLookup {
        if let Some(why) = &self.shared.unavailable {
            return PpLookup::Unavailable(why.clone());
        }
        let mut ratings = lock(&self.shared.ratings);
        if let Some(known) = ratings.get(&account_id) {
            return known.clone();
        }
        ratings.insert(account_id, PpLookup::Pending);
        drop(ratings);
        if let Some(queue) = &self.shared.queue {
            // A dead worker leaves the entry `Pending` forever rather than lying about a
            // rating, and the next match clears it.
            let _ = queue.send(account_id);
        }
        PpLookup::Pending
    }
}

impl From<PpLookup> for StatlockerPpView {
    fn from(lookup: PpLookup) -> Self {
        match lookup {
            PpLookup::Unavailable(why) => StatlockerPpView {
                state: StatlockerPpState::Unavailable,
                reason: Some(why),
                ..Default::default()
            },
            PpLookup::Pending => StatlockerPpView {
                state: StatlockerPpState::Pending,
                ..Default::default()
            },
            PpLookup::Known(rating) => StatlockerPpView {
                state: StatlockerPpState::Known,
                score: Some(rating.score),
                rank: Some(rating.badge.to_string()),
                tier_name: pp_tier_name(rating.badge).map(str::to_owned),
                // Old-era art, always. PP is the pre-matchmaking ladder and the artwork
                // is what says so; the game's own badge draws from the new set.
                art: crate::rank_art::old_era_art(rating.badge),
                calibration_matches: rating.calibration_matches,
                calibrated: rating.is_calibrated(),
                reason: None,
            },
            PpLookup::Absent => StatlockerPpView {
                state: StatlockerPpState::Absent,
                ..Default::default()
            },
            PpLookup::Failed(why) => StatlockerPpView {
                state: StatlockerPpState::Failed,
                reason: Some(why),
                ..Default::default()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A transport that answers from a script and records what it was asked.
    struct Fake {
        answer: Box<dyn Fn(u32) -> Result<Fetched, String> + Send + Sync>,
        calls: Mutex<Vec<(u32, Instant)>>,
        delay: Duration,
    }

    impl Fake {
        fn new(
            answer: impl Fn(u32) -> Result<Fetched, String> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(Fake {
                answer: Box::new(answer),
                calls: Mutex::new(Vec::new()),
                delay: Duration::ZERO,
            })
        }

        fn slow(delay: Duration) -> Arc<Self> {
            Arc::new(Fake {
                answer: Box::new(|_| Ok(Fetched::Absent)),
                calls: Mutex::new(Vec::new()),
                delay,
            })
        }

        fn account_of(url: &str) -> u32 {
            url.rsplit('/')
                .next()
                .and_then(|s| s.parse().ok())
                .expect("an account id in the url")
        }

        fn calls(&self) -> Vec<(u32, Instant)> {
            lock(&self.calls).clone()
        }
    }

    impl Transport for Fake {
        fn get(&self, url: &str, api_key: &str) -> Result<Fetched, String> {
            assert!(!api_key.is_empty(), "a request went out with no key");
            let account = Fake::account_of(url);
            lock(&self.calls).push((account, Instant::now()));
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            (self.answer)(account)
        }
    }

    fn keyed(transport: Arc<dyn Transport>, gap: Duration) -> Statlocker {
        Statlocker::with_transport(transport, Some("test-key".into()), gap)
    }

    /// Poll the cache until it stops saying `Pending`, or give up.
    fn settle(sl: &Statlocker, account: u32) -> PpLookup {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let now = sl.pp(account);
            if now != PpLookup::Pending {
                return now;
            }
            assert!(Instant::now() < deadline, "lookup never settled");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// With no key nothing is asked, and the answer says so rather than saying "no rating".
    #[test]
    fn no_api_key_means_unavailable_and_no_traffic() {
        let fake = Fake::new(|_| Ok(Fetched::Absent));
        let sl = Statlocker::with_transport(fake.clone(), None, Duration::ZERO);
        let PpLookup::Unavailable(why) = sl.pp(7) else {
            panic!("a keyless facade must not report anything but unavailable");
        };
        assert!(!why.is_empty(), "unavailable has to explain itself");
        assert!(fake.calls().is_empty(), "a keyless facade made a request");

        let view = StatlockerPpView::from(sl.pp(7));
        assert_eq!(view.state, StatlockerPpState::Unavailable);
        assert_eq!(view.score, None, "unavailable is not a score");
        assert_eq!(view.rank, None, "unavailable is not a rank");
    }

    /// The first call answers immediately even while the transport is still blocked.
    ///
    /// This is the whole point of the facade: the scoreboard poll loop runs at 250ms, and
    /// a lookup that waited on a socket would stall every frame behind twelve of them.
    #[test]
    fn a_lookup_never_blocks_the_caller() {
        let fake = Fake::slow(Duration::from_millis(600));
        let sl = keyed(fake, Duration::ZERO);

        let started = Instant::now();
        assert_eq!(sl.pp(1), PpLookup::Pending);
        assert_eq!(sl.pp(1), PpLookup::Pending);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(200),
            "pp() waited {elapsed:?} on a transport that takes 600ms"
        );
    }

    /// One request per account per match, however many frames ask.
    #[test]
    fn a_rating_is_fetched_once_and_then_cached() {
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let fake = Fake::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Fetched::Body(
                r#"{"accountId":42,"ppScore":5150.5,"calibrationMatches":120}"#.into(),
            ))
        });
        let sl = keyed(fake.clone(), Duration::ZERO);
        sl.observe_match(Some(11));

        let settled = settle(&sl, 42);
        assert_eq!(
            settled,
            PpLookup::Known(PpRating {
                score: 5150.5,
                badge: RankBadge(93),
                calibration_matches: Some(120),
            })
        );
        for _ in 0..60 {
            assert_eq!(sl.pp(42), settled);
        }
        assert_eq!(seen.load(Ordering::SeqCst), 1, "the cache did not hold");
    }

    /// A new match empties the cache, so last match's rating cannot follow a player.
    #[test]
    fn a_new_match_drops_the_cache() {
        let fake = Fake::new(|_| Ok(Fetched::Body(r#"{"ppScore":2000}"#.into())));
        let sl = keyed(fake.clone(), Duration::ZERO);
        sl.observe_match(Some(1));
        settle(&sl, 5);

        sl.observe_match(Some(1));
        assert_ne!(sl.pp(5), PpLookup::Pending);

        sl.observe_match(Some(2));
        assert_eq!(sl.pp(5), PpLookup::Pending, "a new match kept old ratings");
        settle(&sl, 5);
        assert_eq!(fake.calls().len(), 2);
    }

    /// Requests are spaced, so a twelve-player lobby cannot become a burst.
    #[test]
    fn requests_are_rate_limited() {
        let gap = Duration::from_millis(60);
        let fake = Fake::new(|_| Ok(Fetched::Absent));
        let sl = keyed(fake.clone(), gap);
        for account in 1..=4 {
            assert_eq!(sl.pp(account), PpLookup::Pending);
        }
        for account in 1..=4 {
            settle(&sl, account);
        }

        let calls = fake.calls();
        assert_eq!(calls.len(), 4);
        for pair in calls.windows(2) {
            let spacing = pair[1].1.duration_since(pair[0].1);
            assert!(
                spacing + Duration::from_millis(10) >= gap,
                "requests {} and {} were only {spacing:?} apart",
                pair[0].0,
                pair[1].0
            );
        }
    }

    /// Absent, failed and pending stay three separate answers all the way to the view.
    #[test]
    fn the_three_failure_states_never_collapse_into_one() {
        let fake = Fake::new(|account| match account {
            1 => Ok(Fetched::Absent),
            2 => Err("connection refused".into()),
            _ => Ok(Fetched::Body(r#"{"ppScore":0}"#.into())),
        });
        let sl = keyed(fake, Duration::ZERO);

        assert_eq!(
            StatlockerPpView::from(sl.pp(1)).state,
            StatlockerPpState::Pending
        );

        assert_eq!(settle(&sl, 1), PpLookup::Absent);
        let PpLookup::Failed(why) = settle(&sl, 2) else {
            panic!("a transport error must surface as failed, not as absent");
        };
        assert_eq!(why, "connection refused");
        assert_eq!(settle(&sl, 3), PpLookup::Absent);

        let known = StatlockerPpView::from(PpLookup::Known(PpRating {
            score: 5150.5,
            badge: pp_badge(5150.5),
            calibration_matches: Some(120),
        }));
        assert_eq!(known.state, StatlockerPpState::Known);
        assert_eq!(known.score, Some(5150.5), "the score is the reported value");
        assert_eq!(known.rank.as_deref(), Some("9-3"));
        assert_eq!(known.tier_name.as_deref(), Some("Phantom"));
        assert_eq!(
            known.art,
            crate::rank_art::old_era_art(RankBadge(93)),
            "a PP rating must carry the pre-matchmaking artwork"
        );
        assert!(known.calibrated);
        assert_eq!(known.reason, None, "a known rating has nothing to excuse");

        let states = [
            StatlockerPpView::from(PpLookup::Pending).state,
            StatlockerPpView::from(PpLookup::Absent).state,
            StatlockerPpView::from(PpLookup::Failed("x".into())).state,
            StatlockerPpView::from(PpLookup::Unavailable("y".into())).state,
        ];
        for (i, a) in states.iter().enumerate() {
            for b in &states[i + 1..] {
                assert_ne!(a, b, "two distinct outcomes serialise the same");
            }
        }
    }

    /// A PP rating is drawn in the old era's artwork, which is what labels its source.
    ///
    /// The row shows no caption saying which rank came from where - the badge art is the
    /// label - so a PP rating carrying the game's current badge set would silently claim
    /// to be the Valve rank sitting next to it.
    #[test]
    fn a_pp_rating_carries_old_era_art_and_never_the_new_set() {
        let view = StatlockerPpView::from(PpLookup::Known(PpRating {
            score: 700.0,
            badge: pp_badge(700.0),
            calibration_matches: None,
        }));
        let art = view.art.expect("a ranked PP badge has artwork");
        assert_eq!(art, crate::rank_art::old_era_art(RankBadge(21)).unwrap());
        assert_ne!(
            Some(&art),
            crate::rank_art::new_era_art(RankBadge(21)).as_ref(),
            "PP must never draw with the game's current badge art"
        );

        for lookup in [
            PpLookup::Pending,
            PpLookup::Absent,
            PpLookup::Failed("x".into()),
            PpLookup::Unavailable("y".into()),
        ] {
            let view = StatlockerPpView::from(lookup.clone());
            assert_eq!(view.art, None, "{lookup:?} must not paint a badge");
        }
    }

    /// The inferred parse: what it accepts, and what it refuses to guess at.
    #[test]
    fn a_body_without_a_score_is_absent_rather_than_a_guess() {
        assert_eq!(
            parse_pp(r#"{"profile":{"ppScore":700,"calibrationMatches":12}}"#),
            PpLookup::Known(PpRating {
                score: 700.0,
                badge: RankBadge(21),
                calibration_matches: Some(12),
            })
        );
        assert_eq!(parse_pp(r#"{"valve":{"badge":93}}"#), PpLookup::Absent);
        assert_eq!(parse_pp("{}"), PpLookup::Absent);
        assert_eq!(parse_pp(r#"{"ppScore":0}"#), PpLookup::Absent);

        let PpLookup::Failed(why) = parse_pp(r#"{"error":"Missing API key"}"#) else {
            panic!("an error body must not read as absent");
        };
        assert_eq!(why, "Missing API key");
        assert!(matches!(parse_pp("<html>"), PpLookup::Failed(_)));
    }

    /// The PP-to-badge conversion, against the bundle's own arithmetic.
    #[test]
    fn a_pp_score_converts_to_the_badge_the_site_would_draw() {
        assert_eq!(pp_badge(100.0), RankBadge(11));
        assert_eq!(pp_badge(699.0), RankBadge(16));
        assert_eq!(pp_badge(700.0), RankBadge(21), "600 PP is one whole tier");
        assert_eq!(pp_badge(5150.0), RankBadge(93), "step 51 is Phantom 3");

        assert_eq!(pp_badge(6600.0), RankBadge(116));
        assert_eq!(pp_badge(999_999.0), RankBadge(116));

        assert_eq!(pp_badge(1.0), RankBadge(11));
        assert!(!pp_badge(0.0).is_ranked());
        assert!(!pp_badge(-5.0).is_ranked());
        assert!(!pp_badge(f64::NAN).is_ranked());
    }

    /// PP tiers are Statlocker's own ladder and must not be named with the game's.
    ///
    /// The two lists disagree - tier 3 is `Alchemist` here and something else entirely on
    /// Valve's - so naming a PP tier from `deadlock_data::RankNames` would print a rank
    /// the player does not hold.
    #[test]
    fn pp_tiers_carry_statlockers_own_names() {
        assert_eq!(pp_tier_name(RankBadge(11)), Some("Initiate"));
        assert_eq!(pp_tier_name(RankBadge(33)), Some("Alchemist"));
        assert_eq!(pp_tier_name(RankBadge(93)), Some("Phantom"));
        assert_eq!(pp_tier_name(RankBadge(116)), Some("Eternus"));
        assert_eq!(pp_tier_name(RankBadge(0)), None);
        assert_eq!(pp_tier_name(RankBadge(126)), None);

        let valve = deadlock_data::RankNames::bundled();
        assert_ne!(
            valve.tier_name(3),
            pp_tier_name(RankBadge(33)),
            "if these ever agree, the reason for a separate table is gone"
        );
    }

    /// An uncalibrated rating says so, because the site refuses to draw one as a rank.
    #[test]
    fn an_uncalibrated_rating_is_flagged_rather_than_hidden() {
        let fresh = PpRating {
            score: 1200.0,
            badge: pp_badge(1200.0),
            calibration_matches: Some(3),
        };
        assert!(!fresh.is_calibrated());
        let view = StatlockerPpView::from(PpLookup::Known(fresh));
        assert!(!view.calibrated);
        assert_eq!(view.calibration_matches, Some(3));
        assert_eq!(view.state, StatlockerPpState::Known);
        assert_eq!(view.score, Some(1200.0));

        let settled = PpRating {
            calibration_matches: Some(CALIBRATION_MATCHES),
            ..fresh
        };
        assert!(settled.is_calibrated());
        let counted = PpRating {
            calibration_matches: None,
            ..fresh
        };
        assert!(counted.is_calibrated());
        assert_eq!(
            StatlockerPpView::from(PpLookup::Known(counted)).calibration_matches,
            None,
            "an unreported count must not be invented as zero"
        );
    }

    /// The URL is built from the account id, and goes to the profile - not the Valve
    /// mirror, which is a different measurement the overlay already has from memory.
    #[test]
    fn the_profile_url_is_built_from_the_declared_base() {
        assert_eq!(
            profile_url(347_246_372),
            "https://statlocker.gg/api/profile/347246372"
        );
        assert!(
            profile_url(1).starts_with(STATLOCKER_BASE),
            "lookups must go to the one declared base"
        );
        assert!(
            !profile_url(1).contains("valve-rank"),
            "the Valve badge comes from memory; this endpoint is for Statlocker's own PP"
        );
        assert!(
            !profile_url(1).contains("deadlock-api.com"),
            "statlocker and deadlock-api.com are different services"
        );
    }
}
