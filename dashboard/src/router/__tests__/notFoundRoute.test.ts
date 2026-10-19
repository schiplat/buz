import { describe, expect, it } from 'vitest'
import { router } from '@/router/index'

describe('not-found catch-all route', () => {
  it('resolves unknown paths to the friendly 404 page', async () => {
    const resolved = await router.resolve('/definitely/not/a/page')
    expect(resolved.name).toBe('not-found')
  })

  it('keeps known routes untouched', async () => {
    expect((await router.resolve('/dashboard')).name).toBe('dashboard')
    expect((await router.resolve('/jobs')).name).toBe('jobs')
    expect((await router.resolve('/login')).name).toBe('login')
  })

  it('does not shadow known prefixes', async () => {
    expect((await router.resolve('/jobs/abc')).name).toBe('job-detail')
    expect((await router.resolve('/pipelines/p1/definition')).name).toBe('pipeline-detail')
  })
})
