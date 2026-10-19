import { createApp } from 'vue'
import { createPinia } from 'pinia'
import './style.css'
import App from './App.vue'
import { router } from './router'
import { isNavDebugEnabled } from '@/utils/navigation'

const app = createApp(App)

// Global error handler: capture template rendering errors and Vue-internal
// failures (e.g. accessing properties on undefined) so a single broken page
// doesn't brick the entire application / sidebar navigation.
app.config.errorHandler = (err, instance, info) => {
  console.error('[Vue] Unhandled error:', err)
  console.error('[Vue] Component:', instance?.$?.type?.__name || instance?.$?.type?.name || 'unknown')
  console.error('[Vue] Info:', info)
  // Surface a friendly notice for errors that escape the route-level
  // ErrorBoundary (e.g. failures inside dialogs / outside the shell).
  void import('@/stores/apiFailures')
    .then(({ useApiFailuresStore }) => {
      const message = (err as Error)?.message || String(err)
      useApiFailuresStore().report(500, message)
    })
    .catch(() => {})
  // Do NOT re-throw — let the page degrade gracefully rather than
  // freezing the sidebar navigation.
}

app.config.warnHandler = (msg, _instance, trace) => {
  if (isNavDebugEnabled()) {
    console.warn('[Vue] Warning:', msg, trace)
  }
}

// Unhandled promise rejections (e.g. failed dynamic imports outside router)
window.addEventListener('unhandledrejection', (event) => {
  console.error('[buz] unhandledrejection:', event.reason)
})

if (isNavDebugEnabled()) {
  console.info(
    '[buz-nav] debug ON — disable with localStorage.removeItem("buz_debug_nav")',
  )
}

app.use(createPinia())
app.use(router)
app.mount('#app')
