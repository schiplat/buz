import { ref, onMounted, onUnmounted } from 'vue'

export function useWebSocket() {
  const connected = ref(false)
  // Will be implemented in Phase 1 with Xterm.js integration
  return { connected }
}
