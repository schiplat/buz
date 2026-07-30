/**
 * Parse a UTC timestamp string into milliseconds since epoch.
 *
 * Handles formats produced by Rust `chrono` (ISO 8601 with up to 6-digit
 * fractional seconds) and SQLite `datetime('now')` (space-separated, no
 * timezone). JavaScript `Date.parse` only supports up to 3 fractional
 * digits, so microseconds must be truncated.
 *
 * Timestamps without an explicit offset are treated as UTC (Manager/SQLite
 * convention), then callers format into the **browser local** timezone.
 */
export function parseUtc(ts: string): number {
  // Normalize SQLite "2026-07-28 06:17:56" → ISO "2026-07-28T06:17:56"
  let iso = ts.trim().replace(' ', 'T')
  // Truncate fractional seconds to 3 digits (milliseconds)
  // chrono may produce "2026-07-28T06:17:56.950846Z" → "2026-07-28T06:17:56.950Z"
  iso = iso.replace(/\.(\d{1,3})\d*/, '.$1')
  // Append 'Z' if no timezone suffix (Z / +HH:MM / -HH:MM)
  if (!/[Zz]$|[+\-]\d{2}:\d{2}$/.test(iso)) {
    iso += 'Z'
  }
  const ms = Date.parse(iso)
  if (isNaN(ms)) {
    console.warn('[parseUtc] Failed to parse timestamp:', ts, '→', iso)
    return Date.now()
  }
  return ms
}

function pad2(n: number): string {
  return String(n).padStart(2, '0')
}

/**
 * `HH:MM:SS` in the **browser's local timezone** for log line prefixes.
 *
 * Server stores UTC; we convert so a user in CST sees local wall time without
 * a settings picker (follows `Intl` / OS timezone of the browser).
 */
export function formatLogClock(ts?: string | null, fallback = ''): string {
  if (ts == null || !String(ts).trim()) return fallback
  const d = new Date(parseUtc(String(ts)))
  if (isNaN(d.getTime())) return fallback
  return `${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}`
}

/**
 * Local `YYYY-MM-DD HH:MM:SS` for UI metadata (job created/ended, lists).
 */
export function formatLocalDateTime(ts?: string | null, fallback = '—'): string {
  if (ts == null || !String(ts).trim()) return fallback
  const d = new Date(parseUtc(String(ts)))
  if (isNaN(d.getTime())) return fallback
  return (
    `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())} ` +
    `${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}`
  )
}

/**
 * Compact local `MM-DD HH:MM:SS` for dense lists.
 */
export function formatLocalShort(ts?: string | null, fallback = '—'): string {
  if (ts == null || !String(ts).trim()) return fallback
  const d = new Date(parseUtc(String(ts)))
  if (isNaN(d.getTime())) return fallback
  return (
    `${pad2(d.getMonth() + 1)}-${pad2(d.getDate())} ` +
    `${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}`
  )
}
