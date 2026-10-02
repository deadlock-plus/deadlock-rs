<script lang="ts">
  import HeroArt from './HeroArt.svelte';
  import Icon from './Icon.svelte';
  import { short } from './format';
  import type { PlayerView } from './types';

  let { player, accent }: { player: PlayerView; accent: string } = $props();

  const hp = $derived(
    player.maxHealth > 0 ? Math.max(0, Math.min(1, player.health / player.maxHealth)) : 0
  );
  const hurt = $derived(hp > 0 && hp < 0.35);

  /**
   * Whoever is on screen — you, or the player you are watching — is marked in the product
   * accent rather than the team's own colour, so "this row is yours" never has to be told
   * apart from "this row is on the amber side" at a glance.
   *
   * Only ever set by the reader on a row it actually identified: when nobody could be, no
   * row carries this and the header says so instead of a row being guessed at.
   *
   * Resolved to one value here rather than layered as two `outline-color` utilities, which
   * would be settled by stylesheet order instead of by which state is true.
   */
  const ring = $derived(
    player.isCurrent ? 'var(--color-rust)' : player.isLocal ? 'var(--color-line)' : null
  );

  // Same reason: dead and unread both want an opacity, and only one of them can win.
  const kdaFade = $derived(!player.alive ? 'opacity-45' : !player.statsRead ? 'opacity-50' : '');
  const fade = $derived(player.alive ? '' : 'opacity-45');

  // `-` is what the reader sends for a hero it could not identify. Dimmed so it reads as
  // an admitted gap rather than as a hero called "-".
  const selected = $derived(player.isCurrent ? 'bg-rust/12' : '');
  const hpFade = $derived(player.alive ? 'opacity-[0.14]' : 'opacity-[0.05]');

  const heroTone = $derived(player.heroId === null ? 'font-normal text-dim' : 'font-semibold');
  const pp = $derived(player.statlockerPp);

  // Every non-`known` PP state gets its own mark: a rating nobody has, a lookup still in
  // flight and a request that fell over are three different facts, and collapsing them
  // into a blank would say Statlocker has no rating when it was never asked.
  const ppMark = $derived(
    pp.state === 'pending' ? '···' : pp.state === 'absent' ? '–' : '!'
  );
  const ppTitle = $derived(
    pp.state === 'known'
      ? `Statlocker PP ${pp.score ?? '?'}${pp.tierName ? ` (${pp.tierName})` : ''}${pp.calibrated ? '' : ' - still calibrating'}`
      : pp.state === 'pending'
        ? 'Statlocker PP: looking up'
        : pp.state === 'absent'
          ? 'Statlocker has no PP rating for this player'
          : (pp.reason ?? 'Statlocker PP unavailable')
  );

  const rankTone = $derived(
    player.rank ? 'border border-line bg-panel-2 px-[5px] font-bold' : 'font-normal text-dim'
  );

  // The new-era Valve badge art bakes in a tiny subrank numeral that is unreadable at row
  // size, so the subrank is painted over the badge as crisp text instead. `player.rank` is
  // `tier-subrank`; a subrank outside 1-6 means the string was not the shape expected, and
  // gets no overlay rather than a wrong numeral.
  const ROMAN = ['I', 'II', 'III', 'IV', 'V', 'VI'];
  const valveSubrank = $derived.by(() => {
    const parts = player.rank.split('-');
    const n = parts.length === 2 ? Number(parts[1]) : NaN;
    return Number.isInteger(n) && n >= 1 && n <= 6 ? ROMAN[n - 1] : null;
  });
</script>

<div
  class="row isolate grid grid-cols-[auto_auto_auto_1fr_auto] items-center gap-x-1.5 gap-y-px rounded-none px-2 py-2 {selected}"
  style:outline={ring ? `1px solid ${ring}` : undefined}
  style:outline-offset={ring ? '-1px' : undefined}
>
  <!-- Health is the one value worth reading without looking, so it is the row itself
       rather than another column. Squared off with the row; a rounded fill inside a
       square row read as a pill floating behind the text. -->
  <div
    class="absolute inset-y-0 left-0 -z-10 rounded-none transition-[width] duration-[180ms] ease-linear {hpFade}"
    style="width: {hp * 100}%; background: {accent}"
  ></div>

  <!-- A bar on the leading edge, so the row is still identifiable where the outline is
       lost against the health fill behind it. -->
  {#if player.isCurrent}
    <div class="absolute inset-y-0 left-0 w-[2px] rounded-none bg-rust"></div>
  {/if}

  <!-- The portrait is what the eye lands on first, so it carries the team ring: a row
       scrolled away from its header still says which side it is on. A hero with no art,
       or no network to fetch it, gets its initials rather than an empty square. -->
  <div class="mr-[2px] self-center [grid-area:art]">
    <HeroArt
      name={player.hero}
      heroId={player.heroId}
      portrait={player.heroPortrait}
      card={player.heroCard}
      size={64}
      {accent}
      faded={!player.alive}
    />
  </div>

  <div
    class="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap [grid-area:hero] {heroTone} {fade}"
  >
    {player.hero}{#if player.abandoned}<span class="text-mini font-normal text-bad">
        (left)</span
      >{/if}
  </div>
  <div
    class="text-micro rounded-none border border-line bg-panel-2 px-1 tabular-nums text-muted [grid-area:lvl]"
  >
    {player.level ?? '-'}
  </div>
  <!-- Badge art, not a number, and the era of the art is the source: the current badges
       are the Valve rank read from game memory, the pre-matchmaking badges are Statlocker's
       PP rating. A reader who can see both knows which is which without a caption, so
       there deliberately is none. Neither badge ever falls back to the other era's art -
       that would misreport where the rank came from. -->
  <div class="flex h-full items-center justify-center gap-1.5 pl-1 [grid-area:rank]">
    {#if player.rankArt}
      <div class="relative h-20 w-20 shrink-0" title="Valve rank {player.rank}">
        <img src={player.rankArt} alt="Valve rank {player.rank}" class="h-full w-full object-contain" />
        {#if valveSubrank}
          <span
            class="pointer-events-none absolute inset-x-0 bottom-[-2px] text-center text-2xl font-black leading-none text-fg [text-shadow:0_0_4px_#000,0_0_4px_#000,0_1px_2px_#000]"
            >{valveSubrank}</span
          >
        {/if}
      </div>
    {:else}
      <span
        class="flex h-20 w-20 items-center justify-center text-mini {rankTone}"
        title={player.rankRead ? 'Unranked' : 'Rank did not read'}>{player.rankRead ? '—' : '?'}</span
      >
    {/if}
    {#if pp.art}
      <img
        src={pp.art}
        alt="Statlocker PP {pp.tierName}"
        title={ppTitle}
        class="h-20 w-20 shrink-0 object-contain {pp.calibrated ? '' : 'opacity-50'}"
      />
    {:else if pp.state !== 'unavailable'}
      <span
        class="flex h-20 w-20 items-center justify-center text-micro text-dim"
        title={ppTitle}>{ppMark}</span
      >
    {/if}
  </div>
  <!-- A dash, never a zero. `statsRead` is false when m_PlayerDataGlobal did not resolve,
       which drops all of this player's statistics at once — so the numbers below would be
       placeholder zeros, indistinguishable from a player who has done nothing. -->
  <div class="text-right tracking-[0.2px] tabular-nums [grid-area:kda] {kdaFade}">
    {#if player.statsRead}
      <span>{player.kills}</span><i class="px-px text-dim not-italic">/</i><span class="text-muted"
        >{player.deaths}</span
      ><i class="px-px text-dim not-italic">/</i><span>{player.assists}</span>
    {:else}
      <span title="statistics did not read">–/–/–</span>
    {/if}
  </div>

  <div
    class="text-mini min-w-0 overflow-hidden text-ellipsis whitespace-nowrap text-muted [grid-area:name] {fade}"
  >
    {player.name}
  </div>
  <!-- Icons rather than a header row: the row explains itself wherever it is scrolled to. -->
  <div
    class="text-mini flex justify-end gap-[9px] tabular-nums text-muted [grid-area:stats] {fade}"
  >
    <span class="inline-flex items-center gap-[3px] whitespace-nowrap" title="hero damage"
      ><Icon name="damage" size={12} />{player.statsRead ? short(player.heroDamage) : '–'}</span
    >
    <span class="inline-flex items-center gap-[3px] whitespace-nowrap" title="objective damage"
      ><Icon name="objective" size={12} />{player.statsRead
        ? short(player.objectiveDamage)
        : '–'}</span
    >
    <span
      class="inline-flex items-center gap-[3px] whitespace-nowrap"
      class:hurt
      class:text-bad={hurt}
      title="health"><Icon name="health" size={12} />{player.maxHealth > 0 ? player.health : '-'}</span
    >
  </div>
</div>

<style>
  /* Kept as scoped CSS, deliberately:

     - `grid-template-areas` is two quoted strings, which as a Tailwind arbitrary value
       becomes an unreadable pile of underscores and escaped quotes for no gain.
     - `position: relative` pairs with the areas here so the health fill and the leading
       accent bar have something to anchor to.
     - The icon colour has to reach into a child component's `<svg>`, and it has an
       override (`.hurt`) that has to win by specificity rather than by which utility the
       sheet happened to emit last. */
  .row {
    position: relative;
    grid-template-areas:
      'art hero lvl kda rank'
      'art name name stats rank';
  }

  .row :global(svg) {
    color: var(--color-icon);
  }

  .hurt :global(svg) {
    color: var(--color-bad);
  }
</style>
