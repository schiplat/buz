import {
  createRouter,
  createWebHistory,
  type RouteRecordRaw,
} from 'vue-router'
import { useAuthStore } from '@/stores/auth'
import { isNavDebugEnabled, navLog } from '@/utils/navigation'
import { pageTitle } from '@/utils/documentTitle'

const routes: RouteRecordRaw[] = [
  {
    path: '/setup',
    name: 'setup',
    component: () => import('@/pages/Setup.vue'),
    meta: { public: true, title: 'Setup' },
  },
  {
    path: '/login',
    name: 'login',
    component: () => import('@/pages/Login.vue'),
    meta: { public: true, title: 'Sign in' },
  },
  {
    path: '/',
    component: () => import('@/layouts/AppShell.vue'),
    children: [
      { path: '', redirect: '/dashboard' },
      { path: 'dashboard', name: 'dashboard', component: () => import('@/pages/Overview.vue'), meta: { title: 'Overview' } },
      { path: 'runners', name: 'runners', component: () => import('@/pages/Runners.vue'), meta: { title: 'Runners', adminOnly: true } },
      { path: 'pipelines', name: 'pipelines', component: () => import('@/pages/Pipelines.vue'), meta: { title: 'Pipelines' } },
      { path: 'pipelines/:id/:tab(stage|history|definition|trigger-events)?', name: 'pipeline-detail', component: () => import('@/pages/PipelineDetail.vue'), meta: { title: 'Pipeline Detail' } },
      { path: 'jobs', name: 'jobs', component: () => import('@/pages/Jobs.vue'), meta: { title: 'Jobs' } },
      { path: 'jobs/:id', name: 'job-detail', component: () => import('@/pages/JobDetail.vue'), meta: { title: 'Job Detail' } },
      { path: 'hosts', name: 'hosts', component: () => import('@/pages/Hosts.vue'), meta: { title: 'Hosts' } },
      { path: 'environments', name: 'environments', component: () => import('@/pages/Environments.vue'), meta: { title: 'Environments' } },
      {
        path: 'environments/:id',
        name: 'environment-detail',
        component: () => import('@/pages/EnvironmentDetail.vue'),
        meta: { title: 'Environment Detail' },
      },
      // Backwards-compatible aliases for previous tabbed detail URLs.
      {
        path: 'environments/:id/protection',
        redirect: (to) => ({ name: 'environment-detail', params: { id: to.params.id } }),
      },
      {
        path: 'environments/:id/overrides',
        redirect: (to) => ({ name: 'environment-detail', params: { id: to.params.id } }),
      },
      {
        path: 'environments/:id/variables',
        redirect: (to) => ({ name: 'environment-detail', params: { id: to.params.id } }),
      },
      {
        path: 'environments/:id/secrets',
        redirect: (to) => ({ name: 'environment-detail', params: { id: to.params.id } }),
      },
      {
        path: 'secrets-variables',
        name: 'secrets-variables',
        component: () => import('@/pages/SecretsVariables.vue'),
        meta: { title: 'Secrets & Variables' },
        children: [
          { path: '', redirect: { name: 'secrets' } },
          { path: 'secrets', name: 'secrets', component: () => import('@/pages/Secrets.vue'), meta: { title: 'Secrets' } },
          { path: 'variables', name: 'variables', component: () => import('@/pages/Variables.vue'), meta: { title: 'Variables' } },
          { path: 'github-sync', name: 'github-sync', component: () => import('@/pages/GitHubSync.vue'), meta: { title: 'GitHub Sync', adminOnly: true } },
        ],
      },
      // Backwards-compatible aliases for the pre-merge routes.
      { path: 'secrets', redirect: '/secrets-variables/secrets' },
      { path: 'variables', redirect: '/secrets-variables/variables' },
      { path: 'teams', name: 'teams', component: () => import('@/pages/Teams.vue'), meta: { title: 'Teams' } },
      { path: 'users', name: 'users', component: () => import('@/pages/Users.vue'), meta: { title: 'Users', adminOnly: true } },
      { path: 'api-tokens', name: 'api-tokens', component: () => import('@/pages/ApiTokens.vue'), meta: { title: 'API tokens', adminOnly: true } },
      { path: 'audit-logs', name: 'audit-logs', component: () => import('@/pages/AuditLogs.vue'), meta: { title: 'Audit logs', adminOnly: true } },
      {
        path: 'plugin-library',
        name: 'plugin-library',
        component: () => import('@/pages/PluginLibrary.vue'),
        meta: { title: 'Plugin library', adminOnly: true },
        children: [
          { path: '', redirect: { name: 'plugin-pool' } },
          { path: 'pool', name: 'plugin-pool', component: () => import('@/pages/PluginPool.vue'), meta: { title: 'Plugins' } },
          { path: 'sources', name: 'plugin-sources', component: () => import('@/pages/PluginSources.vue'), meta: { title: 'Sources & Settings' } },
        ],
      },
      { path: 'admin-settings', redirect: '/plugin-library/sources' },
      // Catch-all: unknown URLs get a friendly 404 inside the app shell
      // (sidebar/topnav keep working) instead of a blank page.
      {
        path: ':pathMatch(.*)*',
        name: 'not-found',
        component: () => import('@/pages/NotFound.vue'),
        meta: { title: 'Page not found' },
      },
    ],
  },
]

export const router = createRouter({
  history: createWebHistory(),
  routes,
})

let setupChecked = false
let needsSetupCache = false

/** Call after successful first-admin setup so login is allowed. */
export function markSetupComplete() {
  setupChecked = true
  needsSetupCache = false
}

async function ensureSetupGate(toName: string | symbol | null | undefined) {
  // Re-check when entering auth pages so a completed setup is visible after refresh.
  const forceCheck = toName === 'setup' || toName === 'login' || !setupChecked
  if (forceCheck) {
    try {
      const { authApi } = await import('@/api/auth')
      const s = await authApi.setupStatus()
      needsSetupCache = s.needs_setup
      setupChecked = true
    } catch {
      return null
    }
  }
  if (needsSetupCache && toName !== 'setup') {
    return { name: 'setup' as const }
  }
  if (!needsSetupCache && toName === 'setup') {
    return { name: 'login' as const }
  }
  return null
}

router.beforeEach(async (to, from) => {
  navLog('beforeEach', { from: from.fullPath, to: to.fullPath })
  const gate = await ensureSetupGate(to.name)
  if (gate) return gate

  const auth = useAuthStore()
  if (to.meta.public) return true
  if (!auth.isAuthenticated) {
    return { name: 'login', query: { redirect: to.fullPath } }
  }
  if (to.meta.adminOnly && !auth.isPlatformAdmin) {
    return { name: 'dashboard' }
  }
  return true
})

router.afterEach((to, from, failure) => {
  if (failure) {
    navLog('afterEach failure', { to: to.fullPath, failure })
  } else {
    navLog('afterEach ok', { from: from.fullPath, to: to.fullPath })
  }

  // Per-route tab title: `{page} · Buz` (falls back to `Buz`).
  const title = typeof to.meta.title === 'string' ? to.meta.title : undefined
  document.title = pageTitle(title)
})

router.onError((error, to) => {
  console.error('[buz-nav] router.onError', error)
  const msg = String((error as Error)?.message || error)
  const isChunkError =
    msg.includes('Failed to fetch') ||
    msg.includes('Loading chunk') ||
    msg.includes('Loading CSS chunk') ||
    msg.includes('dynamically imported module') ||
    msg.includes('Importing a module script failed')

  if (isChunkError && to) {
    console.warn('[buz-nav] chunk/load error — hard reload', to.fullPath)
    // Avoid reload loops: only hard-nav once per target within a short window.
    const key = `buz_nav_reload:${to.fullPath}`
    const last = Number(sessionStorage.getItem(key) || 0)
    if (Date.now() - last > 10_000) {
      sessionStorage.setItem(key, String(Date.now()))
      window.location.assign(to.fullPath)
    }
  } else if (isNavDebugEnabled()) {
    console.debug('[buz-nav] non-chunk router error (no hard reload)')
  }
})
