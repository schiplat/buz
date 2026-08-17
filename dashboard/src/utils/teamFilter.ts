/**
 * Client-side team scope for list pages.
 *
 * - Empty `activeTeamId` → "All my teams" (no extra filter).
 * - Set `activeTeamId` → only resources belonging to that team.
 * - `includeShared: true` (Hosts) → also keep unscoped (`null`/`''`) rows.
 */
export function matchesTeamFilter(
  resourceTeamId: string | null | undefined,
  activeTeamId: string,
  opts?: { includeShared?: boolean },
): boolean {
  if (!activeTeamId) return true
  const tid = (resourceTeamId ?? '').trim()
  if (!tid) {
    return opts?.includeShared === true
  }
  return tid === activeTeamId
}
