import { navLog } from './navigation'

export type ManagedWebSocketHandlers = {
  onMessage?: (data: string) => void
  onOpen?: () => void
  /** Called after a close that will NOT be auto-retried (manual close / unmount). */
  onClose?: () => void
  label?: string
}

export type ManagedWebSocket = {
  /** Replace the active socket (closes previous). */
  connect: (url: string) => void
  /** Close permanently — disables reconnect. */
  close: () => void
  readonly readyState: number
}

/**
 * WebSocket wrapper with exponential-backoff reconnect.
 * Prevents half-open sockets from lingering after manager restart.
 */
export function createManagedWebSocket(
  handlers: ManagedWebSocketHandlers = {},
): ManagedWebSocket {
  const label = handlers.label || 'ws'
  let socket: WebSocket | null = null
  let url: string | null = null
  let closed = false
  let attempt = 0
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null

  function clearReconnect() {
    if (reconnectTimer) {
      clearTimeout(reconnectTimer)
      reconnectTimer = null
    }
  }

  function scheduleReconnect() {
    if (closed || !url) return
    clearReconnect()
    const delay = Math.min(30_000, 1_000 * 2 ** Math.min(attempt, 5))
    attempt += 1
    navLog(`[${label}] reconnect in ${delay}ms (attempt ${attempt})`)
    reconnectTimer = setTimeout(() => {
      if (!closed && url) open(url)
    }, delay)
  }

  function open(nextUrl: string) {
    url = nextUrl
    if (socket) {
      socket.onopen = null
      socket.onmessage = null
      socket.onerror = null
      socket.onclose = null
      try {
        socket.close()
      } catch {
        /* ignore */
      }
      socket = null
    }

    navLog(`[${label}] connecting`, nextUrl.replace(/token=[^&]+/, 'token=***'))
    const ws = new WebSocket(nextUrl)
    socket = ws

    ws.onopen = () => {
      attempt = 0
      navLog(`[${label}] open`)
      handlers.onOpen?.()
    }

    ws.onmessage = (event) => {
      try {
        handlers.onMessage?.(String(event.data))
      } catch (err) {
        console.error(`[${label}] onMessage error`, err)
      }
    }

    ws.onerror = () => {
      console.error(`[${label}] error`)
    }

    ws.onclose = () => {
      navLog(`[${label}] closed`)
      if (socket === ws) socket = null
      if (closed) {
        handlers.onClose?.()
        return
      }
      scheduleReconnect()
    }
  }

  return {
    connect(nextUrl: string) {
      closed = false
      attempt = 0
      clearReconnect()
      open(nextUrl)
    },
    close() {
      closed = true
      clearReconnect()
      if (socket) {
        socket.onopen = null
        socket.onmessage = null
        socket.onerror = null
        socket.onclose = null
        try {
          socket.close()
        } catch {
          /* ignore */
        }
        socket = null
      }
      handlers.onClose?.()
    },
    get readyState() {
      return socket?.readyState ?? WebSocket.CLOSED
    },
  }
}
