/** One bind row that can be pushed (id + display name). */
export type PushableBind = {
  id: string
  owner: string
  repo: string
}

export type PushOneResult = {
  bind_id: string
  owner?: string
  repo?: string
  status: 'ok' | 'failed'
  error?: string
}

export type PushProgress = {
  /** 0-based index of the bind currently in flight (or just finished). */
  index: number
  total: number
  currentId: string
  /** How many binds have fully completed (ok or failed). */
  done: number
}

/**
 * Push each bind one-by-one so a single HTTP timeout covers only one GitHub
 * round-trip, not the whole fan-out. Continues after per-bind failures.
 */
export async function pushBindsSequentially(
  binds: PushableBind[],
  pushOne: (bind: PushableBind) => Promise<PushOneResult>,
  onProgress?: (p: PushProgress) => void,
): Promise<{ failures: string[] }> {
  const failures: string[] = []
  const total = binds.length
  for (let i = 0; i < total; i++) {
    const bind = binds[i]
    onProgress?.({ index: i, total, currentId: bind.id, done: i })
    try {
      const result = await pushOne(bind)
      if (result.status === 'failed') {
        const label = `${result.owner || bind.owner}/${result.repo || bind.repo}`
        failures.push(`${label} — ${result.error || 'unknown'}`)
      }
    } catch (e: unknown) {
      const err = e as { response?: { data?: { message?: string } }; message?: string }
      failures.push(
        `${bind.owner}/${bind.repo} — ${err?.response?.data?.message || err?.message || 'Push failed'}`,
      )
    }
    onProgress?.({ index: i, total, currentId: bind.id, done: i + 1 })
  }
  return { failures }
}
