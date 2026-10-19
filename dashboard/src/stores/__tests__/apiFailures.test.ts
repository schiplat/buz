import { beforeEach, describe, expect, it } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useApiFailuresStore } from '@/stores/apiFailures'

describe('apiFailures store', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    useApiFailuresStore().resetForTests()
  })

  it('reports the newest failure and exposes it', () => {
    const store = useApiFailuresStore()
    store.report(502, 'bad gateway')
    expect(store.current?.status).toBe(502)
    store.report(503, 'unavailable')
    expect(store.current?.status).toBe(503)
    expect(store.current?.message).toBe('unavailable')
  })

  it('collapses bursts of the same status within the dedupe window', () => {
    const store = useApiFailuresStore()
    store.report(500, 'first')
    store.report(500, 'second')
    expect(store.current?.message).toBe('first', 'a burst of identical 5xx shows one notice')
    store.dismiss()
    store.report(500, 'after-dismiss')
    expect(store.current?.message).toBe('after-dismiss', 'dismiss clears the dedupe guard')
  })

  it('a different status replaces the shown notice immediately', () => {
    const store = useApiFailuresStore()
    store.report(500, 'boom')
    store.report(0, 'offline')
    expect(store.current?.status).toBe(0)
  })

  it('dismiss clears the notice', () => {
    const store = useApiFailuresStore()
    store.report(500, 'boom')
    store.dismiss()
    expect(store.current).toBeNull()
  })
})
