import { computed } from 'vue'

export function useStatusMap() {
  const colors = {
    online: { bg: 'bg-green-400/10', text: 'text-green-400', dot: 'bg-green-400' },
    offline: { bg: 'bg-zinc-500/10', text: 'text-zinc-500', dot: 'bg-zinc-500' },
    busy: { bg: 'bg-yellow-400/10', text: 'text-yellow-400', dot: 'bg-yellow-400' },
    pending: { bg: 'bg-blue-400/10', text: 'text-blue-400', dot: 'bg-blue-400' },
    running: { bg: 'bg-blue-400/10', text: 'text-blue-400', dot: 'bg-blue-400' },
    success: { bg: 'bg-green-400/10', text: 'text-green-400', dot: 'bg-green-400' },
    failed: { bg: 'bg-red-400/10', text: 'text-red-400', dot: 'bg-red-400' },
    cancelled: { bg: 'bg-zinc-500/10', text: 'text-zinc-500', dot: 'bg-zinc-500' },
    timeout: { bg: 'bg-orange-400/10', text: 'text-orange-400', dot: 'bg-orange-400' },
  }

  return { colors }
}
