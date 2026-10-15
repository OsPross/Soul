/** Local playback-progress model: anchor from the OS timeline, interpolate between resyncs. */

/** Drift between local interpolation and the source beyond which we resync. */
export const RESYNC_DRIFT_MS = 2000;

export interface TimelineSource {
  title: string;
  artist: string;
  playing: boolean;
  positionMs: number;
  durationMs: number;
  /** Unix ms when `positionMs` was sampled (GSMTC LastUpdatedTime). */
  positionUpdatedAtMs?: number;
  playbackRate?: number;
}

export interface ProgressAnchor {
  trackKey: string;
  /** Position at `atMs`. */
  positionMs: number;
  /** Unix ms the position is valid for. */
  atMs: number;
  durationMs: number;
  playing: boolean;
  rate: number;
  /** Bumped on every resync; the display may only go backwards when this changes. */
  epoch: number;
}

export function trackKeyOf(np: Pick<TimelineSource, "title" | "artist" | "durationMs">): string {
  return `${np.title}|${np.artist}|${np.durationMs}`;
}

function rateOf(np: TimelineSource): number {
  return np.playbackRate && np.playbackRate > 0 ? np.playbackRate : 1;
}

function clamp(ms: number, durationMs: number): number {
  const m = Math.max(0, ms);
  return durationMs > 0 ? Math.min(m, durationMs) : m;
}

/** Position (ms) for an anchor at wall-clock `now`: pos + elapsed * rate while playing. */
export function positionAt(a: ProgressAnchor, now: number): number {
  const elapsed = a.playing ? Math.max(0, now - a.atMs) * a.rate : 0;
  return clamp(a.positionMs + elapsed, a.durationMs);
}

/** Where the OS says playback is at `now` (its sample extrapolated forward). */
function sourceAt(np: TimelineSource, now: number): number {
  const sampledAt = Math.min(np.positionUpdatedAtMs || now, now);
  const elapsed = np.playing ? (now - sampledAt) * rateOf(np) : 0;
  return clamp(np.positionMs + elapsed, np.durationMs);
}

function fresh(np: TimelineSource, now: number, epoch: number): ProgressAnchor {
  return {
    trackKey: trackKeyOf(np),
    // Anchor on the source sample itself so the extrapolation uses LastUpdatedTime.
    positionMs: np.positionMs,
    atMs: Math.min(np.positionUpdatedAtMs || now, now),
    durationMs: np.durationMs,
    playing: np.playing,
    rate: rateOf(np),
    epoch,
  };
}

/**
 * Fold a new OS sample into the anchor. Resync (new epoch) only on track change,
 * play/pause change, or >2 s drift (covers seeks); otherwise keep the local anchor
 * so the displayed time just keeps ticking.
 */
export function reconcile(
  prev: ProgressAnchor | null,
  np: TimelineSource,
  now: number,
): ProgressAnchor {
  if (!prev) return fresh(np, now, 1);
  if (prev.trackKey !== trackKeyOf(np) || prev.playing !== np.playing) {
    return fresh(np, now, prev.epoch + 1);
  }
  if (Math.abs(positionAt(prev, now) - sourceAt(np, now)) > RESYNC_DRIFT_MS) {
    return fresh(np, now, prev.epoch + 1);
  }
  const rate = rateOf(np);
  if (rate !== prev.rate) {
    // Re-base locally so a rate change doesn't shift the displayed time.
    return { ...prev, positionMs: positionAt(prev, now), atMs: now, rate, durationMs: np.durationMs };
  }
  return prev.durationMs === np.durationMs ? prev : { ...prev, durationMs: np.durationMs };
}

/** Never show time going backwards within one epoch (jitter between samples). */
export function monotonic(
  last: { epoch: number; ms: number },
  anchor: ProgressAnchor,
  now: number,
): number {
  const ms = positionAt(anchor, now);
  if (last.epoch === anchor.epoch && ms < last.ms) return last.ms;
  last.epoch = anchor.epoch;
  last.ms = ms;
  return ms;
}
