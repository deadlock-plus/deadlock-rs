/**
 * Mirrors the payloads emitted by `src-tauri/src/model.rs`.
 *
 * Kept by hand rather than generated: the set is small, and writing it out means a change
 * on the Rust side shows up as a type error here instead of as an undefined at runtime.
 * Field names match the `camelCase` serde rename on those structs.
 */

/**
 * A hero named for display, with whatever art the catalog publishes for it.
 *
 * `portrait` and `card` are remote URLs, not bytes. `null` means the catalog has no art
 * of that kind — not that there is no hero — so the window draws a deliberate stand-in
 * keyed on `heroId` rather than an empty box.
 */
export interface HeroRef {
  heroId: number;
  name: string;
  portrait: string | null;
  card: string | null;
}

/**
 * One player swapping from one hero to another, mid-match.
 *
 * Both heroes are always identified — the reader only reports a swap between two known
 * heroes, never a hero id that has just started or stopped reading. The *player* is not:
 * `slot` and `player` are each `null` when the reader could not attribute the swap, and
 * the popup has to say so rather than paint slot zero or a nameless row.
 */
export interface HeroSwapView {
  /**
   * Stable while a swap is being carried, never reused afterwards.
   *
   * The same swap arrives on many consecutive frames on purpose (see `MatchView.heroSwaps`),
   * so this is the only thing separating a re-sent swap from a second one. Act on it, not
   * on the list changing.
   */
  seq: number;
  /** Lobby slot, or `null` when the row carried no slot. Never `0` as a stand-in. */
  slot: number | null;
  /** Steam persona name, or `null` when it did not read. Not a `Slot 4` fallback. */
  player: string | null;
  from: HeroRef;
  to: HeroRef;
}

/**
 * Where a Statlocker PP lookup for one player stands.
 *
 * Five outcomes, not one nullable rating, because they call for five different things on
 * screen:
 *
 * - `unavailable` — nothing was asked and nothing will be. `reason` says why; the usual
 *   one is that no Statlocker API key is configured.
 * - `pending` — asked for, no answer yet. An answer is expected on a later frame.
 * - `known` — Statlocker answered, and `score` is its PP rating.
 * - `absent` — Statlocker answered, and has no PP rating for this account.
 * - `failed` — the request fell over. `reason` says how.
 *
 * `absent`, `failed` and `unavailable` must not paint the same, and none of them is
 * "unranked".
 */
export type StatlockerPpState =
  | 'unavailable'
  | 'pending'
  | 'known'
  | 'absent'
  | 'failed';

/**
 * A player's Statlocker PP rating, or the reason there is not one.
 *
 * PP — "performance points" — is Statlocker's own ladder, from before Valve shipped
 * ranked matchmaking. It is a different measurement from `PlayerView.rank`, which is the
 * game's own badge read out of memory, and the two are free to disagree.
 *
 * Read `state` first: every other field is `null` unless that state has something to put
 * in it, and `null` here never means zero.
 */
export interface StatlockerPpView {
  state: StatlockerPpState;
  /** The performance-points score, the reported value. `null` unless `state` is `known`. */
  score: number | null;
  /** The badge that score draws as, as `tier-subrank`. `null` unless `state` is `known`. */
  rank: string | null;
  /** Statlocker's own name for that tier — its ladder kept the pre-matchmaking names, so
   * this is not one of the game's current tier names. */
  tierName: string | null;
  /**
   * **Old-era** badge art for `rank`, or `null` when there is no badge to draw.
   *
   * The era of the artwork is what identifies the source: this is always the
   * pre-matchmaking badge set, and never the game's current one, so a row showing both
   * badges needs no caption to say which is which. `null` is not a blank cell — read
   * `state` to find out whether it means "no rating", "not looked up yet" or "the request
   * failed", and paint a deliberate placeholder for each, the way `HeroArt` does.
   */
  art: string | null;
  /** Matches played towards calibration, when the answer carried a count. */
  calibrationMatches: number | null;
  /** `false` means Statlocker is still calibrating this rating and would not draw the
   * badge as a rank yet. Always `true` when `state` is not `known`. */
  calibrated: boolean;
  /** Why, for `failed` and `unavailable`. `null` for the other three. */
  reason: string | null;
}

export interface PlayerView {
  slot: number | null;
  team: number;
  name: string;
  hero: string;
  heroId: number | null;
  /** Portrait URL. `null` both when the hero did not read and when it has no published
   * art; `heroId` tells the two apart, and neither is drawn as a blank. */
  heroPortrait: string | null;
  heroCard: string | null;
  level: number | null;
  kills: number;
  deaths: number;
  assists: number;
  heroDamage: number;
  objectiveDamage: number;
  healing: number;
  netWorth: number;
  health: number;
  maxHealth: number;
  /**
   * The account behind this row, 32-bit rather than Steam64.
   *
   * `null` when the id did not read, or when it is not an individual account — a bot slot
   * has no account id at all. Narrow on purpose: a Steam64 does not survive a JSON number
   * and would arrive silently rounded.
   */
  accountId: number | null;
  /**
   * Packed rank badge as `tier-subrank`; empty string when unranked.
   *
   * Empty covers both "unranked" and "the field did not read" — `rankRead` tells those
   * apart. Never draw an empty string as unranked on its own.
   */
  rank: string;
  /** `false` means the rank field did not read, which is not the same as unranked. */
  rankRead: boolean;
  /**
   * **New-era** badge art for `rank`, or `null` when there is no badge to draw.
   *
   * Always the game's current badge set, never the old one: the era of the artwork is
   * what tells a viewer this badge is the game's own rank rather than the Statlocker PP
   * rating beside it, so it must never fall back to the other era's art.
   *
   * `null` covers three different facts — the field did not read, the player is unranked,
   * and the badge sits off the ladder — and `rankRead` and `rank` tell them apart. None
   * of them is a blank cell.
   *
   * Served at one size only, unlike `statlockerPp.art`, so scale it down for a row icon.
   */
  rankArt: string | null;
  /** Statlocker's PP rating for this player, a separate ladder from the badge above and
   * free to disagree with it. Read its `state` before its `score`. */
  statlockerPp: StatlockerPpView;
  alive: boolean;
  isLocal: boolean;
  isObserved: boolean;
  /**
   * The row the client is effectively *being* — you, or whoever you are watching.
   *
   * Neither `isLocal` nor `isObserved` answers that alone: while spectating, `isLocal` is
   * the observer controller, which is not a scoreboard row at all, and while dead
   * `isObserved` wanders onto a teammate. At most one row carries this, and **no** row
   * carries it when nobody could be identified — see `MatchView.currentIdentified`.
   */
  isCurrent: boolean;
  abandoned: boolean;
  /** `false` means every number on this row is a placeholder zero, not a result. */
  statsRead: boolean;
}

export interface TeamView {
  team: number;
  name: string;
  kills: number;
  deaths: number;
  assists: number;
  heroDamage: number;
  objectiveDamage: number;
  healing: number;
  souls: number;
  players: number;
  /** Souls ahead of the other side; negative when behind. */
  soulLead: number;
  /** `false` means these totals are understated, not that the side has none. */
  complete: boolean;
}

/** Objective timers and health, pre-formatted by the reader. */
export interface ObjectivesView {
  midbossHealth: string | null;
  midbossFraction: number | null;
  midbossRespawn: string | null;
  riftContested: boolean;
  riftProgress: number | null;
  riftGiveUp: string | null;
  urnCount: number;
  /** Schedule-derived, not read from the game. */
  bridgeBuff: string | null;
  brawlPhase: string | null;
  brawlNextPhase: string | null;
  brawlRound: number | null;
}

export interface MatchView {
  attached: boolean;
  inMatch: boolean;
  status: string;
  matchId: number | null;
  clock: string | null;
  teams: TeamView[];
  objectives: ObjectivesView;
  players: PlayerView[];
  /** Banned heroes, named and with art. Empty both when nothing is banned and when the
   * vector could not be read; `bansReadable` tells the two apart. */
  bans: HeroRef[];
  /** Whether `bans` is a measurement rather than a gap. Never draw `false` as "no bans". */
  bansReadable: boolean;
  /**
   * Whether the reader identified whoever is on screen at all.
   *
   * `false` is free camera, a replay with no target, or a tick before anyone spawned —
   * and it is what stops "no row is highlighted" from being read as a broken highlight.
   * Never guess a row when this is `false`.
   */
  currentIdentified: boolean;
  /**
   * Hero swaps recent enough to still be worth announcing, oldest first.
   *
   * Not a per-frame diff. Each entry is re-sent on every frame for about ten seconds so a
   * window that missed a frame still sees it, which means the list changing is meaningless
   * — dedupe on `seq`.
   */
  heroSwaps: HeroSwapView[];
  /** Winning team number, only once the match is actually decided. */
  winner: number | null;
  error: string | null;
}

export interface PartyMemberView {
  accountId: number;
  name: string;
  rank: string;
  ready: boolean | null;
  isCreator: boolean;
  queuedAs: HeroRef[];
}

export interface PartyView {
  partyId: number | null;
  joinCode: string | null;
  queueing: boolean;
  queue: string;
  /** Seconds since matchmaking began. May span more than one queue attempt. */
  matchmakingSeconds: number | null;
  region: string;
  preference: string;
  members: PartyMemberView[];
  /** Members disagree on the game build, which silently blocks queueing. */
  buildMismatch: boolean;
}

/** Team numbers the game uses; anything else is a spectator or neutral. */
export const AMBER = 2;
export const SAPPHIRE = 3;

export const EMPTY_MATCH: MatchView = {
  attached: false,
  inMatch: false,
  status: 'starting…',
  matchId: null,
  clock: null,
  teams: [],
  objectives: {
    midbossHealth: null,
    midbossFraction: null,
    midbossRespawn: null,
    riftContested: false,
    riftProgress: null,
    riftGiveUp: null,
    urnCount: 0,
    bridgeBuff: null,
    brawlPhase: null,
    brawlNextPhase: null,
    brawlRound: null,
  },
  players: [],
  bans: [],
  bansReadable: false,
  currentIdentified: false,
  heroSwaps: [],
  winner: null,
  error: null
};
