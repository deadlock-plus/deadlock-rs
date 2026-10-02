<script lang="ts">
  import HeroArt from './HeroArt.svelte';
  import type { HeroRef } from './types';

  let { bans, readable }: { bans: HeroRef[]; readable: boolean } = $props();

  // Three states, not two. An unread ban vector and a mode that bans nothing both arrive
  // as an empty list, and drawing them the same way would tell a viewer mid-draft that the
  // bans had been cleared. `readable` is what separates them; see `MatchView::bans` on the
  // Rust side.
  const state = $derived(!readable ? 'unread' : bans.length === 0 ? 'none' : 'some');
</script>

{#if state !== 'none'}
  <!-- A ruled strip rather than a floating card: it is a header band over the two panels
       below it, and a 1px rule says that more plainly than a rounded box would. -->
  <div class="flex flex-wrap items-center gap-x-1.5 gap-y-1 border-b border-line px-1 py-1">
    <span class="text-nano tracking-[0.08em] text-muted uppercase">bans</span>
    {#if state === 'unread'}
      <span class="text-dim" title="the ban list did not read">–</span>
    {:else}
      {#each bans as hero (hero.heroId)}
        <!-- Portrait plus name rather than name alone: the portrait is what a player
             recognises at a glance, and the name is what survives when there is no art to
             show. Desaturated, not dimmed - a ban still has to be recognisable, so the bar
             and the strikethrough carry the "banned" and the art only loses its colour. -->
        <span
          class="relative inline-flex items-center gap-1 rounded-none border border-line bg-panel-2 py-[2px] pr-1.5 pl-[2px] [&_.art]:brightness-90 [&_.art]:grayscale-[0.7]"
          title="{hero.name} — banned"
        >
          <HeroArt name={hero.name} heroId={hero.heroId} portrait={hero.portrait} size={22} />
          <!-- The bar sits over the portrait, so a ban stays a ban even where the
               strikethrough on the name is too fine to notice at a glance. -->
          <span
            class="pointer-events-none absolute top-1/2 left-[2px] h-[2px] w-[22px] -translate-y-1/2 rotate-[-38deg] rounded-none bg-bad"
          ></span>
          <span class="text-micro whitespace-nowrap text-muted line-through decoration-bad"
            >{hero.name}</span
          >
        </span>
      {/each}
    {/if}
  </div>
{/if}
