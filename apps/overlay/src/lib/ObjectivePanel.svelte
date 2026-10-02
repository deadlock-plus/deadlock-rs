<script lang="ts">
  import type { ObjectivesView } from './types';

  let {
    objectives,
    streetBrawl = false
  }: { objectives: ObjectivesView; streetBrawl?: boolean } = $props();

  // A dash, never a zero. Most of these are absent because the client is never told them,
  // which is a different fact from a timer having expired, and rendering both the same way
  // invites the reader to act on a countdown that does not exist.
  const shown = (v: string | null) => v ?? '–';
</script>

<!-- Ruled off the scoreboard above rather than boxed: it is the last band of the same
     table, not a separate card. -->
<div class="text-micro flex flex-col gap-[2px] border-t border-line px-1.5 py-1">
  <div class="flex items-center gap-1.5">
    <span class="text-nano min-w-[46px] tracking-[0.04em] text-dim uppercase">midboss</span>
    {#if objectives.midbossHealth}
      <span class="tabular-nums">{objectives.midbossHealth}</span>
      {#if objectives.midbossFraction !== null}
        <span class="h-[4px] max-w-[78px] flex-1 overflow-hidden rounded-none bg-white/12"
          ><span
            class="block h-full bg-white/50"
            style:width="{objectives.midbossFraction * 100}%"
          ></span></span
        >
      {/if}
    {:else}
      <span class="tabular-nums text-dim">not on the map</span>
      <span class="tabular-nums">respawn {shown(objectives.midbossRespawn)}</span>
    {/if}
  </div>

  <div class="flex items-center gap-1.5">
    <span class="text-nano min-w-[46px] tracking-[0.04em] text-dim uppercase">rift</span>
    {#if objectives.riftContested}
      <span class="font-semibold tabular-nums text-rust-text">contested</span>
    {:else}
      <span class="tabular-nums text-dim">quiet</span>
    {/if}
    {#if objectives.riftProgress !== null}
      <span class="h-[4px] max-w-[78px] flex-1 overflow-hidden rounded-none bg-white/12"
        ><span class="block h-full bg-rust" style:width="{objectives.riftProgress * 100}%"></span
        ></span
      >
    {/if}
    {#if objectives.riftGiveUp}
      <span class="tabular-nums">ends {objectives.riftGiveUp}</span>
    {/if}
  </div>

  <div class="flex items-center gap-1.5">
    <span class="text-nano min-w-[46px] tracking-[0.04em] text-dim uppercase">urn</span>
    <span class="tabular-nums" class:text-dim={objectives.urnCount === 0}
      >{objectives.urnCount} on map</span
    >
    <!-- The only schedule-derived timer here, marked so it is not mistaken for a read one. -->
    <span
      class="tabular-nums text-dim"
      title="estimated from the spawn cadence, not read from the game">buffs ~{shown(objectives.bridgeBuff)}</span
    >
  </div>

  {#if streetBrawl && objectives.brawlPhase}
    <div class="flex items-center gap-1.5">
      <span class="text-nano min-w-[46px] tracking-[0.04em] text-dim uppercase">brawl</span>
      <span class="tabular-nums">{objectives.brawlPhase}</span>
      {#if objectives.brawlRound !== null}<span class="tabular-nums"
          >round {objectives.brawlRound}</span
        >{/if}
      <span class="tabular-nums">next {shown(objectives.brawlNextPhase)}</span>
    </div>
  {/if}
</div>
