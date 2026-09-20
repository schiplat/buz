/**
 * Runner↔Manager clock skew presentation.
 *
 * The Runner validates its own clock against the Manager on startup (and
 * re-checks on every heartbeat) and reports the offset in `system_info`.
 * Positive = the Runner is behind the Manager.
 */

/** Warn tone once the Runner is ≥ 60s off (Runner `[clock] warn_threshold`). */
export const CLOCK_WARN_SECONDS = 60

/** Danger tone once the Runner is ≥ 300s off (Runner `[clock] max_skew`). */
export const CLOCK_DANGER_SECONDS = 300

export type ClockTone = 'ok' | 'warn' | 'danger'

export interface ClockSkewInfo {
  /** e.g. `+12s` (behind) / `-45s` (ahead). */
  text: string
  tone: ClockTone
  /** Tooltip-friendly explanation. */
  title: string
}

/**
 * Describe a reported skew, or `null` when the Runner has not measured one
 * (older Runner, or `[clock] enabled = false`).
 */
export function clockSkewInfo(skew: number | null | undefined): ClockSkewInfo | null {
  if (typeof skew !== 'number' || !Number.isFinite(skew)) return null

  const secs = Math.round(skew)
  const drift = Math.abs(secs)
  const tone: ClockTone =
    drift >= CLOCK_DANGER_SECONDS ? 'danger' : drift >= CLOCK_WARN_SECONDS ? 'warn' : 'ok'

  const direction = secs >= 0 ? 'behind' : 'ahead'
  const text = secs >= 0 ? `+${secs}s` : `-${Math.abs(secs)}s`
  const title =
    tone === 'ok'
      ? `Runner clock is ${drift}s ${direction} the Manager — within tolerance`
      : `Runner clock is ${drift}s ${direction} the Manager — sync NTP on the Runner host`

  return { text, tone, title }
}

/** Tailwind text class for a tone (matches the rest of the dashboard palette). */
export function clockToneClass(tone: ClockTone): string {
  if (tone === 'danger') return 'text-red-500'
  if (tone === 'warn') return 'text-amber-500'
  return ''
}
