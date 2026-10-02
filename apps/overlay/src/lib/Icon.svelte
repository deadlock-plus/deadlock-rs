<script lang="ts">
  /**
   * Inline SVG so the overlay has no network dependency and no icon font to load.
   *
   * Solid silhouettes on a 24x24 grid, not outlines on a 16x16 one. The distinction is
   * the whole point: these are drawn at 11-14 CSS pixels, where a 1.6-wide stroke lands
   * on roughly one device pixel and half of it falls between pixels, so an outlined glyph
   * arrives as grey haze with no readable shape. A filled shape keeps its silhouette all
   * the way down, because the renderer has area to anti-alias rather than a hairline.
   *
   * The larger grid is for the same reason: it leaves room to draw a shape that survives
   * being reduced, instead of detail that dissolves.
   *
   * The clock is the one exception - a ring reads as a clock and a disc does not - so it
   * carries its own stroke, deliberately heavy enough to hold up.
   */
  type IconName =
    | 'kills'
    | 'deaths'
    | 'assists'
    | 'damage'
    | 'objective'
    | 'health'
    | 'souls'
    | 'rank'
    | 'healing'
    | 'party'
    | 'clock';

  let {
    name,
    size = 14,
    label
  }: {
    name: IconName;
    size?: number;
    /** Announced to a screen reader. Omit for an icon a neighbouring label already names. */
    label?: string;
  } = $props();

  // Raw markup rather than one `d`, because several glyphs are clearer as a couple of
  // primitives than as one path with the joins hand-written.
  const glyphs: Record<IconName, string> = {
    // Sword, point up: kills.
    kills:
      '<path d="M12 0.9 15.7 7v7.3H8.3V7L12 0.9Z"/>' +
      '<rect x="6" y="14.7" width="12" height="2.9" rx="1.45"/>' +
      '<rect x="10.4" y="17.6" width="3.2" height="2.5"/>' +
      '<rect x="8.4" y="20.1" width="7.2" height="2.9" rx="1.45"/>',
    // Skull with the eyes and nose knocked out: deaths.
    deaths:
      '<path fill-rule="evenodd" d="M12 1.6c-4.6 0-8.3 3.5-8.3 7.9 0 2.6 1.1 4.6 2.8 5.9v3.4c0 .9.8 1.7 1.7 1.7h7.6c.9 0 1.7-.8 1.7-1.7v-3.4c1.7-1.3 2.8-3.3 2.8-5.9 0-4.4-3.7-7.9-8.3-7.9Zm-3.2 6.4a2.1 2.1 0 1 1 0 4.2 2.1 2.1 0 0 1 0-4.2Zm6.4 0a2.1 2.1 0 1 1 0 4.2 2.1 2.1 0 0 1 0-4.2ZM12 13.6l1.5 2.8h-3L12 13.6Z"/>' +
      '<rect x="8.4" y="20.8" width="2.2" height="2.1" rx="1"/>' +
      '<rect x="13.4" y="20.8" width="2.2" height="2.1" rx="1"/>',
    // Two figures, the second half-behind the first: assists.
    assists:
      '<circle cx="9" cy="7.1" r="3.9"/>' +
      '<path d="M9 12.2c3.9 0 6.6 2.4 6.6 5.7v3.5H2.4v-3.5c0-3.3 2.7-5.7 6.6-5.7Z"/>' +
      '<circle cx="17.6" cy="8.4" r="3.1"/>' +
      '<path d="M17.2 14.2c2.8 0 4.4 1.8 4.4 4.4v2.8h-4.1v-3.5c0-1.5-.4-2.8-1.2-3.7h.9Z"/>',
    // Eight-point burst: hero damage.
    damage:
      '<path d="M12 1.4 13.68 7.93 19.5 4.5 16.07 10.32 22.6 12 16.07 13.68 19.5 19.5 13.68 16.07 12 22.6 10.32 16.07 4.5 19.5 7.93 13.68 1.4 12 7.93 10.32 4.5 4.5 10.32 7.93Z"/>',
    // Walled tower with a gate: objective damage.
    objective:
      '<path fill-rule="evenodd" d="M5.4 4h3.07v2.2h2V4h3.06v2.2h2V4h3.07v18H5.4V4Zm6.6 8.4a2.8 2.8 0 0 0-2.8 2.8V22h5.6v-6.8a2.8 2.8 0 0 0-2.8-2.8Z"/>',
    // Heart: health.
    health:
      '<path d="M12 21.8 4.6 14.7A5.6 5.6 0 0 1 12 6.6a5.6 5.6 0 0 1 7.4 8.1L12 21.8Z"/>',
    // Coin stack: souls.
    souls:
      '<ellipse cx="12" cy="5.4" rx="8.6" ry="3.4"/>' +
      '<path d="M3.4 8.6c1.7 1.6 4.9 2.6 8.6 2.6s6.9-1 8.6-2.6v3.6c0 1.9-3.9 3.4-8.6 3.4S3.4 14.1 3.4 12.2V8.6Z"/>' +
      '<path d="M3.4 14.4c1.7 1.6 4.9 2.6 8.6 2.6s6.9-1 8.6-2.6V18c0 1.9-3.9 3.4-8.6 3.4S3.4 19.9 3.4 18v-3.6Z"/>',
    // Shield with an upward chevron cut out: rank.
    rank: '<path fill-rule="evenodd" d="M12 1.3 3 4.7v6.9c0 5.3 3.8 9.9 9 11.1 5.2-1.2 9-5.8 9-11.1V4.7L12 1.3Zm0 4.1L17.4 12h-3.6v5.9h-3.6V12H6.6L12 5.4Z"/>',
    // Bold cross: healing.
    healing:
      '<rect x="9.2" y="2.2" width="5.6" height="19.6" rx="1.8"/>' +
      '<rect x="2.2" y="9.2" width="19.6" height="5.6" rx="1.8"/>',
    // Three figures: party.
    party:
      '<circle cx="12" cy="6.4" r="3.6"/>' +
      '<path d="M12 11.4c3.6 0 6.2 2.3 6.2 5.4v4.4H5.8v-4.4c0-3.1 2.6-5.4 6.2-5.4Z"/>' +
      '<circle cx="4.5" cy="9.2" r="2.7"/>' +
      '<circle cx="19.5" cy="9.2" r="2.7"/>' +
      '<path d="M4.6 13.1c.7 0 1.3.1 1.9.3a7.7 7.7 0 0 0-2.1 4.6v3.2H.6v-4.3c0-2.3 1.7-3.8 4-3.8Z"/>' +
      '<path d="M19.4 13.1c2.3 0 4 1.5 4 3.8v4.3h-3.8V18a7.7 7.7 0 0 0-2.1-4.6c.6-.2 1.2-.3 1.9-.3Z"/>',
    // Ring and hands: match time. Stroked on purpose - a filled disc is not a clock.
    clock:
      '<circle cx="12" cy="12" r="9.3" fill="none" stroke="currentColor" stroke-width="2.6"/>' +
      '<path d="M12 6.6v5.7l3.9 2.4" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/>'
  };
</script>

<svg
  class="shrink-0 [shape-rendering:geometricPrecision] [vertical-align:-0.14em]"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="currentColor"
  role={label ? 'img' : 'presentation'}
  aria-label={label}
  aria-hidden={label ? undefined : 'true'}
  focusable="false"
>
  {#if label}<title>{label}</title>{/if}
  <!-- eslint-disable-next-line svelte/no-at-html-tags -- fixed, in-module markup -->
  {@html glyphs[name] ?? ''}
</svg>
