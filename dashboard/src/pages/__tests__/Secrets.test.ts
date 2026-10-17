import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import Secrets from '@/pages/Secrets.vue'
import { secretsApi, type Secret, type SecretKind } from '@/api/secrets'
import { teamsApi } from '@/api/teams'

const { auth, dialog } = vi.hoisted(() => ({
  auth: {
    isPlatformAdmin: false,
    preferredTeamId: 'team-other',
    matchesActiveTeam: vi.fn(),
  },
  dialog: { alert: vi.fn(), confirm: vi.fn() },
}))

vi.mock('@/api/secrets', async (importOriginal) => ({
  ...await importOriginal<typeof import('@/api/secrets')>(),
  secretsApi: { list: vi.fn(), create: vi.fn(), update: vi.fn(), delete: vi.fn() },
}))
vi.mock('@/api/teams', () => ({ teamsApi: { list: vi.fn() } }))
vi.mock('@/stores/auth', () => ({ useAuthStore: () => auth }))
vi.mock('@/composables/useAppDialog', () => ({ useAppDialog: () => dialog }))
vi.mock('@/components/github/GitHubBindChips.vue', () => ({
  default: {
    props: ['binds'],
    emits: ['open'],
    template: '<button title="Open GitHub binds" @click="$emit(\'open\')">GitHub binds</button>',
  },
}))
vi.mock('@/components/github/GitHubBindDialog.vue', () => ({
  default: {
    props: ['show', 'resourceKind', 'resourceId', 'resourceName', 'canManage'],
    emits: ['update:show', 'changed'],
    template: `<div v-if="show" data-github-dialog :data-kind="resourceKind"
      :data-id="resourceId" :data-name="resourceName" :data-can-manage="canManage">
      <button @click="$emit('changed')">Reload GitHub binds</button>
    </div>`,
  },
}))

let app: App | undefined
const flush = async () => {
  await new Promise((resolve) => setTimeout(resolve, 0))
  await nextTick()
}

const secret = (kind: SecretKind, overrides: Partial<Secret> = {}): Secret => ({
  id: `secret-${kind}`,
  name: `EXISTING_${kind.toUpperCase()}`,
  value: 'redacted***',
  owner_group: 'Default',
  team_id: 'team-default',
  team_shared: true,
  kind,
  updated_at: '2026-10-17T00:00:00Z',
  ...overrides,
})

function required<T extends Element>(selector: string, root: ParentNode = document.body): T {
  const element = root.querySelector<T>(selector)
  expect(element, `Missing ${selector}`).not.toBeNull()
  return element!
}

function editor(): HTMLElement {
  return required<HTMLHeadingElement>('h3').parentElement!
}

function field<T extends Element>(label: string, selector: string): T {
  const element = [...editor().querySelectorAll('label')].find((node) =>
    node.textContent?.trim().startsWith(`${label} `),
  )
  expect(element, `Missing ${label} label`).toBeTruthy()
  return required<T>(selector, element!.parentElement!)
}

function button(text: string, root: ParentNode = document.body): HTMLButtonElement {
  const element = [...root.querySelectorAll<HTMLButtonElement>('button')].find((node) =>
    node.textContent?.trim() === text,
  )
  expect(element, `Missing ${text} button`).toBeTruthy()
  return element!
}

async function mountPage(rows: Secret[] = []) {
  vi.mocked(secretsApi.list).mockResolvedValue(rows)
  const el = document.createElement('div')
  document.body.appendChild(el)
  app = createApp(Secrets)
  app.mount(el)
  await flush()
}

async function openCreate() {
  button('Add Secret').click()
  await nextTick()
}

async function openEdit() {
  required<HTMLButtonElement>('button[title="Edit"]').click()
  await nextTick()
}

async function input(element: HTMLInputElement | HTMLTextAreaElement, value: string) {
  element.value = value
  element.dispatchEvent(new Event('input', { bubbles: true }))
  await nextTick()
}

async function select(element: HTMLSelectElement, value: string) {
  element.value = value
  element.dispatchEvent(new Event('change', { bubbles: true }))
  await nextTick()
}

function kindOptions() {
  return [...field<HTMLSelectElement>('Kind', 'select').options].map((option) => option.value)
}

function expectNoOldGuidance() {
  const text = document.body.textContent || ''
  for (const oldText of ['registry_secret', 'config.source', 'config_source', 'Advanced', 'Insert sample',
    'Registry credentials sample', 'Aliyun OSS auth sample', '"provider"', '"access_key_secret"']) {
    expect(text).not.toContain(oldText)
  }
  expect(document.body.querySelector('button[hidden]')).toBeNull()
}

beforeEach(() => {
  vi.resetAllMocks()
  auth.isPlatformAdmin = false
  auth.preferredTeamId = 'team-other'
  auth.matchesActiveTeam.mockReturnValue(true)
  dialog.alert.mockResolvedValue(undefined)
  dialog.confirm.mockResolvedValue(false)
  vi.mocked(teamsApi.list).mockResolvedValue([
    { id: 'team-other', name: 'Other', slug: 'other' },
    { id: 'team-default', name: 'Default', slug: 'default' },
  ])
  vi.mocked(secretsApi.create).mockResolvedValue({
    id: 'created', name: 'REGISTRY_PASSWORD', team_id: 'team-default', owner_group: 'Default',
    team_shared: false, kind: 'generic',
  })
  vi.mocked(secretsApi.update).mockResolvedValue({ id: 'updated', updated: true })
})

afterEach(() => {
  app?.unmount()
  app = undefined
  document.body.innerHTML = ''
})

describe('Secrets plugin-first guidance', () => {
  it('creates only Generic secrets with an empty, hidden value and no legacy entry or sample', async () => {
    await mountPage()
    expect(document.body.textContent).toContain('No secrets configured yet')
    expect(document.body.textContent).toContain('${secrets.NAME}')
    expectNoOldGuidance()

    await openCreate()
    expect(kindOptions()).toEqual(['generic'])
    expect(field<HTMLSelectElement>('Kind', 'select').value).toBe('generic')
    expect(field<HTMLInputElement>('Name', 'input').value).toBe('')
    expect(field<HTMLSelectElement>('Team', 'select').value).toBe('team-default')
    expect(required<HTMLInputElement>('input[type="checkbox"]', editor()).checked).toBe(false)
    const value = field<HTMLTextAreaElement>('Value', 'textarea')
    expect(value.value).toBe('')
    expect(value.placeholder).toBe('Paste secret value…')
    expect(value.getAttribute('style')).toContain('-webkit-text-security: disc')
    expectNoOldGuidance()
    expect(secretsApi.create).not.toHaveBeenCalled()
  })

  it('renders literal, pinned plugin examples with separate credentials and a secret URL', async () => {
    await mountPage()
    await openCreate()
    const examples = [...editor().querySelectorAll('pre')].map((node) => node.textContent || '')
    expect(examples).toHaveLength(2)
    expect(examples[0]).toContain('uses: buz/docker-login@1.1.0')
    expect(examples[0]).toContain('registry: crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com')
    expect(examples[0]).toContain('username: ${secrets.REGISTRY_USERNAME}')
    expect(examples[0]).toContain('password: ${secrets.REGISTRY_PASSWORD}')
    expect(examples[1]).toContain('uses: buz/config-fetch@1.1.0')
    expect(examples[1]).toContain('url: ${secrets.CONFIG_BUNDLE_URL}')
    expect(examples[1]).toContain('dest: config')
    const text = editor().textContent?.replace(/\s+/g, ' ') || ''
    expect(text).toContain('each in a separate Generic secret')
    expect(text).toContain('password itself, not a credentials JSON object')
    expect(text).toContain('full presigned URL with its filename and signed query parameters')
    expect(text).toContain('config.render')
    expect(text).toContain('step env')
    expect(field<HTMLTextAreaElement>('Value', 'textarea').value).toBe('')
    expectNoOldGuidance()
  })

  it('preserves required-value validation, name normalization, Team share and the create payload', async () => {
    await mountPage()
    await openCreate()
    await input(field<HTMLInputElement>('Name', 'input'), 'registry_password')
    expect(field<HTMLInputElement>('Name', 'input').value).toBe('REGISTRY_PASSWORD')
    button('Create', editor()).click()
    await flush()
    expect(dialog.alert).toHaveBeenCalledWith('Value is required')
    expect(secretsApi.create).not.toHaveBeenCalled()

    await input(field<HTMLTextAreaElement>('Value', 'textarea'), 'actual-password')
    await select(field<HTMLSelectElement>('Team', 'select'), 'team-other')
    required<HTMLInputElement>('input[type="checkbox"]', editor()).click()
    await nextTick()
    button('Create', editor()).click()
    await flush()
    expect(secretsApi.create).toHaveBeenCalledWith({
      name: 'REGISTRY_PASSWORD', value: 'actual-password', team_id: 'team-other',
      team_shared: true, kind: 'generic',
    })
    expect(secretsApi.list).toHaveBeenCalledTimes(2)
    expect(document.body.querySelector('h3')).toBeNull()
  })

  it.each(['registry', 'aliyun_oss_auth'] as const)(
    'keeps an existing %s kind and empty-value update payload without automatic migration',
    async (kind) => {
      const row = secret(kind)
      await mountPage([row])
      const badge = kind === 'registry' ? 'Registry (Deprecated)' : 'Aliyun OSS (Deprecated)'
      expect(document.body.textContent).toContain(badge)
      await openEdit()
      expect(kindOptions()).toEqual(['generic', kind])
      expect(field<HTMLSelectElement>('Kind', 'select').value).toBe(kind)
      const option = field<HTMLSelectElement>('Kind', 'select').selectedOptions[0]!
      expect(option.textContent).toContain('Deprecated')
      expect(field<HTMLInputElement>('Name', 'input').disabled).toBe(true)
      const value = field<HTMLTextAreaElement>('Value', 'textarea')
      expect(value.value).toBe('')
      expect(value.placeholder).toBe('Leave empty to keep current value')
      const text = editor().textContent?.replace(/\s+/g, ' ') || ''
      expect(text).toContain('Do not pass old credentials JSON directly as a plugin password')
      expect(text).toContain('Save the username and password separately as Generic secrets')
      expectNoOldGuidance()
      button('Update', editor()).click()
      await flush()
      expect(secretsApi.update).toHaveBeenCalledWith(row.id, {
        value: '', team_id: row.team_id, team_shared: true, kind,
      })
      expect(secretsApi.create).not.toHaveBeenCalled()
    },
  )

  it.each(['registry', 'aliyun_oss_auth'] as const)(
    'retains the deprecated %s placeholder and kind when re-encrypting a legacy value',
    async (kind) => {
      const row = secret(kind, { reencrypt_required: true })
      await mountPage([row])
      await openEdit()
      const value = field<HTMLTextAreaElement>('Value', 'textarea')
      expect(value.placeholder).toBe(kind === 'registry'
        ? 'Paste registry credentials JSON… (Deprecated kind)'
        : 'Paste Aliyun OSS credentials JSON… (Deprecated kind)')
      button('Update', editor()).click()
      await flush()
      expect(dialog.alert).toHaveBeenCalledWith(
        'This secret must be re-saved with a new value (plaintext storage is no longer accepted).',
      )
      expect(secretsApi.update).not.toHaveBeenCalled()
      await input(value, 'replacement-legacy-value')
      button('Update', editor()).click()
      await flush()
      expect(secretsApi.update).toHaveBeenCalledWith(row.id, {
        value: 'replacement-legacy-value', team_id: row.team_id, team_shared: true, kind,
      })
    },
  )

  it.each(['registry', 'aliyun_oss_auth'] as const)(
    'allows explicit Generic selection from %s, but never offers the other legacy kind',
    async (kind) => {
      const row = secret(kind)
      await mountPage([row])
      await openEdit()
      await select(field<HTMLSelectElement>('Kind', 'select'), 'generic')
      expect(kindOptions()).toEqual(['generic', kind])
      expect(editor().textContent).toContain('Deprecated kind')
      await input(field<HTMLTextAreaElement>('Value', 'textarea'), 'single-credential')
      button('Update', editor()).click()
      await flush()
      expect(secretsApi.update).toHaveBeenCalledWith(row.id, {
        value: 'single-credential', team_id: row.team_id, team_shared: true, kind: 'generic',
      })
    },
  )

  it('offers only Generic when editing a Generic secret', async () => {
    await mountPage([secret('generic')])
    await openEdit()
    expect(kindOptions()).toEqual(['generic'])
    expect(editor().textContent).not.toContain('Deprecated kind')
    expect(field<HTMLTextAreaElement>('Value', 'textarea').value).toBe('')
  })

  it('resets legacy kind, value and visibility when switching from edit to a new secret', async () => {
    await mountPage([secret('registry')])
    await openEdit()
    await input(field<HTMLTextAreaElement>('Value', 'textarea'), 'edited-value')
    button('Show', editor()).click()
    await nextTick()
    expect(field<HTMLTextAreaElement>('Value', 'textarea').getAttribute('style')).not.toContain('disc')
    button('Hide', editor()).click()
    await nextTick()
    expect(field<HTMLTextAreaElement>('Value', 'textarea').getAttribute('style')).toContain('disc')
    button('Cancel', editor()).click()
    await nextTick()
    await openCreate()
    expect(kindOptions()).toEqual(['generic'])
    expect(field<HTMLTextAreaElement>('Value', 'textarea').value).toBe('')
    expect(field<HTMLTextAreaElement>('Value', 'textarea').getAttribute('style')).toContain('disc')
  })

  it('keeps active-team filtering and redacted list values', async () => {
    auth.matchesActiveTeam.mockImplementation((id: string) => id === 'team-default')
    await mountPage([
      secret('registry'),
      secret('generic', { name: 'OTHER_TEAM_SECRET', team_id: 'team-other' }),
    ])
    expect(document.body.textContent).toContain('EXISTING_REGISTRY')
    expect(document.body.textContent).toContain('redacted***')
    expect(document.body.textContent).not.toContain('OTHER_TEAM_SECRET')
    expect(document.body.querySelectorAll('tbody tr')).toHaveLength(1)
  })

  it.each([false, true])('preserves GitHub binding resource and platform-admin management (%s)', async (admin) => {
    auth.isPlatformAdmin = admin
    const row = secret('registry', {
      github_binds: [{
        id: 'bind-1', repo_id: 'repo-1', connection_name: 'github', owner: 'org', repo: 'app',
        last_pushed_at: null, last_status: 'pending', last_error: '', last_op: 'bind',
      }],
    })
    await mountPage([row])
    required<HTMLButtonElement>('button[title="Open GitHub binds"]').click()
    await nextTick()
    const github = required<HTMLElement>('[data-github-dialog]')
    expect(github.dataset.kind).toBe('secret')
    expect(github.dataset.id).toBe(row.id)
    expect(github.dataset.name).toBe(row.name)
    expect(github.dataset.canManage).toBe(String(admin))
    button('Reload GitHub binds', github).click()
    await flush()
    expect(secretsApi.list).toHaveBeenCalledTimes(2)
  })
})
