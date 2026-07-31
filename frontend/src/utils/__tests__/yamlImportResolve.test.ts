import { describe, expect, it } from 'vitest'
import { resolveNamesToIds } from '@/utils/yamlImportResolve'

describe('resolveNamesToIds', () => {
  const available = [
    { id: '1', name: 'DOCKER_PASS' },
    { id: '2', name: 'GIT_SSH_KEY' },
  ]

  it('resolves known names to ids', () => {
    expect(resolveNamesToIds(['DOCKER_PASS', 'GIT_SSH_KEY'], available)).toEqual({
      ids: ['1', '2'],
      missing: [],
    })
  })

  it('reports missing names without inventing ids', () => {
    expect(resolveNamesToIds(['DOCKER_PASS', 'NEW_SECRET'], available)).toEqual({
      ids: ['1'],
      missing: ['NEW_SECRET'],
    })
  })

  it('sees newly added secrets when available list is refreshed', () => {
    const before = resolveNamesToIds(['NEW_SECRET'], available)
    expect(before.missing).toEqual(['NEW_SECRET'])

    const afterList = [...available, { id: '3', name: 'NEW_SECRET' }]
    const after = resolveNamesToIds(['NEW_SECRET'], afterList)
    expect(after).toEqual({ ids: ['3'], missing: [] })
  })
})
