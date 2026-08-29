import { describe, expect, it, vi, beforeEach } from 'vitest'
import { createApp, nextTick } from 'vue'
import GitHubBindDialog from '@/components/github/GitHubBindDialog.vue'
import { githubSyncApi } from '@/api/githubSync'

vi.mock('@/api/githubSync', () => ({
  githubSyncApi: {
    listSecretBinds: vi.fn(),
    listVariableBinds: vi.fn(),
    listRepos: vi.fn(),
    addSecretBind: vi.fn(),
    addVariableBind: vi.fn(),
    pushSecretBinds: vi.fn(),
    pushVariableBinds: vi.fn(),
  },
}))

const flush = () => new Promise((resolve) => setTimeout(resolve, 0))

const repo = (id: string, owner: string, name: string) => ({
  id,
  connection_id: 'conn-1',
  connection_name: 'gh',
  owner,
  repo: name,
  url: `https://github.com/${owner}/${name}`,
  updated_at: null,
  binds: [],
})

function mountDialog() {
  const el = document.createElement('div')
  document.body.appendChild(el)
  const app = createApp(GitHubBindDialog, {
    show: true,
    resourceKind: 'secret',
    resourceId: 'secret-1',
    resourceName: 'MY_SECRET',
    canManage: true,
    'onUpdate:show': () => {},
    onChanged: () => {},
  })
  app.mount(el)
  return { unmount: () => { app.unmount(); el.remove() } }
}

beforeEach(() => {
  vi.clearAllMocks()
  vi.mocked(githubSyncApi.listSecretBinds).mockResolvedValue([])
  vi.mocked(githubSyncApi.listVariableBinds).mockResolvedValue([])
  vi.mocked(githubSyncApi.listRepos).mockResolvedValue([
    repo('repo-a', 'myorg', 'alpha'),
    repo('repo-b', 'myorg', 'beta'),
    repo('repo-c', 'myorg', 'gamma'),
  ])
  vi.mocked(githubSyncApi.addSecretBind).mockResolvedValue({} as never)
  vi.mocked(githubSyncApi.addVariableBind).mockResolvedValue({} as never)
})

async function mountAndLoad() {
  const mounted = mountDialog()
  await flush()
  await nextTick()
  return mounted
}

async function checkRepo(index: number) {
  const box = document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')[index]
  box.checked = true
  box.dispatchEvent(new Event('change', { bubbles: true }))
  await nextTick()
}

describe('GitHubBindDialog multi-select', () => {
  it('checks multiple repos and shows a removable chip per selection', async () => {
    const { unmount } = await mountAndLoad()
    const boxes = document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')
    expect(boxes.length).toBe(3)

    await checkRepo(0)
    await checkRepo(1)

    const chips = document.querySelectorAll('[title^="Remove "]')
    expect(chips.length).toBe(2)

    ;(document.querySelector('[title="Remove myorg/alpha"]') as HTMLButtonElement).click()
    await nextTick()
    expect(document.querySelectorAll('[title^="Remove "]').length).toBe(1)

    unmount()
  })

  it('binds every selected repo when Bind is clicked', async () => {
    const { unmount } = await mountAndLoad()
    await checkRepo(0)
    await checkRepo(1)
    await checkRepo(2)

    const buttons = [...document.querySelectorAll<HTMLButtonElement>('button')]
    const bindBtn = buttons.find((b) => b.textContent?.includes('Bind 3 repos'))
    expect(bindBtn).toBeTruthy()
    bindBtn!.click()
    await flush()

    expect(vi.mocked(githubSyncApi.addSecretBind)).toHaveBeenCalledTimes(3)
    expect(vi.mocked(githubSyncApi.addSecretBind)).toHaveBeenCalledWith('secret-1', 'repo-a')
    expect(vi.mocked(githubSyncApi.addSecretBind)).toHaveBeenCalledWith('secret-1', 'repo-b')
    expect(vi.mocked(githubSyncApi.addSecretBind)).toHaveBeenCalledWith('secret-1', 'repo-c')

    unmount()
  })

  it('reports partial failures without dropping successful binds', async () => {
    vi.mocked(githubSyncApi.addSecretBind).mockRejectedValueOnce(
      Object.assign(new Error('repo already bound'), { response: { data: { message: 'secret already bound to myorg/beta' } } }),
    )
    const { unmount } = await mountAndLoad()
    await checkRepo(0)
    await checkRepo(1)

    const buttons = [...document.querySelectorAll<HTMLButtonElement>('button')]
    const bindBtn = buttons.find((b) => b.textContent?.includes('Bind 2 repos'))
    bindBtn!.click()
    await flush()

    expect(vi.mocked(githubSyncApi.addSecretBind)).toHaveBeenCalledTimes(2)
    expect(document.body.textContent).toContain('Bind failed for')
    expect(document.body.textContent).toContain('myorg/beta')
    unmount()
  })
})

const bindRow = (id: string, owner: string, name: string) => ({
  id,
  repo_id: `repo-${name}`,
  connection_name: 'gh',
  owner,
  repo: name,
  last_pushed_at: null,
  last_status: 'pending' as const,
  last_error: '',
  last_op: 'bind' as const,
})

describe('GitHubBindDialog Push All', () => {
  it('pushes each bind with bind_id sequentially', async () => {
    vi.mocked(githubSyncApi.listSecretBinds).mockResolvedValue([
      bindRow('b1', 'myorg', 'alpha'),
      bindRow('b2', 'myorg', 'beta'),
    ])
    vi.mocked(githubSyncApi.pushSecretBinds)
      .mockResolvedValueOnce({ results: [{ bind_id: 'b1', owner: 'myorg', repo: 'alpha', status: 'ok' }] })
      .mockResolvedValueOnce({ results: [{ bind_id: 'b2', owner: 'myorg', repo: 'beta', status: 'ok' }] })

    const { unmount } = await mountAndLoad()
    const pushAll = [...document.querySelectorAll<HTMLButtonElement>('button')].find((b) =>
      b.textContent?.includes('Push All'),
    )
    expect(pushAll).toBeTruthy()
    pushAll!.click()
    await flush()
    await flush()

    expect(vi.mocked(githubSyncApi.pushSecretBinds)).toHaveBeenCalledTimes(2)
    expect(vi.mocked(githubSyncApi.pushSecretBinds)).toHaveBeenNthCalledWith(1, 'secret-1', { bind_id: 'b1' })
    expect(vi.mocked(githubSyncApi.pushSecretBinds)).toHaveBeenNthCalledWith(2, 'secret-1', { bind_id: 'b2' })
    // Never a bulk push without bind_id.
    for (const call of vi.mocked(githubSyncApi.pushSecretBinds).mock.calls) {
      expect(call[1]?.bind_id).toBeTruthy()
    }
    unmount()
  })
})
