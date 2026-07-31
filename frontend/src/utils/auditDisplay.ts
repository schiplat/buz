/** Convert datetime-local value to SQLite-friendly bound for audit log filters. */
export function toSqliteDateBound(local: string): string | undefined {
  const t = local.trim()
  if (!t) return undefined
  return t.includes('T') ? t.replace('T', ' ') : t
}

export function formatAuditJson(v: unknown): string {
  if (v == null) return '—'
  try {
    return JSON.stringify(v, null, 2)
  } catch {
    return String(v)
  }
}
