import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import type { RunnerStatus } from '@/types'

export const useAppStore = defineStore('app', () => {
  const sidebarCollapsed = ref(false)
  const currentTitle = ref('Overview')

  function toggleSidebar() {
    sidebarCollapsed.value = !sidebarCollapsed.value
  }

  function setTitle(title: string) {
    currentTitle.value = title
  }

  return { sidebarCollapsed, currentTitle, toggleSidebar, setTitle }
})
