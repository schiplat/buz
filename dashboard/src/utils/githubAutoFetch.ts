/**
 * Render helpers for the GitHub Sync connections table's Auto-fetch column.
 *
 * Auto-fetch is only effective when BOTH an interval and at least one owner
 * are configured (the backend scheduler requires `fetch_owners != ''`), so a
 * set interval without owners is surfaced as an explicit hint instead of a
 * plain "Off".
 */

export interface AutoFetchStatus {
  /** Badge text like "60m". Null when auto-fetch is fully disabled. */
  badge: string | null
  /** Detail after the badge: owner list, or a "no owners" hint. */
  detail: string | null
  /** True when the detail is a warning that needs owner input. */
  warn: boolean
}

export function autoFetchStatus(
  intervalMinutes: number | null | undefined,
  owners: string | null | undefined,
): AutoFetchStatus {
  if (intervalMinutes == null) {
    return { badge: null, detail: null, warn: false }
  }
  const list = (owners ?? '')
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean)
  const badge = `${intervalMinutes}m`
  if (list.length === 0) {
    return { badge, detail: 'no owners — add Fetch Owners to enable', warn: true }
  }
  const shown = list.slice(0, 2).join(', ')
  return { badge, detail: list.length > 2 ? `${shown}…` : shown, warn: false }
}
