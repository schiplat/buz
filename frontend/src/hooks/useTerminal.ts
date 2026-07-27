import { ref } from 'vue'

export function useTerminal() {
  const terminalEl = ref<HTMLElement | null>(null)
  // Will be implemented in Phase 1 with @xterm/xterm
  return { terminalEl }
}
