/**
 * Pure helpers for YAML → form name resolution.
 * Kept separate so Import-to-Form refresh logic can be unit-tested without Vue.
 */

export function resolveNamesToIds(
  names: string[],
  available: { id: string; name: string }[],
): { ids: string[]; missing: string[] } {
  const ids: string[] = []
  const missing: string[] = []
  for (const name of names) {
    const found = available.find((s) => s.name === name)
    if (found) ids.push(found.id)
    else missing.push(name)
  }
  return { ids, missing }
}
