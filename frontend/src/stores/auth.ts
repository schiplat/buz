import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { authApi } from '@/api/auth'

export const useAuthStore = defineStore('auth', () => {
  const token = ref<string | null>(localStorage.getItem('buz_token'))
  const username = ref<string | null>(localStorage.getItem('buz_username'))

  const isAuthenticated = computed(() => !!token.value)

  async function login(user: string, pass: string) {
    const resp = await authApi.login(user, pass)
    token.value = resp.token
    username.value = user
    localStorage.setItem('buz_token', resp.token)
    localStorage.setItem('buz_username', user)
  }

  function logout() {
    token.value = null
    username.value = null
    localStorage.removeItem('buz_token')
    localStorage.removeItem('buz_username')
  }

  return { token, username, isAuthenticated, login, logout }
})
