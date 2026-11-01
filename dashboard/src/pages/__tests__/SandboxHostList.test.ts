import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import SandboxHostList from '@/pages/SandboxHostList.vue'
import { sandboxHostsApi, type ResourceGrant, type SandboxHost } from '@/api/sandboxHosts'
import { teamsApi, type Team } from '@/api/teams'

const { dialog } = vi.hoisted(() => ({
  dialog: { alert: vi.fn(), confirm: vi.fn() },
}))

vi.mock('@/api/sandboxHosts', () => ({
  sandboxHostsApi: {
    list: vi.fn(),
    changes: vi.fn(),
    grants: vi.fn(),
    grant: vi.fn(),
    revokeGrant: vi.fn(),
    enroll: vi.fn(),
    remove: vi.fn(),
    setBudget: vi.fn(),
    get: vi.fn(),
    versions: vi.fn(),
    validate: vi.fn(),
    putConfig: vi.fn(),
    rollback: vi.fn(),
    getDefaults: vi.fn(),
    putDefaults: vi.fn(),
    resolveImage: vi.fn(),
  },
}))
vi.mock('@/api/teams', () => ({ teamsApi: { list: vi.fn() } }))
vi.mock('@/composables/useAppDialog', () => ({ useAppDialog: () => dialog }))

let app: App | undefined
let root: HTMLDivElement | undefined
const flush = async () => {
  await new Promise((resolve) => setTimeout(resolve, 0))
  await nextTick()
  await nextTick()
}

const grant = (teamId: string, teamName: string, profile: string): ResourceGrant => ({
  team_id: teamId,
  team_name: teamName,
  profile,
  created_at: '2026-11-01T00:00:00Z',
})

const team = (id: string, name: string): Team => ({ id, name, slug: name })

function buttonsWithAria(root: ParentNode, prefix: string): HTMLButtonElement[] {
  return [...root.querySelectorAll<HTMLButtonElement>('button[aria-label]')].filter((el) =>
    el.getAttribute('aria-label')?.startsWith(prefix),
  )
}

function buttonByText(root: ParentNode, text: string): HTMLButtonElement {
  const found = [...root.querySelectorAll<HTMLButtonElement>('button')].find(
    (el) => el.textContent?.trim() === text,
  )
  expect(found, `missing button "${text}"`).toBeTruthy()
  return found!
}

async function setSelect(el: HTMLSelectElement, value: string) {
  el.value = value
  el.dispatchEvent(new Event('change', { bubbles: true }))
  await nextTick()
}

function pickerOptions(root: ParentNode): string[] {
  const picker = root.querySelector<HTMLSelectElement>('select[aria-label="Pick a team to grant profiles"]')
  expect(picker, 'grant-to-team picker').toBeTruthy()
  return [...picker!.options].map((o) => o.textContent ?? '')
}

const host = (overrides: Partial<SandboxHost> = {}): SandboxHost => ({
  host_id: 'alpha',
  instance_id: '01',
  status: 'enrolled',
  max_resource_slots: 0,
  desired_version: 3,
  applied_version: 3,
  pushed: true,
  live: true,
  applied_at: null,
  applied_sha256: 'sha',
  last_fetch_at: null,
  last_fetch_error: null,
  consecutive_fetch_failures: 0,
  created_at: '2026-11-01T00:00:00Z',
  updated_at: '2026-11-01T00:00:00Z',
  document: null,
  attached_runners: [],
  ...overrides,
})

async function mountPage(opts: { grants: ResourceGrant[]; defaults?: unknown; hosts?: SandboxHost[] }) {
  vi.mocked(sandboxHostsApi.list).mockResolvedValue(opts.hosts ?? [])
  vi.mocked(sandboxHostsApi.changes).mockResolvedValue([])
  vi.mocked(sandboxHostsApi.grants).mockResolvedValue(opts.grants)
  vi.mocked(sandboxHostsApi.getDefaults).mockResolvedValue(
    (opts.defaults ?? {
      tiers: {},
      profiles: {},
      network: 'bridge',
      max_slots: 0,
    }) as Awaited<ReturnType<typeof sandboxHostsApi.getDefaults>>,
  )
  vi.mocked(sandboxHostsApi.revokeGrant).mockResolvedValue(undefined)
  root = document.createElement('div')
  document.body.appendChild(root)
  app = createApp(SandboxHostList)
  app.mount(root)
  await flush()
  return root
}

beforeEach(() => {
  vi.resetAllMocks()
  dialog.alert.mockResolvedValue(undefined)
  dialog.confirm.mockResolvedValue(true)
  vi.mocked(teamsApi.list).mockResolvedValue([
    team('t1', 'platform'),
    team('t2', 'infra'),
  ])
})

afterEach(() => {
  app?.unmount()
  app = undefined
  root?.remove()
  root = undefined
  document.body.innerHTML = ''
})

describe('SandboxHostList — fleet policy profile grants', () => {
  it('renders a visible revoke button on every granted profile chip', async () => {
    const view = await mountPage({
      grants: [
        grant('t1', 'platform', 'gpu-8x'),
        grant('t1', 'platform', 'mem-high'),
      ],
    })

    expect(view.textContent).toContain('Fleet policy · Profile grants')

    const revokeButtons = buttonsWithAria(view, 'Revoke')
    expect(revokeButtons, 'one revoke button per granted chip').toHaveLength(2)
    for (const el of revokeButtons) {
      expect(el.className).toContain('rounded-full')
      expect(el.className).toContain('bg-foreground/15')
      expect(el.querySelector('svg'), 'icon inside the revoke button').toBeTruthy()
    }
    expect(buttonsWithAria(view, 'Revoke gpu-8x from platform')).toHaveLength(1)
  })

  it('collapses >3 chips behind "+N more"; expanding reveals every revoke button', async () => {
    const profiles = ['cpu-64', 'ci-e2e', 'fuzz-long', 'batch-lg', 'gpu-a10']
    const view = await mountPage({
      grants: profiles.map((p) => grant('t1', 'platform', p)),
    })

    expect(buttonsWithAria(view, 'Revoke')).toHaveLength(3)
    buttonByText(view, '+2 more').dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    )
    await flush()

    expect(buttonsWithAria(view, 'Revoke')).toHaveLength(5)
    buttonByText(view, 'less').dispatchEvent(new MouseEvent('click', { bubbles: true }))
    await flush()
    expect(buttonsWithAria(view, 'Revoke')).toHaveLength(3)
  })

  it('lists unbound teams in the picker instead of empty rows', async () => {
    const view = await mountPage({ grants: [] })

    expect(view.textContent).toContain('Fleet policy · Profile grants')
    // no bindings at all: no rows, no revoke/remove buttons
    expect(buttonsWithAria(view, 'Revoke')).toHaveLength(0)
    expect(buttonsWithAria(view, 'Remove all grants from platform')).toHaveLength(0)
    // every team is a picker candidate
    expect(pickerOptions(view)).toContain('platform')
    expect(pickerOptions(view)).toContain('infra')
  })

  it('revokes through the API when a chip X is clicked', async () => {
    const view = await mountPage({
      grants: [grant('t1', 'platform', 'gpu-8x')],
    })

    buttonsWithAria(view, 'Revoke gpu-8x from platform')[0].dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    )
    await flush()
    await flush()

    expect(vi.mocked(sandboxHostsApi.revokeGrant)).toHaveBeenCalledWith('t1', 'gpu-8x')
  })

  it('row-level remove-binding button revokes every grant of the team', async () => {
    const view = await mountPage({
      grants: [
        grant('t1', 'platform', 'cpu-64'),
        grant('t1', 'platform', 'gpu-8x'),
      ],
    })
    // the reload after revoking sees an empty allowlist
    vi.mocked(sandboxHostsApi.grants).mockResolvedValue([])

    // teams without grants carry no remove button
    expect(buttonsWithAria(view, 'Remove all grants from infra')).toHaveLength(0)

    buttonsWithAria(view, 'Remove all grants from platform')[0].dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    )
    await flush()
    await flush()
    await flush()

    expect(dialog.confirm).toHaveBeenCalled()
    expect(vi.mocked(sandboxHostsApi.revokeGrant).mock.calls).toEqual([
      ['t1', 'cpu-64'],
      ['t1', 'gpu-8x'],
    ])
    // the binding row is gone; the team moves to the picker
    expect(buttonsWithAria(view, 'Revoke')).toHaveLength(0)
    expect(pickerOptions(view)).toContain('platform')
  })

  it('host card offers chips hide the implicit default size and order smallest first', async () => {
    const view = await mountPage({
      grants: [],
      defaults: {
        tiers: {},
        profiles: {
          default: { memory: '2g', cpu: '2', pids: 256, slots: 1 },
          small: { memory: '2g', cpu: '2', pids: 256, slots: 1 },
          medium: { memory: '4g', cpu: '4', pids: 512, slots: 2 },
          large: { memory: '8g', cpu: '8', pids: 1024, slots: 4 },
        },
        network: 'bridge',
        max_slots: 0,
      },
      hosts: [
        host({
          document: {
            profiles: {
              default: { memory: '2g', cpu: '2', pids: 256, slots: 1 },
              large: { memory: '8g', cpu: '8', pids: 1024, slots: 4 },
              medium: { memory: '4g', cpu: '4', pids: 512, slots: 2 },
              small: { memory: '2g', cpu: '2', pids: 256, slots: 1 },
            },
            tiers: {},
          } as SandboxHost['document'],
        }),
      ],
    })

    const offersRow = [...view.querySelectorAll('div')].find(
      (d) => d.querySelector('span')?.textContent === 'offers',
    )
    expect(offersRow, 'host card offers row').toBeTruthy()
    const chipTexts = [...offersRow!.querySelectorAll('span')].map((s) => s.textContent?.trim())
    expect(chipTexts).toContain('small')
    expect(chipTexts).toContain('medium')
    expect(chipTexts).toContain('large')
    expect(chipTexts).not.toContain('default')
    // smallest-first ordering: slots weight via the platform catalog
    const idx = (n: string) => chipTexts.indexOf(n)
    expect(idx('small')).toBeLessThan(idx('medium'))
    expect(idx('medium')).toBeLessThan(idx('large'))
  })

  it('orders granted profile chips smallest first', async () => {
    const view = await mountPage({
      grants: [
        grant('t1', 'platform', 'large'),
        grant('t1', 'platform', 'small'),
        grant('t1', 'platform', 'medium'),
      ],
      defaults: {
        tiers: {},
        profiles: {
          default: { memory: '2g', cpu: '2', pids: 256, slots: 1 },
          small: { memory: '2g', cpu: '2', pids: 256, slots: 1 },
          medium: { memory: '4g', cpu: '4', pids: 512, slots: 2 },
          large: { memory: '8g', cpu: '8', pids: 1024, slots: 4 },
        },
        network: 'bridge',
        max_slots: 0,
      },
    })

    expect(buttonsWithAria(view, 'Revoke').map((el) => el.getAttribute('aria-label'))).toEqual([
      'Revoke small from platform',
      'Revoke medium from platform',
      'Revoke large from platform',
    ])
  })

  it('picker opens the grant panel for an unbound team and grants through the API', async () => {
    vi.mocked(sandboxHostsApi.grant).mockResolvedValue(undefined)
    const view = await mountPage({ grants: [grant('t1', 'platform', 'gpu-8x')] })

    const picker = view.querySelector<HTMLSelectElement>('select[aria-label="Pick a team to grant profiles"]')
    expect(picker).toBeTruthy()
    await setSelect(picker!, 't2')
    await flush()

    // the infra row joined with its grant panel open
    const checkbox = view.querySelector<HTMLInputElement>('input[aria-label="Grant gpu-8x to infra"]')
    expect(checkbox, 'panel lists grantable profiles for infra').toBeTruthy()
    checkbox!.click()
    await flush()

    buttonByText(view, 'Grant (1)').dispatchEvent(new MouseEvent('click', { bubbles: true }))
    await flush()
    await flush()

    expect(vi.mocked(sandboxHostsApi.grant)).toHaveBeenCalledWith('t2', 'gpu-8x')
  })
})

describe('SandboxHostList — platform defaults dialog', () => {
  const defaultsPayload = {
    tiers: {},
    profiles: {
      default: { memory: '2g', cpu: '2', pids: 256, slots: 1 },
      small: { memory: '2g', cpu: '2', pids: 256, slots: 1 },
      medium: { memory: '4g', cpu: '4', pids: 512, slots: 2 },
    },
    network: 'bridge',
    max_slots: 0,
  }

  async function openDefaultsDialog() {
    const view = await mountPage({ grants: [], defaults: defaultsPayload })
    buttonByText(view, 'Platform defaults').dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    )
    await flush()
    return view
  }

  /** The job-size row div for a given name input value. */
  function sizeRow(view: ParentNode, name: string): HTMLDivElement {
    const input = [...view.querySelectorAll<HTMLInputElement>('input')].find(
      (el) => el.value === name && el.closest('.grid')?.textContent?.includes('Memory') === false,
    )
    expect(input, `job-size row for '${name}'`).toBeTruthy()
    return input!.closest('div.grid') as HTMLDivElement
  }

  afterEach(() => {
    app?.unmount()
    app = undefined
    root?.remove()
    root = undefined
    document.body.innerHTML = ''
  })

  it('lists no default row; small is locked and carries the default alias badge', async () => {
    const view = await openDefaultsDialog()

    const defaultRowInput = [...view.querySelectorAll<HTMLInputElement>('input')].find(
      (el) => el.value === 'default',
    )
    expect(defaultRowInput, "no 'default' row in job sizes").toBeUndefined()

    const smallRow = sizeRow(view, 'small')
    const nameInput = [...smallRow.querySelectorAll<HTMLInputElement>('input')][0]
    expect(nameInput.disabled, 'small name is locked').toBe(true)
    expect(smallRow.querySelectorAll('button'), 'no delete button on small').toHaveLength(0)
    expect(smallRow.textContent, 'no default alias text under small').not.toContain('default')

    const mediumRow = sizeRow(view, 'medium')
    expect(mediumRow.querySelectorAll('button'), 'other sizes keep delete').toHaveLength(1)
  })

  it('saves default in lockstep with small', async () => {
    vi.mocked(sandboxHostsApi.putDefaults).mockResolvedValue({
      bumped_hosts: [],
    } as Awaited<ReturnType<typeof sandboxHostsApi.putDefaults>>)
    const view = await openDefaultsDialog()

    // edit small's memory; default must follow on save
    const smallMemory = [...sizeRow(view, 'small').querySelectorAll<HTMLInputElement>('input')][1]
    smallMemory.value = '3g'
    smallMemory.dispatchEvent(new Event('input', { bubbles: true }))
    await flush()

    buttonByText(view, 'Save defaults').dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    )
    await flush()
    await flush()

    expect(vi.mocked(sandboxHostsApi.putDefaults)).toHaveBeenCalledTimes(1)
    const payload = vi.mocked(sandboxHostsApi.putDefaults).mock.calls[0][0] as {
      profiles: Record<string, { memory: string }>
    }
    expect(payload.profiles.default.memory).toBe('3g')
    expect(payload.profiles.small.memory).toBe('3g')
    expect(payload.profiles.medium.memory).toBe('4g')
  })
})
