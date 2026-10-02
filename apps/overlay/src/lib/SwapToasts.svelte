<script lang="ts">
  /**
   * Transient popups for mid-match hero swaps.
   *
   * # The transient problem, and where it is solved
   *
   * A swap exists for one tick on the reader's side and then the world simply looks like a
   * player on a different hero. This window is on the far side of an event bridge it can
   * miss frames of — it is not listening until it mounts, and it drops what arrives while
   * it is busy — so the reader does not fire a swap once. It carries each one on
   * `MatchView.heroSwaps` for about ten seconds and re-sends it on every frame in that
   * window (see `SwapLog` in `model.rs`).
   *
   * That makes "the list changed" meaningless here: the same swap arrives forty times. The
   * only thing that separates a re-send from a second swap is `seq`, which is stable while
   * a swap is carried and never reused, so this keeps a high-water mark and shows each swap
   * exactly once however many frames it sees it on. A swap that lands between two frames is
   * still in the list on the next one; a swap already shown never comes back.
   *
   * `seen` is a plain variable rather than `$state` on purpose: it is a watermark the
   * effect both reads and writes, and making it reactive would have the effect invalidate
   * itself every time it ran.
   */
  import { untrack } from 'svelte';
  import HeroArt from './HeroArt.svelte';
  import type { HeroSwapView } from './types';

  let { swaps }: { swaps: HeroSwapView[] } = $props();

  /** How long one popup stays up. Long enough to read, short enough to be gone. */
  const LIFETIME = 5500;
  /** Most popups stacked at once; beyond this the oldest is retired early. */
  const STACK = 3;

  type Toast = HeroSwapView & { key: number };

  let shown = $state<Toast[]>([]);
  const timers = new Map<number, ReturnType<typeof setTimeout>>();
  let seen = 0;

  function retire(key: number) {
    const timer = timers.get(key);
    if (timer !== undefined) {
      clearTimeout(timer);
      timers.delete(key);
    }
    shown = untrack(() => shown).filter((t) => t.key !== key);
  }

  $effect(() => {
    // `swaps` is the only intended dependency. Every read of `shown` below is untracked,
    // because an effect that depended on the list it appends to would re-run itself on
    // every append - harmless here only by accident of the `seq` watermark, and not worth
    // resting on.
    const fresh = swaps.filter((s) => s.seq > seen);
    if (fresh.length === 0) return;
    seen = Math.max(seen, ...fresh.map((s) => s.seq));

    untrack(() => {
      let next = shown;
      for (const s of fresh) {
        next = [...next, { ...s, key: s.seq }];
        timers.set(
          s.seq,
          setTimeout(() => retire(s.seq), LIFETIME)
        );
      }
      // Newest wins the space. Retiring from the front rather than refusing the new one:
      // the swap somebody just made is the one worth reading.
      for (const dropped of next.slice(0, Math.max(0, next.length - STACK))) {
        clearTimeout(timers.get(dropped.key));
        timers.delete(dropped.key);
      }
      shown = next.slice(-STACK);
    });
  });

  $effect(() => () => {
    timers.forEach(clearTimeout);
    timers.clear();
  });

  /**
   * Who swapped, in whatever terms the reader could actually establish.
   *
   * Four states, not one: a name, a slot, both, or neither. `null` slot is not slot 0 and
   * `null` name is not an empty name, so neither is drawn as a blank — a swap the reader
   * could not attribute says that in words instead.
   */
  function who(s: HeroSwapView): string {
    if (s.player !== null && s.slot !== null) return `Slot ${s.slot} · ${s.player}`;
    if (s.player !== null) return s.player;
    if (s.slot !== null) return `Slot ${s.slot}`;
    return 'unidentified player';
  }

  const unattributed = (s: HeroSwapView) => s.player === null && s.slot === null;

  // An admitted gap, not a name. Dimmed and italic so it cannot be mistaken for a player
  // actually called that.
  const whoTone = (s: HeroSwapView) => (unattributed(s) ? 'text-dim italic' : 'text-muted');
</script>

{#if shown.length > 0}
  <!-- Bottom-right and click-through. The panels are read at a glance mid-game, so a
       popup gets the corner with the least in it and never takes a click. -->
  <div
    class="pointer-events-none fixed right-2.5 bottom-2.5 z-10 flex max-w-[min(280px,calc(100%-20px))] flex-col items-end gap-[5px]"
    role="status"
    aria-live="polite"
  >
    {#each shown as s (s.key)}
      <!-- Squared, ruled, and edged in the accent on the leading side. The 2px accent bar
           plus a tight shadow is what separates it from a scoreboard row of nearly the
           same colour - it is the one thing here laid over other content, so it needs to
           read as above the panel rather than part of it. -->
      <div
        class="animate-toast-in flex min-w-0 items-center gap-2 rounded-none border border-line border-l-2 border-l-rust bg-panel-2 py-[5px] pr-2 pl-1.5 shadow-[0_2px_6px_rgb(0_0_0/0.55)] motion-reduce:animate-none"
      >
        <span class="flex flex-none items-center gap-[3px]">
          <!-- The hero left behind is faded, the one taken is not, so the direction of the
               swap reads before the words do. Both go through the shared art component, so
               a hero with no published art still arrives as its initials rather than as an
               empty square. -->
          <HeroArt
            name={s.from.name}
            heroId={s.from.heroId}
            portrait={s.from.portrait}
            size={22}
            faded
          />
          <span class="text-mini leading-none text-icon" aria-hidden="true">→</span>
          <HeroArt name={s.to.name} heroId={s.to.heroId} portrait={s.to.portrait} size={22} />
        </span>
        <span class="flex min-w-0 flex-col gap-px">
          <span
            class="text-micro overflow-hidden text-ellipsis whitespace-nowrap {whoTone(s)}"
            >{who(s)}</span
          >
          <span class="text-tiny flex min-w-0 items-baseline gap-1">
            <span class="overflow-hidden text-ellipsis whitespace-nowrap text-muted"
              >{s.from.name}</span
            >
            <span class="flex-none text-icon" aria-hidden="true">→</span>
            <span class="overflow-hidden text-ellipsis whitespace-nowrap font-semibold"
              >{s.to.name}</span
            >
          </span>
        </span>
      </div>
    {/each}
  </div>
{/if}
