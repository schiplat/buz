<script setup lang="ts">
import {
  Box,
  EyeOff,
  Globe,
  KeyRound,
  Layers,
  PanelLeft,
  ScrollText,
  Shield,
  Server,
  Users,
  LayoutDashboard as OverviewIcon,
  Package,
} from '@lucide/vue'
import { useRoute, useRouter } from 'vue-router'
import { computed, onMounted, ref } from 'vue'
import { cn } from '@/lib/utils'
import { useUiStore } from '@/stores/ui'
import { useAuthStore } from '@/stores/auth'
import { safeNavigate, navLog } from '@/utils/navigation'
import { healthApi } from '@/api/health'

const ui = useUiStore()
const auth = useAuthStore()
const route = useRoute()
const router = useRouter()

type NavLink = {
  kind: 'link'
  name: string
  label: string
  icon: typeof OverviewIcon
  to: string
  adminOnly?: boolean
  opsOnly?: boolean
}

type NavSection = {
  kind: 'section'
  label: string
}

type NavEntry = NavLink | NavSection

const allEntries: NavEntry[] = [
  { kind: 'link', name: 'dashboard', label: 'Overview', icon: OverviewIcon, to: '/dashboard' },
  { kind: 'section', label: 'Infrastructure' },
  { kind: 'link', name: 'runners', label: 'Runners', icon: Server, to: '/runners/list', adminOnly: true },
  { kind: 'link', name: 'hosts', label: 'SSH Targets', icon: Globe, to: '/hosts' },
  { kind: 'link', name: 'environments', label: 'Environments', icon: Layers, to: '/environments' },
  { kind: 'link', name: 'secrets-variables', label: 'Secrets & Variables', icon: EyeOff, to: '/secrets-variables' },
  { kind: 'link', name: 'plugin-library', label: 'Plugins', icon: Package, to: '/plugin-library', adminOnly: true },
  { kind: 'section', label: 'Automation' },
  { kind: 'link', name: 'pipelines', label: 'Pipelines', icon: Box, to: '/pipelines' },
  { kind: 'section', label: 'Access' },
  { kind: 'link', name: 'users', label: 'Users & teams', icon: Users, to: '/users' },
  { kind: 'link', name: 'api-tokens', label: 'API tokens', icon: KeyRound, to: '/api-tokens', adminOnly: true },
  { kind: 'link', name: 'audit-logs', label: 'Audit logs', icon: ScrollText, to: '/audit-logs', adminOnly: true },
  { kind: 'link', name: 'sso-settings', label: 'SSO', icon: Shield, to: '/settings/sso', adminOnly: true },
]

const entries = computed(() =>
  allEntries.filter((e) => {
    if (e.kind === 'section') return true
    if (e.adminOnly && !auth.isPlatformAdmin) return false
    if (e.opsOnly && !auth.isOps && !auth.isPlatformAdmin) return false
    return true
  }),
)

const uiVersion = __APP_VERSION__
const managerVersion = ref<string | null>(null)

const REPO_URL = 'https://github.com/schiplat/buz'
const DOCS_URL = 'https://buz.docs.sconts.com/'

// Manager version is authoritative for the badge; the UI package version is
// only a fallback (offline / starting) and the tooltip when they differ.
const versionLabel = computed(() => `v${managerVersion.value || uiVersion}`)
const versionTitle = computed(() =>
  managerVersion.value && managerVersion.value !== uiVersion
    ? `UI v${uiVersion} · Manager v${managerVersion.value}`
    : `v${uiVersion}`,
)

onMounted(async () => {
  try {
    const info = await healthApi.get()
    managerVersion.value = info.version
  } catch {
    /* offline / starting — fall back to UI package version */
  }
})

function isActive(name: string) {
  if (name === 'pipelines') {
    return (
      route.name === 'pipelines' ||
      route.name === 'pipeline-detail' ||
      route.name === 'job-detail'
    )
  }
  if (name === 'runners') {
    return (
      route.name === 'runners' ||
      route.name === 'runners-list' ||
      route.name === 'sandbox-hosts'
    )
  }
  if (name === 'environments') return route.name === 'environments' || route.name === 'environment-detail'
  if (name === 'secrets-variables') {
    return (
      route.name === 'secrets-variables' ||
      route.name === 'secrets' ||
      route.name === 'variables' ||
      route.name === 'github-sync'
    )
  }
  return route.name === name
}

async function onNavClick(e: MouseEvent, to: string) {
  // Let the browser handle modified clicks (new tab, etc.)
  if (e.defaultPrevented) return
  if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey || e.button !== 0) return
  e.preventDefault()
  navLog('sidebar click', to)
  await safeNavigate(router, to)
}

function onLogoClick(e: MouseEvent) {
  if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey || e.button !== 0) return
  e.preventDefault()
  void safeNavigate(router, '/')
}
</script>

<template>
  <aside
    :class="
      cn(
        'sticky top-0 z-20 flex h-screen shrink-0 flex-col bg-sidebar text-sidebar-foreground',
        'transition-[width] duration-200 ease-out',
        ui.isCollapsed ? 'w-17' : 'w-62',
      )
    "
  >
    <!-- Logo area -->
    <div
      :class="
        cn(
          'flex h-14 items-center px-3',
          ui.isCollapsed ? 'justify-center' : 'justify-between gap-2',
        )
      "
    >
      <a
        href="/"
        class="group flex min-w-0 items-center gap-2.5 rounded-xl px-1 py-1 text-left outline-hidden focus-visible:ring-2 focus-visible:ring-ring/25"
        :title="ui.isCollapsed ? 'Buz' : undefined"
        @click="onLogoClick"
      >
        <span
          class="flex h-8 w-8 shrink-0 items-center justify-center rounded-xl bg-primary text-xs font-semibold tracking-wide text-primary-foreground"
        >
          Bz
        </span>
        <span v-if="!ui.isCollapsed" class="min-w-0">
          <span class="block truncate text-sm font-semibold tracking-tight">Buz</span>
          <span class="block truncate text-xs text-muted-foreground">Orchestration</span>
        </span>
      </a>
      <button
        v-if="!ui.isCollapsed"
        type="button"
        class="inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-foreground"
        title="Collapse sidebar (⌘B)"
        @click="ui.toggleSidebar()"
      >
        <PanelLeft class="h-4 w-4" />
      </button>
    </div>

    <!-- Navigation -->
    <nav class="scrollbar-sidebar flex flex-1 flex-col gap-0.5 overflow-y-auto px-2.5 pt-4 pb-3">
      <template
        v-for="(entry, idx) in entries"
        :key="entry.kind === 'section' ? `s-${entry.label}` : entry.name"
      >
        <p
          v-if="entry.kind === 'section' && !ui.isCollapsed"
          :class="cn('px-2.5 type-eyebrow', idx === 0 ? 'mb-1.5' : 'mb-1.5 mt-3')"
        >
          {{ entry.label }}
        </p>
        <div
          v-else-if="entry.kind === 'section' && ui.isCollapsed"
          class="my-1.5 mx-auto h-px w-6 bg-sidebar-border"
        />
        <a
          v-else-if="entry.kind === 'link'"
          :href="entry.to"
          :title="ui.isCollapsed ? entry.label : undefined"
          :class="
            cn(
              'group flex items-center gap-3 rounded-xl px-2.5 py-2 text-sm font-medium text-muted-foreground transition-colors',
              'hover:bg-sidebar-accent hover:text-foreground',
              ui.isCollapsed && 'justify-center px-0',
              isActive(entry.name) && 'bg-sidebar-accent text-foreground',
            )
          "
          @click="onNavClick($event, entry.to)"
        >
          <component
            :is="entry.icon"
            :class="
              cn(
                'h-4 w-4 shrink-0',
                isActive(entry.name)
                  ? 'text-foreground'
                  : 'text-muted-foreground group-hover:text-foreground',
              )
            "
          />
          <span v-if="!ui.isCollapsed">{{ entry.label }}</span>
        </a>
      </template>
    </nav>

    <!-- Footer -->
    <div v-if="!ui.isCollapsed" class="px-4 pb-4">
      <p
        class="flex items-center gap-1.5 whitespace-nowrap px-1 text-xs leading-relaxed text-muted-foreground/60"
      >
        <a
          :href="REPO_URL"
          target="_blank"
          rel="noopener noreferrer"
          class="transition-colors hover:text-foreground"
        >
          Powered by Buz <span aria-hidden="true">↗</span>
        </a>
        <span aria-hidden="true">·</span>
        <span :title="versionTitle">{{ versionLabel }}</span>
        <span aria-hidden="true">·</span>
        <a
          :href="DOCS_URL"
          target="_blank"
          rel="noopener noreferrer"
          class="transition-colors hover:text-foreground"
        >
          Docs <span aria-hidden="true">↗</span>
        </a>
      </p>
    </div>
  </aside>
</template>
