<script lang="ts">
  /**
   * Hero art, with a stand-in that is the default rather than the accident.
   *
   * The art the catalog publishes lives on `assets-bucket.deadlock-api.com`, so it needs
   * the network. `tauri.conf.json` allows that one host in `img-src` and nothing else,
   * which means the picture is a bonus and not a dependency: with no network, a first run
   * offline, or a hero the catalog has no art for, this draws a deliberate marker instead.
   *
   * Three states, told apart on purpose:
   *
   * - **unknown hero** (`heroId === null`): the row's hero id did not read. Drawn as a
   *   neutral `?`, never as initials, because inventing letters for a hero nobody
   *   identified is worse than admitting the gap.
   * - **known hero, no art**: initials on a colour derived from the id, so the same hero
   *   is the same swatch every match. Four of the bundled catalog's heroes genuinely have
   *   no published art, so this is a real state and not a fallback nobody reaches.
   * - **art loaded**: the picture, over the same swatch.
   *
   * The swatch renders first and the image on top of it, so a slow or failed load shows
   * the marker rather than a broken-image glyph - `onerror` then pins the failure so the
   * `<img>` is dropped for good.
   */
  let {
    name,
    heroId,
    portrait = null,
    card = null,
    variant = 'portrait',
    size = 30,
    accent,
    faded = false
  }: {
    /** Display name, as the reader resolved it. `-` when the hero is unknown. */
    name: string;
    /** `null` when the hero id itself did not read - a different fact from missing art. */
    heroId: number | null;
    portrait?: string | null;
    card?: string | null;
    /** `portrait` is the square scoreboard art; `card` is the taller illustration. */
    variant?: 'portrait' | 'card';
    /** Width in px. A card is 4:3 taller than it is wide; a portrait is square. */
    size?: number;
    /** Team colour, drawn as the ring, so a portrait never floats free of its side. */
    accent?: string;
    /** Dead, banned, or otherwise not currently a live thing. */
    faded?: boolean;
  } = $props();

  const known = $derived(heroId !== null);

  // Each variant prefers its own art and accepts the other rather than giving up: a
  // portrait scaled into a card slot still identifies the hero, which an empty box does
  // not.
  const src = $derived(variant === 'card' ? (card ?? portrait) : (portrait ?? card));

  // Pinning the URL rather than a boolean: a row is recycled across heroes, and a plain
  // `failed = true` would follow the component onto the next hero and suppress art that
  // would have loaded fine.
  let failedSrc = $state<string | null>(null);
  const showImage = $derived(!!src && failedSrc !== src);

  /**
   * Up to two letters, from the words of the name.
   *
   * `Lady Geist` gives `LG` and `Infernus` gives `IN`, so two heroes rarely collide - and
   * where they do, the colour separates them.
   */
  const initials = $derived.by(() => {
    const words = name.trim().split(/[\s'’._-]+/u).filter(Boolean);
    if (words.length === 0) return '?';
    if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
    return (words[0][0] + words[1][0]).toUpperCase();
  });

  // The golden angle spreads consecutive ids far apart in hue, so neighbouring heroes on
  // a scoreboard never land on the same swatch.
  const hue = $derived(known ? Math.round((heroId! * 137.508) % 360) : 0);

  // Resolved here rather than as competing utility classes: two `background-color` rules
  // would be settled by stylesheet order instead of by which state is true.
  const swatchFill = $derived(known ? `hsl(${hue} 40% 20%)` : 'var(--color-panel-2)');
  const swatchInk = $derived(known ? `hsl(${hue} 70% 80%)` : 'var(--color-dim)');

  const shape = $derived(variant === 'card' ? 'aspect-[3/4]' : 'aspect-square');
  const dead = $derived(faded ? 'grayscale-[0.85] opacity-45' : '');
  const weight = $derived(known ? 'font-extrabold' : 'font-bold');

  const label = $derived(known ? name : 'hero did not read');
</script>

<!-- Squared off, like every other box here. The ring is the team colour and is drawn as
     an inset shadow rather than a border so it never steals a pixel from the art. -->
<span
  class="art relative block w-[var(--size)] flex-none overflow-hidden rounded-none shadow-[inset_0_0_0_1px_var(--ring)] {shape} {dead}"
  title={label}
  style:--size="{size}px"
  style:--ring={accent ?? 'var(--color-line)'}
  style:background={swatchFill}
>
  <!-- Scales with the box rather than the page: the same component is a 22px chip in a
       toast and a 30px portrait in a scoreboard row. -->
  <span
    class="absolute inset-0 grid place-items-center text-[calc(var(--size)*0.42)] tracking-[0.02em] select-none {weight}"
    style:color={swatchInk}
    aria-hidden="true">{known ? initials : '?'}</span
  >
  {#if showImage}
    <!-- Hero art is head-and-shoulders with room above; bias the crop upward so a square
         slot keeps the face. -->
    <img
      class="absolute inset-0 block h-full w-full object-cover object-[50%_22%]"
      src={src}
      alt=""
      loading="eager"
      decoding="async"
      draggable="false"
      referrerpolicy="no-referrer"
      onerror={() => (failedSrc = src)}
    />
  {/if}
</span>
