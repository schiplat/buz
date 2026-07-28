/**
 * Parse a UTC timestamp string into milliseconds since epoch.
 *
 * Handles formats produced by Rust `chrono` (ISO 8601 with up to 6-digit
 * fractional seconds) and SQLite `datetime('now')` (space-separated, no
 * timezone). JavaScript `Date.parse` only supports up to 3 fractional
 * digits, so microseconds must be truncated.
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

/**
 * Extract `HH:MM:SS` for log line prefixes.
 *
 * Accepts SQLite `YYYY-MM-DD HH:MM:SS` and RFC3339. Falls back to `fallback`
 * (default empty) when the value is missing or unparseable — callers that
 * need a live clock should pass `nowTimeStr()`.
 */
export function formatLogClock(ts?: string | null, fallback = ''): string {
  if (ts == null || !String(ts).trim()) return fallback
  const m = String(ts).trim().match(/[T ](\d{2}:\d{2}:\d{2})/)
  if (m) return m[1]
  const d = new Date(parseUtc(String(ts)))
  const hh = String(d.getUTCHours()).padStart(2, '0')
  const mm = String(d.getUTCMinutes()).padStart(2, '0')
  const ss = String(d.getUTCSeconds()).padStart(2, '0')
  return `${hh}:${mm}:${ss}`
}
