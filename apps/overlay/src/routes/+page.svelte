<script lang="ts">
  import { onMount } from 'svelte';
  import HeroArt from '$lib/HeroArt.svelte';
  import Icon from '$lib/Icon.svelte';
  import TeamPanel from '$lib/TeamPanel.svelte';
  import ObjectivePanel from '$lib/ObjectivePanel.svelte';
  import BanStrip from '$lib/BanStrip.svelte';
  import SwapToasts from '$lib/SwapToasts.svelte';
  import { mmss } from '$lib/format';
  import { AMBER, EMPTY_MATCH, SAPPHIRE, type MatchView, type PartyView } from '$lib/types';

  let match = $state<MatchView>(EMPTY_MATCH);
  let party = $state<PartyView | null>(null);

  const amber = $derived(match.teams.find((t) => t.team === AMBER));
  const sapphire = $derived(match.teams.find((t) => t.team === SAPPHIRE));
  // The winning side's own name, not "Amber"/"Sapphire": a custom lobby renames teams,
  // and the reader already carries whatever the game calls them. Falls back to the team
  // number only when the side never read, which is the same case that leaves the panels
  // unlabelled.
  const winnerName = $derived(
    match.winner === null
      ? null
      : (match.teams.find((t) => t.team === match.winner)?.name ?? `team ${match.winner}`)
  );
  const amberPlayers = $derived(match.players.filter((p) => p.team === AMBER));
  const sapphirePlayers = $derived(match.players.filter((p) => p.team === SAPPHIRE));

  // The winner keeps its own side's colour: it is a fact about a team, not about the app.
  const winnerTone = $derived(
    match.winner === AMBER ? 'text-amber' : match.winner === SAPPHIRE ? 'text-sapphire' : ''
  );
  const statusTone = $derived(match.attached ? 'font-semibold' : 'font-normal text-muted');

  // One squared-off chip, defined once so every one of them is the same box.
  const CHIP =
    'inline-flex items-center gap-[5px] rounded-none border border-line bg-panel-2 px-2 py-px whitespace-nowrap [&_svg]:text-icon';

  onMount(() => {
    let stop: Array<() => void> = [];
    // Running `vite dev` in a plain browser has no Tauri to talk to. Failing softly there
    // keeps the layout previewable without the game or the shell.
    (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        stop.push(await listen<MatchView>('match', (e) => (match = e.payload)));
        stop.push(await listen<PartyView | null>('party', (e) => (party = e.payload)));
      } catch (e) {
        match = {
          ...EMPTY_MATCH,
          status: 'no bridge to the reader',
          error: e instanceof Error ? e.message : String(e)
        };
      }
    })();
    return () => stop.forEach((f) => f());
  });
</script>

<main class="flex h-full flex-col gap-2 overflow-hidden p-2">
  <!-- The one rule in the app drawn in the accent rather than the neutral line: it is
       what separates the app's own chrome from the readout below it. -->
  <!-- Wraps rather than clips. At 340px the chips do not fit beside the status, and the
       one that falls off the end is "no camera target" - a state the reader went out of
       its way to establish. A second line costs 20px; losing it costs the fact. -->
  <header
    class="flex flex-wrap items-center justify-between gap-x-2 gap-y-1 border-b border-rust-dim pb-2"
  >
    <div class="flex min-w-0 items-center gap-2">
      <span class="text-mini font-extrabold tracking-[0.14em] text-rust-text">DeadRS</span>
      <span class="overflow-hidden text-ellipsis whitespace-nowrap {statusTone}">{match.status}</span
      >
    </div>
    <div class="flex min-w-0 flex-wrap items-center justify-end gap-x-2 gap-y-1 tabular-nums">
      {#if match.clock}
        <span class={CHIP}><Icon name="clock" />{match.clock}</span>
      {/if}
      {#if match.matchId}
        <span class="{CHIP} text-dim">#{match.matchId}</span>
      {/if}
      {#if winnerName}
        <span class="{CHIP} font-semibold {winnerTone}">{winnerName} win</span>
      {/if}
      <!-- Said out loud rather than left as an unhighlighted scoreboard. No row marked as
           "on screen" happens for real reasons - free camera, a replay with no target, a
           tick before anyone spawned - and it looks exactly like the highlight having
           broken. The reader distinguishes them; this is where that reaches the viewer. -->
      {#if match.inMatch && !match.currentIdentified}
        <span class="{CHIP} text-dim" title="Nobody is being followed, so no row is marked as yours"
          >no camera target</span
        >
      {/if}
    </div>
  </header>

  {#if party}
    <section
      class="text-tiny flex flex-wrap items-center gap-2 rounded-none border border-line bg-panel px-2 py-1.5"
    >
      <span class={CHIP}><Icon name="party" />{party.members.length}</span>
      {#if party.joinCode}<span class="font-bold tracking-[0.08em]">{party.joinCode}</span>{/if}

      {#if party.queueing}
        <span class="font-semibold text-rust-text">{party.queue}</span>
        {#if party.matchmakingSeconds !== null}
          <span
            class="tabular-nums text-dim"
            title="Time since matchmaking began; may span more than one queue"
            >{mmss(party.matchmakingSeconds)}</span
          >
        {/if}
      {:else}
        <span class="text-dim">not queueing</span>
      {/if}

      <span class="flex-1"></span>
      <span class="text-dim">{party.region} · {party.preference}</span>

      {#if party.buildMismatch}
        <span
          class="font-semibold text-bad"
          title="Members are on different game builds, which blocks queueing">build mismatch</span
        >
      {/if}
    </section>

    {#if !match.inMatch}
      <!-- Outside a match the party roster is all there is, and it carries the ranks. The
           rows are taller now that they carry card art, and the window goes down to 240px
           high, so this scrolls rather than clipping a member off the bottom. -->
      <ul
        class="flex min-h-0 flex-col divide-y divide-line overflow-y-auto rounded-none border border-line"
      >
        {#each party.members as m (m.accountId)}
          <li class="flex min-w-0 items-center gap-[7px] bg-panel px-2 py-1.5">
            <!-- A rank the reader never got is a dash and looks like one: no badge, no
                 box, so it cannot be mistaken for a real placement. -->
            {#if m.rank}
              <span class="text-mini rounded-none border border-line bg-panel-2 px-1.5 font-bold"
                >{m.rank}</span
              >
            {:else}
              <span class="text-mini text-dim">—</span>
            {/if}
            <span class="overflow-hidden text-ellipsis whitespace-nowrap font-semibold">{m.name}</span
            >
            {#if m.isCreator}
              <span
                class="text-micro rounded-none border border-line px-1 tracking-[0.06em] text-muted uppercase"
                >leader</span
              >
            {/if}
            {#if m.ready}
              <span
                class="text-micro rounded-none border border-good/45 px-1 tracking-[0.06em] text-good uppercase"
                >ready</span
              >
            {/if}
            <span class="flex-1"></span>
            <!-- Card art rather than a comma-joined list: this is the one place with the
                 vertical room for it, and a picture of the hero is what a player actually
                 recognises. `None` and an empty roster mean the same thing here - not
                 queued as anything - so both say so in words rather than showing nothing. -->
            {#if m.queuedAs.length > 0}
              <span class="flex flex-none items-start gap-[5px]">
                {#each m.queuedAs.slice(0, 4) as h (h.heroId)}
                  <span class="flex w-[28px] flex-col items-center gap-[2px]" title={h.name}>
                    <HeroArt
                      name={h.name}
                      heroId={h.heroId}
                      portrait={h.portrait}
                      card={h.card}
                      variant="card"
                      size={28}
                    />
                    <span
                      class="text-nano max-w-full overflow-hidden text-ellipsis whitespace-nowrap text-muted"
                      style="line-height: 1.1">{h.name}</span
                    >
                  </span>
                {/each}
                {#if m.queuedAs.length > 4}
                  <span class="text-micro self-center text-dim" title="{m.queuedAs.length - 4} more">
                    +{m.queuedAs.length - 4}
                  </span>
                {/if}
              </span>
            {:else}
              <span
                class="text-mini max-w-[55%] overflow-hidden text-ellipsis whitespace-nowrap text-dim"
                >no hero preference</span
              >
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  {/if}

  {#if match.inMatch}
    <BanStrip bans={match.bans} readable={match.bansReadable} />
    <!-- Two columns while there is room, stacked when the window is made narrow. The
         `min()` is what keeps 300px from being a floor the track cannot go under: at a
         340px window the column collapses to the width available instead of overflowing
         it. -->
    <div
      class="grid min-h-0 flex-1 grid-cols-[repeat(auto-fit,minmax(min(300px,100%),1fr))] content-start gap-2 overflow-y-auto"
    >
      <TeamPanel team={amber} players={amberPlayers} accent="var(--color-amber)" />
      <TeamPanel team={sapphire} players={sapphirePlayers} accent="var(--color-sapphire)" />
    </div>
    <ObjectivePanel
      objectives={match.objectives}
      streetBrawl={match.status.includes('Street Brawl')}
    />
    <!-- Overlaid rather than in the flow: a swap is worth a glance and nothing more, and
         a popup that reflowed the scoreboard would cost more attention than it is worth. -->
    <SwapToasts swaps={match.heroSwaps} />
  {:else if !party}
    <div class="flex flex-1 flex-col items-center justify-center gap-1.5 text-muted">
      <p>{match.attached ? 'Waiting for a match.' : 'Waiting for Deadlock.'}</p>
      {#if match.error}<p class="text-mini text-bad">{match.error}</p>{/if}
      <p class="text-mini text-dim">Read-only. Nothing here is hidden from you in game.</p>
    </div>
  {/if}
</main>
