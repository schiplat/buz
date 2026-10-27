/** Structured pool problems — one entry per distinct issue.
 *
 * The pool card used to concatenate these into a single run-on sentence
 * ("… failed to download — the file is missing on the source. Contact the
 * source administrator to publish it, then re-sync."). Splitting each issue
 * into `summary` (what is wrong) and `hint` (what to do) lets the UI weight
 * them differently, keeps unrelated problems from being read as one
 * sentence, and gives the wording a single home instead of being rebuilt in
 * a template expression.
 */

import type { PoolEntry } from '@/api/pluginSources'

export type NoticeTone = 'warn' | 'danger'

export interface PoolNotice {
  tone: NoticeTone
  /** What is wrong — the fact, with versions and versions-requirements. */
  summary: string
  /** What the curator can do about it. */
  hint: string
}

/** True when a version cannot be enabled on this Manager build. */
export const isStuck = (entry: PoolEntry) => entry.buz_compatible === false

const plural = (n: number) => (n > 1 ? '' : 's')
const names = (entries: PoolEntry[]) => entries.map((v) => `v${v.version}`).join(', ')

function requirement(entry: PoolEntry): string | null {
  return entry.compat_error?.match(/requires Buz (>=\s?\d+\.\d+\.\d+)/)?.[1] ?? null
}

function currentVersion(entry: PoolEntry): string | null {
  return entry.compat_error?.match(/current (\d+\.\d+\.\d+)/)?.[1] ?? null
}

/**
 * Every problem worth surfacing for one plugin, in display order:
 *
 * 1. versions this Manager build is too old for,
 * 2. an unusable version that is still enabled (runs may fail),
 * 3. versions the source lists but does not actually serve.
 */
export function buildPoolNotices(versions: PoolEntry[]): PoolNotice[] {
  const stuck = versions.filter(isStuck)
  if (stuck.length === 0) return []

  const missing = stuck.filter((v) => v.compat_error?.includes('failed to download'))
  const mismatch = stuck.filter((v) => !v.compat_error?.includes('failed to download'))
  const notices: PoolNotice[] = []

  if (mismatch.length > 0) {
    const sample = mismatch[0]
    const required = requirement(sample)
    const current = currentVersion(sample)
    notices.push({
      tone: 'warn',
      summary:
        `${names(mismatch)} require${plural(mismatch.length)} Buz ${required ?? 'a newer build'}` +
        (current ? `, this instance runs ${current}` : ''),
      hint: 'Cannot be enabled on this Manager.',
    })
  }

  const stillEnabled = mismatch.filter((v) => v.enabled)
  if (stillEnabled.length > 0) {
    notices.push({
      tone: 'danger',
      summary:
        `${names(stillEnabled)} ${stillEnabled.length > 1 ? 'are' : 'is'} enabled but cannot ` +
        'run on this Manager',
      hint: `Disable ${stillEnabled.length > 1 ? 'them' : 'it'} or upgrade Buz — new runs may fail.`,
    })
  }

  if (missing.length > 0) {
    notices.push({
      tone: 'warn',
      summary:
        `${names(missing)} ${missing.length > 1 ? 'are' : 'is'} listed in the source index but ` +
        'failed to download',
      hint:
        'The file is missing on the source — contact the source administrator to publish it, ' +
        'then re-sync.',
    })
  }

  return notices
}
