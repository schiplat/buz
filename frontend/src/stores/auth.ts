import { defineStore } from 'pinia'
import { ref, computed, watch } from 'vue'
import { authApi, type MeResponse, type TeamMembership } from '@/api/auth'
import { matchesTeamFilter } from '@/utils/teamFilter'

const ACTIVE_TEAM_KEY = 'buz_active_team'

export const useAuthStore = defineStore('auth', () => {
  const token = ref<string | null>(localStorage.getItem('buz_token'))
  const username = ref<string | null>(localStorage.getItem('buz_username'))
  const userId = ref<string | null>(localStorage.getItem('buz_user_id'))
  const isPlatformAdmin = ref(localStorage.getItem('buz_is_admin') === '1')
  const isSuperAdmin = ref(localStorage.getItem('buz_is_super_admin') === '1')
  const teams = ref<TeamMembership[]>([])

  /** Empty string = All my teams. Persisted across reloads. */
  const activeTeamId = ref<string>(localStorage.getItem(ACTIVE_TEAM_KEY) ?? '')

  const isAuthenticated = computed(() => !!token.value)

  /** Platform admin or multi-team members must pick a team; single-team users inherit theirs. */
  const showTeamPicker = computed(
    () => isPlatformAdmin.value || teams.value.length > 1,
  )

  /** Same visibility rule as form picker — top-bar switcher for multi-team UX. */
  const showTeamSwitcher = computed(
    () => isPlatformAdmin.value || teams.value.length > 1,
  )

  /** Auto team for single-membership users (empty when picker is shown or no membership). */
  const soleTeamId = computed(() =>
    !isPlatformAdmin.value && teams.value.length === 1 ? teams.value[0].id : '',
  )

  /** Prefer active switcher team, else sole membership — for create-form defaults. */
  const preferredTeamId = computed(() => {
    if (activeTeamId.value) return activeTeamId.value
    return soleTeamId.value
  })

  function setActiveTeamId(id: string) {
    activeTeamId.value = id
    if (id) {
      localStorage.setItem(ACTIVE_TEAM_KEY, id)
    } else {
      localStorage.removeItem(ACTIVE_TEAM_KEY)
    }
  }

  /** Drop stale active team after /me if user lost membership (non-admin). */
  function sanitizeActiveTeam() {
    if (!activeTeamId.value) return
    if (isPlatformAdmin.value) return
    const ok = teams.value.some((t) => t.id === activeTeamId.value)
    if (!ok) setActiveTeamId('')
  }
  function matchesActiveTeam(
    resourceTeamId: string | null | undefined,
    opts?: { includeShared?: boolean },
  ): boolean {
    return matchesTeamFilter(resourceTeamId, activeTeamId.value, opts)
  }

  async function applySession(resp: {
    token: string
    user_id?: string
    username?: string
    is_platform_admin?: boolean
  }, fallbackUser: string) {
    token.value = resp.token
    username.value = resp.username || fallbackUser
    userId.value = resp.user_id || null
    isPlatformAdmin.value = !!resp.is_platform_admin
    localStorage.setItem('buz_token', resp.token)
    localStorage.setItem('buz_username', username.value || fallbackUser)
    if (resp.user_id) localStorage.setItem('buz_user_id', resp.user_id)
    localStorage.setItem('buz_is_admin', isPlatformAdmin.value ? '1' : '0')
    await fetchMe()
  }

  async function login(user: string, pass: string) {
    const resp = await authApi.login(user, pass)
    await applySession(resp, user)
  }

  async function completeSetup(user: string, pass: string) {
    const resp = await authApi.setup(user, pass)
    const { markSetupComplete } = await import('@/router')
    markSetupComplete()
    await applySession(resp, user)
  }

  async function fetchMe() {
    if (!token.value) return
    try {
      const me: MeResponse = await authApi.me()
      userId.value = me.id
      username.value = me.username
      isPlatformAdmin.value = me.is_platform_admin
      isSuperAdmin.value = !!me.is_super_admin
      teams.value = me.teams || []
      localStorage.setItem('buz_user_id', me.id)
      localStorage.setItem('buz_username', me.username)
      localStorage.setItem('buz_is_admin', me.is_platform_admin ? '1' : '0')
      localStorage.setItem('buz_is_super_admin', me.is_super_admin ? '1' : '0')
      sanitizeActiveTeam()
    } catch {
      // ignore — 401 interceptor handles logout
    }
  }

  function logout() {
    token.value = null
    username.value = null
    userId.value = null
    isPlatformAdmin.value = false
    isSuperAdmin.value = false
    teams.value = []
    localStorage.removeItem('buz_token')
    localStorage.removeItem('buz_username')
    localStorage.removeItem('buz_user_id')
    localStorage.removeItem('buz_is_admin')
    localStorage.removeItem('buz_is_super_admin')
    // Keep activeTeamId preference across sessions of the same browser user
  }

  watch(teams, () => sanitizeActiveTeam())

  return {
    token,
    username,
    userId,
    isPlatformAdmin,
    isSuperAdmin,
    teams,
    activeTeamId,
    isAuthenticated,
    showTeamPicker,
    showTeamSwitcher,
    soleTeamId,
    preferredTeamId,
    setActiveTeamId,
    matchesActiveTeam,
    login,
    completeSetup,
    fetchMe,
    logout,
  }
})
