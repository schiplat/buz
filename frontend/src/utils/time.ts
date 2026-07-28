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
