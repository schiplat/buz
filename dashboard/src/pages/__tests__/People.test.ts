import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import People from '@/pages/People.vue'
import { usersApi, type User } from '@/api/users'
import { teamsApi, type Team, type TeamMember } from '@/api/teams'
import { oidcIdentitiesApi, type PendingIdentity } from '@/api/oidcIdentities'

const { auth, dialog } = vi.hoisted(() => ({
  auth: {
    isPlatformAdmin: true,
    isSuperAdmin: true,
    userId: 'u-boss',
    teams: [] as Array<{ id: string; name: string; slug: string }>,
  },
  dialog: { alert: vi.fn(), confirm: vi.fn() },
}))

vi.mock('@/api/users', () => ({
  usersApi: { list: vi.fn(), create: vi.fn(), update: vi.fn(), delete: vi.fn() },
}))
vi.mock('@/api/teams', () => ({
  teamsApi: {
    list: vi.fn(),
    get: vi.fn(),
    create: vi.fn(),
    update: vi.fn(),
    delete: vi.fn(),
    upsertMember: vi.fn(),
    removeMember: vi.fn(),
  },
}))
vi.mock('@/api/oidcIdentities', () => ({
  oidcIdentitiesApi: { listPending: vi.fn(), bind: vi.fn(), dismiss: vi.fn() },
}))
vi.mock('@/stores/auth', () => ({ useAuthStore: () => auth }))
vi.mock('@/composables/useAppDialog', () => ({ useAppDialog: () => dialog }))

let app: App | undefined

const flush = async () => {
  for (let i = 0; i < 3; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0))
    await nextTick()
  }
}

function mount() {
  const host = document.createElement('div')
  document.body.appendChild(host)
  app = createApp(People)
  // RouterLink only appears in the first-use strip; a plain anchor is enough.
  app.component('RouterLink', {
    props: { to: { type: String, required: true } },
    template: '<a :href="to"><slot /></a>',
  })
  app.mount(host)
  return host
}

function buttonByText(text: string): HTMLButtonElement {
  const element = [...document.querySelectorAll<HTMLButtonElement>('button')].find((node) =>
    node.textContent?.includes(text),
  )
  expect(element, `Missing button containing "${text}"`).toBeTruthy()
  return element!
}

const user = (overrides: Partial<User>): User => ({
  id: 'u',
  username: 'user',
  display_name: '',
  is_platform_admin: false,
  is_super_admin: false,
  is_active: true,
  auth_source: 'local',
  ...overrides,
})

const member = (overrides: Partial<TeamMember>): TeamMember => ({
  user_id: 'u',
  username: 'user',
  display_name: '',
  role: 'developer',
  ...overrides,
})

const team = (overrides: Partial<Team>): Team => ({
  id: 't',
  name: 'team',
  slug: 'team',
  my_role: null,
  ...overrides,
})

const pendingItem = (overrides: Partial<PendingIdentity>): PendingIdentity => ({
  id: 'p-1',
  provider: 'oidc',
  issuer: 'https://idp.example',
  subject: 'sub-1',
  email: 'dave@sso.example',
  display_name: 'Dave',
  groups: [],
  attempts: 2,
  ...overrides,
})

const alphaTeam = team({ id: 't-alpha', name: 'team-alpha', slug: 'team-alpha', my_role: 'owner' })
const alphaDetail = team({
  ...alphaTeam,
  members: [member({ user_id: 'u-boss', username: 'boss', role: 'owner' })],
})
const betaTeam = team({ id: 't-beta', name: 'team-beta', slug: 'team-beta', my_role: 'developer' })
const betaDetail = team({
  ...betaTeam,
  members: [
    member({ user_id: 'u-frank', username: 'frank', role: 'maintainer' }),
    member({ user_id: 'u-gina', username: 'gina', role: 'viewer' }),
  ],
})

beforeEach(() => {
  auth.isPlatformAdmin = true
  auth.isSuperAdmin = true
  auth.userId = 'u-boss'
  vi.mocked(usersApi.list).mockResolvedValue([
    user({ id: 'u-boss', username: 'boss', is_platform_admin: true, auth_source: 'oidc' }),
    user({ id: 'u-carol', username: 'carol' }),
    user({ id: 'u-dave', username: 'dave' }),
  ])
  vi.mocked(teamsApi.list).mockResolvedValue([alphaTeam])
  vi.mocked(teamsApi.get).mockResolvedValue(alphaDetail)
  vi.mocked(oidcIdentitiesApi.listPending).mockResolvedValue([])
})

afterEach(() => {
  app?.unmount()
  app = undefined
  document.body.innerHTML = ''
  vi.clearAllMocks()
})

describe('Users & teams', () => {
  it('gives platform admins the organization tree, account table and ungrouped bucket', async () => {
    mount()
    await flush()

    expect(document.body.textContent).toContain('Accounts')
    expect(document.body.textContent).toContain('All accounts')
    expect(document.body.textContent).toContain('team-alpha')
    expect(document.body.textContent).toContain('Ungrouped')
    // Accounts without a team are surfaced as an admin action cue, and the
    // first-use strip yields to it (two actionable accounts: carol and dave).
    expect(document.body.textContent).toContain('2 accounts have no team yet')
    expect(document.body.textContent).not.toContain('Identity & access in three steps')
    // Accounts table: ungrouped ones read "no team", SSO origin is badged.
    expect(document.body.textContent).toContain('carol')
    expect(document.body.textContent).toContain('no team')
    expect(document.body.textContent).toContain('SSO')

    // Ungrouped bucket lists the two accounts without a team, with inline grants.
    buttonByText('Ungrouped').click()
    await flush()
    expect(document.body.textContent).toContain('Accounts with no team membership')
    expect(document.body.textContent).toContain('Assign team')
    expect(document.body.textContent).toContain('carol')
    expect(document.body.textContent).toContain('dave')
    expect(document.body.textContent).not.toContain('boss')
  })

  it('lets an admin grant a team and role to an ungrouped account', async () => {
    mount()
    await flush()

    buttonByText('Ungrouped').click()
    await flush()

    const row = [...document.querySelectorAll('tbody tr')].find((node) =>
      node.textContent?.includes('carol'),
    )
    expect(row, 'Missing carol row').toBeTruthy()
    const selects = row!.querySelectorAll<HTMLSelectElement>('select')
    expect(selects.length).toBe(2)

    selects[0].value = 't-alpha'
    selects[0].dispatchEvent(new Event('change'))
    await flush()

    const grant = [...row!.querySelectorAll<HTMLButtonElement>('button')].find((node) =>
      node.textContent?.trim() === 'Grant',
    )
    expect(grant, 'Missing Grant button').toBeTruthy()
    grant!.click()
    await flush()

    expect(vi.mocked(teamsApi.upsertMember)).toHaveBeenCalledWith('t-alpha', {
      user_id: 'u-carol',
      role: 'developer',
    })
  })

  it('shows a selected team, and lets an owner change roles', async () => {
    mount()
    await flush()

    buttonByText('team-alpha').click()
    await flush()
    expect(document.body.textContent).toContain('team-alpha · members')
    expect(document.body.textContent).toContain('your role: owner')

    const roleSelect = document.querySelector<HTMLSelectElement>('tbody select')
    expect(roleSelect, 'Missing role select').not.toBeNull()
    roleSelect!.value = 'maintainer'
    roleSelect!.dispatchEvent(new Event('change'))
    await flush()

    expect(vi.mocked(teamsApi.upsertMember)).toHaveBeenCalledWith('t-alpha', {
      user_id: 'u-boss',
      role: 'maintainer',
    })
  })

  it('restricts members to their own teams without account administration', async () => {
    auth.isPlatformAdmin = false
    auth.isSuperAdmin = false
    auth.userId = 'u-frank'
    vi.mocked(teamsApi.list).mockResolvedValue([betaTeam])
    vi.mocked(teamsApi.get).mockResolvedValue(betaDetail)
    vi.mocked(usersApi.list).mockClear()

    mount()
    await flush()

    expect(document.body.textContent).toContain('My teams')
    expect(document.body.textContent).toContain('team-beta')
    expect(document.body.textContent).not.toContain('All accounts')
    expect(document.body.textContent).not.toContain('Ungrouped')
    expect([...document.querySelectorAll('button')].some((b) => b.textContent?.includes('New user'))).toBe(false)
    expect(vi.mocked(usersApi.list)).not.toHaveBeenCalled()

    buttonByText('team-beta').click()
    await flush()
    expect(document.body.textContent).toContain('team-beta · members')
    expect(document.body.textContent).toContain('frank')
    expect(document.body.textContent).toContain('gina')
    // Developers see roles as text, not editable selects.
    expect(document.querySelector('tbody select')).toBeNull()
  })

  it('shows the approval queue instead of the first-use strip when identities are pending', async () => {
    vi.mocked(oidcIdentitiesApi.listPending).mockResolvedValue([pendingItem({})])

    mount()
    await flush()

    expect(document.body.textContent).toContain('Pending SSO identities')
    expect(document.body.textContent).toContain('dave@sso.example')
    expect(document.body.textContent).not.toContain('Identity & access in three steps')
    // The approval queue takes precedence over the ungrouped action cue.
    expect(document.body.textContent).not.toContain('accounts have no team yet')
  })
})

describe('Users & teams — delete guard', () => {
  it('surfaces the team-delete conflict instead of failing silently', async () => {
    vi.mocked(dialog.confirm).mockResolvedValueOnce(true)
    vi.mocked(teamsApi.delete).mockRejectedValueOnce({
      response: { data: { message: 'team still has 1 pipeline(s), 0 secret(s), 1 host(s)' } },
    })
    mount()
    await flush()

    buttonByText('team-alpha').click()
    await flush()
    const del = document.querySelector<HTMLButtonElement>('button[title="Delete team"]')
    expect(del, 'Missing delete-team button').not.toBeNull()
    del!.click()
    await flush()

    expect(vi.mocked(dialog.alert)).toHaveBeenCalledWith(
      'team still has 1 pipeline(s), 0 secret(s), 1 host(s)',
    )
  })
})
