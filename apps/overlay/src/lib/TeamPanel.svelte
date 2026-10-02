<script lang="ts">
  import Icon from './Icon.svelte';
  import PlayerRow from './PlayerRow.svelte';
  import { short, lead } from './format';
  import type { PlayerView, TeamView } from './types';

  let {
    team,
    players,
    accent
  }: { team: TeamView | undefined; players: PlayerView[]; accent: string } = $props();

  const leadTone = $derived((team?.soulLead ?? 0) > 0 ? 'text-good' : 'text-bad');
</script>

<!-- A ruled box, not a card: 1px edges, a 2px bar in the side's own colour along the top,
     and horizontal rules between the bands so the panel reads as a table. -->
<section
  class="flex min-w-0 flex-col rounded-none border border-line border-t-2 bg-panel"
  style="border-top-color: {accent}"
>
  <header class="flex items-baseline justify-between gap-2 border-b border-line px-2 py-1.5">
    <h2 class="text-tiny font-bold tracking-[0.09em] uppercase" style="color: {accent}">
      {team?.name ?? '—'}
    </h2>
    <div
      class="flex items-center gap-1 font-semibold tabular-nums [&_svg]:ml-1.5 [&_svg]:text-icon [&_svg:first-child]:ml-0"
    >
      <Icon name="kills" /><span>{team?.kills ?? 0}</span>
      <Icon name="deaths" /><span class="text-muted">{team?.deaths ?? 0}</span>
      <Icon name="assists" /><span class="text-muted">{team?.assists ?? 0}</span>
    </div>
  </header>

  <!-- A side whose totals are understated says so rather than showing them as real.
       `complete` is false when a player's m_PlayerDataGlobal did not resolve, which drops
       all of that player's statistics at once; the remaining numbers are a floor, not a
       result. Only shown when it is false, since the ordinary case needs no decoration. -->
  {#if team && !team.complete}
    <p class="text-nano border-b border-line px-2 py-[2px] text-dim italic">
      totals are a floor — a player's statistics did not read
    </p>
  {/if}

  <!-- The collective line: four numbers that decide who is winning. -->
  <div
    class="grid grid-cols-2 gap-x-2 gap-y-[3px] rounded-none border-b border-line bg-panel-2 px-2 py-1.5 [&_svg]:text-icon"
  >
    <div class="flex min-w-0 items-center gap-1.5">
      <Icon name="souls" />
      <b class="font-semibold tabular-nums">{short(team?.souls ?? 0)}</b>
      {#if team?.soulLead}
        <em class="text-mini tabular-nums not-italic {leadTone}">{lead(team.soulLead)}</em>
      {/if}
    </div>
    <div class="flex min-w-0 items-center gap-1.5">
      <Icon name="damage" /><b class="font-semibold tabular-nums">{short(team?.heroDamage ?? 0)}</b>
    </div>
    <div class="flex min-w-0 items-center gap-1.5">
      <Icon name="objective" /><b class="font-semibold tabular-nums"
        >{short(team?.objectiveDamage ?? 0)}</b
      >
    </div>
    <div class="flex min-w-0 items-center gap-1.5">
      <Icon name="healing" /><b class="font-semibold tabular-nums">{short(team?.healing ?? 0)}</b>
    </div>
  </div>

  <!-- Rules between rows rather than gaps: at a glance this is a table, and a 1px line is
       what says "next row" without spending vertical space on it. The panel is sized by
       its rows and the grid outside is what scrolls, so a short window never clips a row
       off the bottom of a side. -->
  <div class="flex flex-col divide-y divide-line/70">
    {#each players as p (p.slot ?? p.name)}
      <PlayerRow player={p} {accent} />
    {:else}
      <p class="my-2 text-center text-dim">no players</p>
    {/each}
  </div>
</section>
