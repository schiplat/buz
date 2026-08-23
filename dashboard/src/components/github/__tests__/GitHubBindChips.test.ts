import { describe, expect, it } from 'vitest'
import { createApp, nextTick } from 'vue'
import GitHubBindChips from '@/components/github/GitHubBindChips.vue'
import type { GitHubBind } from '@/api/githubSync'

function mountChips(binds: GitHubBind[]) {
  const el = document.createElement('div')
  document.body.appendChild(el)
  const app = createApp(GitHubBindChips, {
    binds,
    onOpen: () => window.dispatchEvent(new CustomEvent('chip-open')),
  })
  app.mount(el)
  return {
    el,
    unmount: () => {
      app.unmount()
      el.remove()
    },
  }
}

const baseBind = (overrides: Partial<GitHubBind>): GitHubBind => ({
  id: 'bind-1',
  repo_id: 'repo-1',
  connection_name: 'gh',
  owner: 'myorg',
  repo: 'alpha',
  last_pushed_at: null,
  last_status: 'pending',
  last_error: '',
  last_op: 'push',
  ...overrides,
})

describe('GitHubBindChips', () => {
  it('renders one chip per binding with owner/repo', async () => {
    const { el, unmount } = mountChips([
      baseBind({ id: 'a', owner: 'myorg', repo: 'alpha' }),
      baseBind({ id: 'b', owner: 'myorg', repo: 'beta' }),
    ])
    await nextTick()
    const buttons = el.querySelectorAll('button')
    expect(buttons.length).toBe(2)
    expect(buttons[0].textContent).toContain('myorg/alpha')
    expect(buttons[1].textContent).toContain('myorg/beta')
    unmount()
  })

  it('maps ok/failed/pending to emerald/red/amber accents', async () => {
    const { el, unmount } = mountChips([
      baseBind({ id: 'a', last_status: 'ok', last_pushed_at: '2026-08-01T00:00:00Z' }),
      baseBind({ id: 'b', last_status: 'failed', last_error: '401 token rejected' }),
      baseBind({ id: 'c', last_status: 'pending' }),
    ])
    await nextTick()
    const buttons = el.querySelectorAll('button')
    expect(buttons[0].className).toContain('emerald')
    expect(buttons[1].className).toContain('red')
    expect(buttons[2].className).toContain('amber')
    unmount()
  })

  it('labels a failed unbind as unbind failed in the tooltip', async () => {
    const { el, unmount } = mountChips([
      baseBind({ id: 'a', last_status: 'failed', last_op: 'unbind', last_error: 'unbind failed: 401' }),
      baseBind({ id: 'b', repo: 'beta', last_status: 'failed', last_op: 'push', last_error: '401 token rejected' }),
    ])
    await nextTick()
    const buttons = el.querySelectorAll('button')
    expect((buttons[0] as HTMLButtonElement).title).toContain('Unbind from myorg/alpha failed')
    expect((buttons[0] as HTMLButtonElement).title).toContain('unbind failed: 401')
    expect((buttons[1] as HTMLButtonElement).title).toContain('Push to myorg/beta failed')
    unmount()
  })

  it('emits open when a chip is clicked', async () => {
    let opened = false
    const el = document.createElement('div')
    document.body.appendChild(el)
    const app = createApp(GitHubBindChips, {
      binds: [baseBind({})],
      onOpen: () => {
        opened = true
      },
    })
    app.mount(el)
    await nextTick()
    ;(el.querySelector('button') as HTMLButtonElement).click()
    expect(opened).toBe(true)
    app.unmount()
    el.remove()
  })

  it('renders nothing for an empty bind list', async () => {
    const { el, unmount } = mountChips([])
    await nextTick()
    expect(el.querySelectorAll('button').length).toBe(0)
    unmount()
  })
})
