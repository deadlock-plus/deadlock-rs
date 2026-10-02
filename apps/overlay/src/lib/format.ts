/** Compact a count so a column never changes width mid-fight. */
export function short(n: number | null | undefined): string {
  if (n === null || n === undefined) return '-';
  const v = Number(n);
  if (!Number.isFinite(v)) return '-';
  if (Math.abs(v) >= 1_000_000) return (v / 1_000_000).toFixed(1) + 'M';
  if (Math.abs(v) >= 10_000) return Math.round(v / 1000) + 'k';
  if (Math.abs(v) >= 1_000) return (v / 1000).toFixed(1) + 'k';
  return String(v);
}

/** Seconds to `m:ss`, for a queue that has been running a while. */
export function mmss(secs: number | null | undefined): string {
  if (secs === null || secs === undefined) return '';
  const s = Math.max(0, Math.floor(secs));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

/** Signed souls difference, e.g. `+3.3k`, for the side that is ahead. */
export function lead(n: number | null | undefined): string {
  if (!n) return '';
  return (n > 0 ? '+' : '−') + short(Math.abs(n));
}
