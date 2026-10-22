import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import Runners from '@/pages/Runners.vue'
import { runnersApi, type Runner } from '@/api/runners'
import { teamsApi } from '@/api/teams'

const { dialog } = vi.hoisted(() => ({
  dialog: { alert: vi.fn(), confirm: vi.fn() },
}))

vi.mock('@/api/runners', () => ({
  runnersApi: {
    list: vi.fn(),
    create: vi.fn(),
    update: vi.fn(),
    events: vi.fn(),
    toggleFreeze: vi.fn(),
    restart: vi.fn(),
    reissueRegisterToken: vi.fn(),
    delete: vi.fn(),
  },
}))
vi.mock('@/api/teams', () => ({ teamsApi: { list: vi.fn() } }))
vi.mock('@/composables/useAppDialog', () => ({ useAppDialog: () => dialog }))

let app: App | undefined
const flush = async () => {
  await new Promise((resolve) => setTimeout(resolve, 0))
  await nextTick()
}

const runner = (overrides: Partial<Runner> = {}): Runner => ({
  id: 'runner-1',
  name: 'runner-one',
  labels: ['linux-x64'],
  team_id: null,
  execution_mode: 'host',
  status: 'offline',
  version: null,
  system_info: null,
  is_frozen: false,
  max_concurrent_jobs: 1,
  clean_workspace_after_job: false,
  shell_enabled: true,
  ssh_enabled: true,
  docker_enabled: false,
  wasm_enabled: false,
  ssh_host_key_policy: 'known_hosts',
  running_jobs: 0,
  last_seen: null,
  created_at: '2026-10-21T00:00:00Z',
  updated_at: '2026-10-21T00:00:00Z',
  ...overrides,
})

function button(text: string, root: ParentNode = document.body): HTMLButtonElement {
  const found = [...root.querySelectorAll<HTMLButtonElement>('button')].find((el) => el.textContent?.trim() === text)
  expect(found, `missing button ${text}`).toBeTruthy()
  return found!
}

async function setSelect(el: HTMLSelectElement, value: string) {
  el.value = value
  el.dispatchEvent(new Event('change', { bubbles: true }))
  await nextTick()
}

async function mountPage(rows: Runner[] = []) {
  vi.mocked(runnersApi.list).mockResolvedValue(rows)
  const root = document.createElement('div')
  document.body.appendChild(root)
  app = createApp(Runners)
  app.mount(root)
  await flush()
  return root
}

beforeEach(() => {
  vi.resetAllMocks()
  dialog.alert.mockResolvedValue(undefined)
  dialog.confirm.mockResolvedValue(true)
  vi.mocked(teamsApi.list).mockResolvedValue([
    { id: 'team-a', name: 'Alpha', slug: 'alpha' },
    { id: 'team-b', name: 'Beta', slug: 'beta' },
  ])
  vi.mocked(runnersApi.create).mockResolvedValue({
    id: 'new-runner', name: 'new-runner', team_id: 'team-a', register_token: 'one-time-token',
  })
  vi.mocked(runnersApi.update).mockImplementation(async (_id, payload) => ({
    ...runner(),
    ...payload,
    team_id: payload.team_id ?? null,
  }))
  vi.mocked(runnersApi.events).mockResolvedValue({ items: [], total: 0, page: 1, page_size: 15, total_pages: 0 })
})

afterEach(() => {
  app?.unmount()
  app = undefined
  document.body.innerHTML = ''
})

describe('Runner team binding', () => {
  it('requires an explicit scope and sends the selected team on create', async () => {
    await mountPage()
    button('New Runner').click()
    await nextTick()

    const create = button('Create Runner')
    expect(create.disabled).toBe(true)

    const name = document.querySelector<HTMLInputElement>('input[placeholder="e.g. prod-runner-01"]')!
    name.value = 'team-runner'
    name.dispatchEvent(new Event('input', { bubbles: true }))
    await nextTick()
    expect(create.disabled).toBe(true)

    const team = [...document.querySelectorAll<HTMLSelectElement>('select')]
      .find((select) => [...select.options].some((option) => option.value === 'team-a'))!
    await setSelect(team, 'team-a')
    expect(create.disabled).toBe(false)
    create.click()
    await flush()

    expect(runnersApi.create).toHaveBeenCalledWith(expect.objectContaining({
      name: 'team-runner',
      team_id: 'team-a',
      execution_mode: 'host',
    }))
  })

  it('shows Shared warning for legacy runners and allows explicit bind/unbind', async () => {
    const root = await mountPage([runner()])
    expect(root.textContent).toContain('Shared Runner trust boundary')
    root.querySelector<HTMLButtonElement>('button[title="Open runner-one"]')!.click()
    await nextTick()
    expect(document.body.textContent).toContain('Shared dispatch crosses teams')

    const team = [...document.body.querySelectorAll<HTMLSelectElement>('select')]
      .find((select) => [...select.options].some((option) => option.value === 'team-a'))!
    const mode = [...document.body.querySelectorAll<HTMLSelectElement>('select')]
      .find((select) => [...select.options].some((option) => option.value === 'container'))!
    await setSelect(mode, 'container')
    await setSelect(team, 'team-a')
    button('Save Policy').click()
    await flush()
    expect(runnersApi.update).toHaveBeenLastCalledWith('runner-1', expect.objectContaining({
      team_id: 'team-a',
      execution_mode: 'container',
    }))

    await setSelect(team, 'shared')
    button('Save Policy').click()
    await flush()
    expect(runnersApi.update).toHaveBeenLastCalledWith('runner-1', expect.objectContaining({
      team_id: null,
      execution_mode: 'container',
    }))
  })
})
