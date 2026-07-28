import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { authApi, type MeResponse, type TeamMembership } from '@/api/auth'

export const useAuthStore = defineStore('auth', () => {
  const token = ref<string | null>(localStorage.getItem('buz_token'))
  const username = ref<string | null>(localStorage.getItem('buz_username'))
  const userId = ref<string | null>(localStorage.getItem('buz_user_id'))
  const isPlatformAdmin = ref(localStorage.getItem('buz_is_admin') === '1')
  const teams = ref<TeamMembership[]>([])

  const isAuthenticated = computed(() => !!token.value)

  /** Platform admin or multi-team members must pick a team; single-team users inherit theirs. */
  const showTeamPicker = computed(
    () => isPlatformAdmin.value || teams.value.length > 1,
  )

  /** Auto team for single-membership users (empty when picker is shown or no membership). */
  const soleTeamId = computed(() =>
    !isPlatformAdmin.value && teams.value.length === 1 ? teams.value[0].id : '',
  )

  async function login(user: string, pass: string) {
    const resp = await authApi.login(user, pass)
    token.value = resp.token
    username.value = resp.username || user
    userId.value = resp.user_id || null
    isPlatformAdmin.value = !!resp.is_platform_admin
    localStorage.setItem('buz_token', resp.token)
    localStorage.setItem('buz_username', username.value || user)
    if (resp.user_id) localStorage.setItem('buz_user_id', resp.user_id)
    localStorage.setItem('buz_is_admin', isPlatformAdmin.value ? '1' : '0')
    await fetchMe()
  }

  async function fetchMe() {
    if (!token.value) return
    try {
      const me: MeResponse = await authApi.me()
      userId.value = me.id
      username.value = me.username
      isPlatformAdmin.value = me.is_platform_admin
      teams.value = me.teams || []
      localStorage.setItem('buz_user_id', me.id)
      localStorage.setItem('buz_username', me.username)
      localStorage.setItem('buz_is_admin', me.is_platform_admin ? '1' : '0')
    } catch {
      // ignore — 401 interceptor handles logout
    }
  }

  function logout() {
    token.value = null
    username.value = null
    userId.value = null
    isPlatformAdmin.value = false
    teams.value = []
    localStorage.removeItem('buz_token')
    localStorage.removeItem('buz_username')
    localStorage.removeItem('buz_user_id')
    localStorage.removeItem('buz_is_admin')
  }

  return {
    token,
    username,
    userId,
    isPlatformAdmin,
    teams,
    isAuthenticated,
    showTeamPicker,
    soleTeamId,
    login,
    fetchMe,
    logout,
  }
})
