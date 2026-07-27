<script setup lang="ts">
import {
  Box,
  EyeOff,
  Globe,
  PanelLeft,
  Puzzle,
  Server,
  LayoutDashboard as OverviewIcon,
} from '@lucide/vue'
import { useRoute, useRouter } from 'vue-router'
import { cn } from '@/lib/utils'
import { useUiStore } from '@/stores/ui'

const ui = useUiStore()
const route = useRoute()
const router = useRouter()

type NavLink = {
  kind: 'link'
  name: string
  label: string
  icon: typeof OverviewIcon
  to: string
}

type NavSection = {
  kind: 'section'
  label: string
}

type NavEntry = NavLink | NavSection

const entries: NavEntry[] = [
  { kind: 'link', name: 'dashboard', label: 'Overview', icon: OverviewIcon, to: '/dashboard' },
  { kind: 'section', label: 'Infrastructure' },
  { kind: 'link', name: 'runners', label: 'Nodes', icon: Server, to: '/runners' },
  { kind: 'link', name: 'hosts', label: 'Hosts', icon: Globe, to: '/hosts' },
  { kind: 'link', name: 'secrets', label: 'Secrets', icon: EyeOff, to: '/secrets' },
  { kind: 'link', name: 'variables', label: 'Variables', icon: Puzzle, to: '/variables' },
  { kind: 'section', label: 'Automation' },
  { kind: 'link', name: 'pipelines', label: 'Pipelines', icon: Box, to: '/pipelines' },
]

const copyrightYear = new Date().getFullYear()
const appVersion = __APP_VERSION__

function isActive(name: string) {
  if (name === 'pipelines') return route.name === 'pipelines' || route.name === 'pipeline-detail'
  return route.name === name
}
</script>

<template>
  <aside
    :class="
      cn(
        'sticky top-0 z-20 flex h-screen shrink-0 flex-col bg-sidebar text-sidebar-foreground',
        'transition-[width] duration-200 ease-out',
        ui.isCollapsed ? 'w-[4.25rem]' : 'w-[15.5rem]',
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
      <button
        type="button"
        class="group flex min-w-0 items-center gap-2.5 rounded-xl px-1 py-1 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring/25"
        :title="ui.isCollapsed ? 'Buz' : undefined"
        @click="router.push('/')"
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
      </button>
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
    <nav class="flex flex-1 flex-col gap-0.5 overflow-y-auto px-2.5 pt-4 pb-3">
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
        <RouterLink
          v-else-if="entry.kind === 'link'"
          :to="entry.to"
          :title="ui.isCollapsed ? entry.label : undefined"
          :class="
            cn(
              'group flex items-center gap-3 rounded-xl px-2.5 py-2 text-sm font-medium text-muted-foreground transition-colors',
              'hover:bg-sidebar-accent hover:text-foreground',
              ui.isCollapsed && 'justify-center px-0',
              isActive(entry.name) && 'bg-sidebar-accent text-foreground',
            )
          "
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
        </RouterLink>
      </template>
    </nav>

    <!-- Footer -->
    <div v-if="!ui.isCollapsed" class="px-4 pb-4">
      <p class="px-1 text-xs leading-relaxed text-muted-foreground/80">
        &copy; {{ copyrightYear }} Buz
      </p>
      <p class="px-1 font-mono text-xs text-muted-foreground/60">
        v{{ appVersion }}
      </p>
    </div>
  </aside>
</template>
