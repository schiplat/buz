import { describe, expect, it } from 'vitest'
import { createMemoryHistory, createRouter, type RouteLocationNormalized } from 'vue-router'

const stub = { template: '<div/>' }

function makeRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', redirect: '/environments' },
      { path: '/environments', name: 'environments', component: stub },
      {
        path: '/environments/:id',
        name: 'environment-detail',
        component: stub,
      },
      {
        path: '/environments/:id/protection',
        redirect: (to: RouteLocationNormalized) => ({
          name: 'environment-detail',
          params: { id: to.params.id },
        }),
      },
      {
        path: '/environments/:id/overrides',
        redirect: (to: RouteLocationNormalized) => ({
          name: 'environment-detail',
          params: { id: to.params.id },
        }),
      },
      {
        path: '/environments/:id/variables',
        redirect: (to: RouteLocationNormalized) => ({
          name: 'environment-detail',
          params: { id: to.params.id },
        }),
      },
      {
        path: '/environments/:id/secrets',
        redirect: (to: RouteLocationNormalized) => ({
          name: 'environment-detail',
          params: { id: to.params.id },
        }),
      },
    ],
  })
}

describe('environment routes', () => {
  it('keeps list and detail on separate named routes', () => {
    const names = makeRouter().getRoutes().map((r) => r.name)
    expect(names).toContain('environments')
    expect(names).toContain('environment-detail')
  })

  it('resolves detail at /environments/:id without tabs', async () => {
    const router = makeRouter()
    await router.push('/environments')
    expect(router.currentRoute.value.name).toBe('environments')

    const id = 'ffa91957-99de-4b34-933a-1f9831e18efe'
    await router.push(`/environments/${id}`)
    expect(router.currentRoute.value.name).toBe('environment-detail')
    expect(router.currentRoute.value.params.id).toBe(id)
    expect(router.currentRoute.value.params.tab).toBeUndefined()
  })

  it('redirects legacy tab paths to /environments/:id', async () => {
    const router = makeRouter()
    const id = 'ffa91957-99de-4b34-933a-1f9831e18efe'
    for (const tab of ['protection', 'overrides', 'variables', 'secrets']) {
      await router.push(`/environments/${id}/${tab}`)
      expect(router.currentRoute.value.name).toBe('environment-detail')
      expect(router.currentRoute.value.path).toBe(`/environments/${id}`)
    }
  })
})
