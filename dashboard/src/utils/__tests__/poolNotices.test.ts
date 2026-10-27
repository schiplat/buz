import { describe, expect, it } from 'vitest'
import { buildPoolNotices } from '../poolNotices'
import type { PoolEntry } from '@/api/pluginSources'

const entry = (overrides: Partial<PoolEntry>): PoolEntry => ({
  namespace: 'alice',
  step_id: 'oss-upload',
  version: '1.0.0',
  sha256: 'a'.repeat(64),
  source_id: 'src-1',
  source_name: 'alice',
  file_path: 'steps/oss-upload/1.0.0.yaml',
  enabled: false,
  reviewed: true,
  added_by: 'sync',
  updated_by: null,
  updated_at: null,
  requires_buz: null,
  buz_compatible: null,
  compat_error: null,
  meta: null,
  ...overrides,
})

describe('buildPoolNotices', () => {
  it('says nothing when every version is usable', () => {
    expect(buildPoolNotices([entry({ buz_compatible: true })])).toEqual([])
  })

  it('splits the requirement mismatch into fact and remedy', () => {
    const notices = buildPoolNotices([
      entry({
        version: '1.1.0',
        requires_buz: '>=99.0.0',
        buz_compatible: false,
        compat_error:
          "step spec 'alice/oss-upload' is invalid: requires_buz: step requires Buz >= 99.0.0 — this plugin is too new for this Manager (current 1.10.5)",
      }),
    ])
    expect(notices).toHaveLength(1)
    expect(notices[0].tone).toBe('warn')
    expect(notices[0].summary).toBe(
      'v1.1.0 requires Buz >= 99.0.0, this instance runs 1.10.5',
    )
    expect(notices[0].hint).toBe('Cannot be enabled on this Manager.')
    // A newer Buz is not something refreshing can fix.
    expect(notices[0].action).toBeUndefined()
  })

  it('separates "cannot run" from "but it is still enabled"', () => {
    const notices = buildPoolNotices([
      entry({ version: '1.0.0', enabled: true, buz_compatible: false }),
    ])
    expect(notices.map((n) => n.tone)).toEqual(['warn', 'danger'])
    expect(notices[1].summary).toBe('v1.0.0 is enabled but cannot run on this Manager')
    expect(notices[1].hint).toContain('Disable it or upgrade Buz')
  })

  it('groups versions the source fails to serve and points at the source', () => {
    const notices = buildPoolNotices([
      entry({ version: '1.0.0', buz_compatible: false, compat_error: 'failed to download: 404' }),
      entry({ version: '1.1.0', buz_compatible: false, compat_error: 'failed to download: 404' }),
    ])
    expect(notices).toHaveLength(1)
    expect(notices[0].summary).toBe(
      'v1.0.0, v1.1.0 are listed in the source index but failed to download',
    )
    expect(notices[0].hint).toContain('publish it, then')
    // Only this kind is fixable by re-fetching, so it is the only one with an action.
    expect(notices[0].action).toBe('refresh')
  })

  it('pluralises correctly for a single version', () => {
    const notices = buildPoolNotices([
      entry({ version: '1.0.0', buz_compatible: false, compat_error: 'failed to download: 404' }),
    ])
    expect(notices[0].summary).toBe(
      'v1.0.0 is listed in the source index but failed to download',
    )
  })
})
