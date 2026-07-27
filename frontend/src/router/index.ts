import {
  createRouter,
  createWebHistory,
  type RouteRecordRaw,
} from 'vue-router'
import { useAuthStore } from '@/stores/auth'

const routes: RouteRecordRaw[] = [
  {
    path: '/login',
    name: 'login',
    component: () => import('@/pages/Login.vue'),
    meta: { public: true },
  },
  {
    path: '/',
    component: () => import('@/layouts/AppShell.vue'),
    children: [
      { path: '', redirect: '/dashboard' },
      { path: 'dashboard', name: 'dashboard', component: () => import('@/pages/Overview.vue'), meta: { title: 'Overview' } },
      { path: 'runners', name: 'runners', component: () => import('@/pages/Runners.vue'), meta: { title: 'Nodes' } },
      { path: 'pipelines', name: 'pipelines', component: () => import('@/pages/Pipelines.vue'), meta: { title: 'Pipelines' } },
      { path: 'pipelines/:id/:tab(stage|history|definition)?', name: 'pipeline-detail', component: () => import('@/pages/PipelineDetail.vue'), meta: { title: 'Pipeline Detail' } },
      { path: 'jobs', name: 'jobs', component: () => import('@/pages/Jobs.vue'), meta: { title: 'Jobs' } },
      { path: 'jobs/:id', name: 'job-detail', component: () => import('@/pages/JobDetail.vue'), meta: { title: 'Job Detail' } },
      { path: 'hosts', name: 'hosts', component: () => import('@/pages/Hosts.vue'), meta: { title: 'Hosts' } },
      { path: 'secrets', name: 'secrets', component: () => import('@/pages/Secrets.vue'), meta: { title: 'Secrets' } },
      { path: 'variables', name: 'variables', component: () => import('@/pages/Variables.vue'), meta: { title: 'Variables' } },
    ],
  },
]

export const router = createRouter({
  history: createWebHistory(),
  routes,
})

router.beforeEach(async (to) => {
  const auth = useAuthStore()
  if (to.meta.public) return true
  if (!auth.isAuthenticated) return { name: 'login', query: { redirect: to.fullPath } }
  return true
})
