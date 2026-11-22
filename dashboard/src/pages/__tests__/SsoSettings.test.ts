import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import SsoSettings from '@/pages/SsoSettings.vue'
import { oidcSettingsApi, type OidcEvent, type OidcSettings } from '@/api/oidcSettings'
import { oidcIdentitiesApi } from '@/api/oidcIdentities'
import { sessionPolicyApi } from '@/api/sessionPolicy'

const { dialog } = vi.hoisted(() => ({
  dialog: { alert: vi.fn(), confirm: vi.fn() },
}))

vi.mock('@/api/oidcSettings', () => ({
  oidcSettingsApi: {
    get: vi.fn(),
    update: vi.fn(),
    reset: vi.fn(),
    test: vi.fn(),
    events: vi.fn(),
  },
}))
vi.mock('@/api/oidcIdentities', () => ({
  oidcIdentitiesApi: { listPending: vi.fn() },
}))
vi.mock('@/api/sessionPolicy', () => ({
  sessionPolicyApi: { get: vi.fn(), update: vi.fn() },
}))
vi.mock('@/composables/useAppDialog', () => ({ useAppDialog: () => dialog }))
vi.mock('vue-router', () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
  useRoute: () => ({ query: {} as Record<string, string> }),
}))

const settings = (overrides: Partial<OidcSettings> = {}): OidcSettings => ({
  source: 'database',
  enabled: true,
  issuer: 'https://sso.example.com',
  client_id: 'buz',
  client_secret_configured: true,
  redirect_uri_effective: 'https://buz.example.com/api/v1/auth/oidc/callback',
  redirect_uri_override: null,
  public_base_url: 'https://buz.example.com',
  derived_redirect_uri: 'https://buz.example.com/api/v1/auth/oidc/callback',
  backchannel_logout_uri: 'https://buz.example.com/api/v1/auth/oidc/backchannel-logout',
  post_logout_redirect_uri: 'https://buz.example.com/auth/sso/logged-out',
  scopes: ['openid', 'profile', 'email'],
  auto_provision: true,
  token_auth: 'auto',
  groups_claim: 'groups',
  updated_by: 'boss',
  active: true,
  validation: { ok: true },
  ...overrides,
})

const event = (overrides: Partial<OidcEvent> = {}): OidcEvent => ({
  id: 'evt-1',
  created_at: '2026-11-21 10:00:00',
  actor_user_id: 'user-1',
  actor_username: 'alice',
  action: 'auth.oidc_login',
  resource_type: 'user',
  resource_id: 'user-1',
  resource_name: 'alice',
  team_id: null,
  before: null,
  after: {
    username: 'alice',
    subject: 'sub-alice',
    email: 'alice@example.com',
    detail: 'mapped to account alice',
  },
  meta: null,
  ip: '',
  ...overrides,
})

let app: App | undefined
const flush = async () => {
  await new Promise((resolve) => setTimeout(resolve, 0))
  await nextTick()
}

async function mountPage() {
  const root = document.createElement('div')
  document.body.appendChild(root)
  app = createApp(SsoSettings)
  app.mount(root)
  await flush()
  await flush()
  return root
}

function buttonByText(root: HTMLElement, label: string): HTMLButtonElement {
  const button = [...root.querySelectorAll('button')].find(
    (candidate) => candidate.textContent?.trim() === label,
  )
  if (!button) throw new Error(`no button labelled ${label}`)
  return button
}

function input(root: HTMLElement, selector: string): HTMLInputElement {
  const element = root.querySelector<HTMLInputElement>(selector)
  if (!element) throw new Error(`no input ${selector}`)
  return element
}

function setValue(element: HTMLInputElement | HTMLSelectElement, value: string) {
  element.value = value
  element.dispatchEvent(new Event('input', { bubbles: true }))
  element.dispatchEvent(new Event('change', { bubbles: true }))
}

beforeEach(() => {
  vi.resetAllMocks()
  dialog.confirm.mockResolvedValue(true)
  vi.mocked(oidcSettingsApi.get).mockResolvedValue(settings())
  vi.mocked(oidcSettingsApi.update).mockResolvedValue(settings())
  vi.mocked(oidcIdentitiesApi.listPending).mockResolvedValue([])
  vi.mocked(oidcSettingsApi.events).mockResolvedValue({
    items: [event()],
    total: 1,
    page: 1,
    page_size: 20,
  })
  vi.mocked(sessionPolicyApi.get).mockResolvedValue({
    ttl_hours: 24,
    ttl_default_hours: 24,
    ttl_overridden: false,
    idle_minutes: 0,
    idle_default_minutes: 0,
    idle_overridden: false,
  })
  vi.mocked(sessionPolicyApi.update).mockResolvedValue({
    ttl_hours: 24,
    ttl_default_hours: 24,
    ttl_overridden: false,
    idle_minutes: 0,
    idle_default_minutes: 0,
    idle_overridden: false,
  })
})

afterEach(() => {
  app?.unmount()
  app = undefined
  document.body.innerHTML = ''
})

describe('SsoSettings', () => {
  it('saves the token endpoint auth override and the groups claim name', async () => {
    const root = await mountPage()

    setValue(root.querySelector<HTMLSelectElement>('#sso-token-auth')!, 'client_secret_basic')
    setValue(input(root, '#sso-groups-claim'), 'roles')
    await nextTick()

    buttonByText(root, 'Save & apply').click()
    await flush()

    expect(oidcSettingsApi.update).toHaveBeenCalledTimes(1)
    const payload = vi.mocked(oidcSettingsApi.update).mock.calls[0][0]
    expect(payload.token_auth).toBe('client_secret_basic')
    expect(payload.groups_claim).toBe('roles')
  })

  it('saves the access policy from the sidebar card', async () => {
    const root = await mountPage()

    setValue(input(root, '#sso-session-ttl'), '12')
    setValue(input(root, '#sso-session-idle'), '30')
    await nextTick()

    buttonByText(root, 'Save policy').click()
    await flush()

    expect(sessionPolicyApi.update).toHaveBeenCalledWith({ ttl_hours: 12, idle_minutes: 30 })
  })

  it('renders recent SSO events with friendly labels', async () => {
    const root = await mountPage()

    expect(root.textContent).toContain('Recent authentication events')
    expect(oidcSettingsApi.events).toHaveBeenCalledWith(1, 20)
    expect(root.textContent).toContain('Signed in')
    // User = the identity (email), Detail = the backend's sentence.
    expect(root.textContent).toContain('alice@example.com')
    expect(root.textContent).toContain('mapped to account alice')
    // The opaque IdP subject never reaches the UI.
    expect(root.innerHTML).not.toContain('sub-alice')
  })

  it('keeps the last-saved SSO knobs when only the sign-in policy changes', async () => {
    vi.mocked(oidcSettingsApi.get).mockResolvedValue(
      settings({ auto_provision: true, token_auth: 'client_secret_post', groups_claim: 'roles' }),
    )
    vi.mocked(oidcSettingsApi.update).mockResolvedValue(
      settings({ auto_provision: false, token_auth: 'client_secret_post', groups_claim: 'roles' }),
    )
    const root = await mountPage()

    const queue = [...root.querySelectorAll('input[type="radio"]')][1] as HTMLInputElement
    queue.checked = true
    queue.dispatchEvent(new Event('change', { bubbles: true }))
    await flush()

    const payload = vi.mocked(oidcSettingsApi.update).mock.calls[0][0]
    expect(payload.auto_provision).toBe(false)
    expect(payload.token_auth).toBe('client_secret_post')
    expect(payload.groups_claim).toBe('roles')
  })

  // The page used to branch on `updated_by === ''`: a never-configured instance
  // rendered only "Provider connection" and pushed "Register at your provider"
  // into the 320px sidebar while hiding "Sign-in policy" entirely, so two
  // releases looked like different pages. One layout now, with defaults.
  it('renders every card with defaults before the first save', async () => {
    vi.mocked(oidcSettingsApi.get).mockResolvedValue(
      settings({
        enabled: false,
        issuer: '',
        client_id: '',
        client_secret_configured: false,
        active: false,
        updated_by: '',
        // The Manager derives these from server.public_base_url even with no
        // saved configuration — they are what the operator registers.
        redirect_uri_effective:
          'https://buz.example.com/api/v1/auth/oidc/callback',
        backchannel_logout_uri:
          'https://buz.example.com/api/v1/auth/oidc/backchannel-logout',
        post_logout_redirect_uri:
          'https://buz.example.com/auth/sso/logged-out',
      }),
    )
    const root = await mountPage()

    expect(root.textContent).toContain('Provider connection')
    expect(root.textContent).toContain('Register at your provider')
    expect(root.textContent).toContain('Sign-in policy')
    expect(root.textContent).toContain('Access policy')
    expect(root.textContent).toContain('No configuration yet')

    // The registration card lists the derived URIs the provider needs, even
    // before anything is saved — that is the whole point of showing it.
    expect(root.textContent).toContain('/api/v1/auth/oidc/callback')
    expect(root.textContent).not.toContain('available after the issuer is saved')

    // Picking a policy before the first save is a preview, not a request: the
    // PUT would carry an empty issuer and be rejected.
    const queue = [...root.querySelectorAll('input[type="radio"]')][1] as HTMLInputElement
    queue.checked = true
    queue.dispatchEvent(new Event('change', { bubbles: true }))
    await flush()

    expect(oidcSettingsApi.update).not.toHaveBeenCalled()
    expect(root.textContent).toContain('rides along with your first Save')
  })
})
