<script setup lang="ts">
import type { PoolNotice } from '@/utils/poolNotices'

/** One line per pool problem: the fact in its tone colour, the remedy muted.
 *  Rendering lives here so every caller shows the same message body — the
 *  pages only supply `{ tone, summary, hint }`. */
withDefaults(defineProps<{ notices: PoolNotice[]; compact?: boolean }>(), { compact: false })
</script>

<template>
  <ul v-if="notices.length" class="m-0 list-none space-y-1 p-0" :class="compact ? 'mt-1' : 'mt-1.5'">
    <li
      v-for="(notice, index) in notices"
      :key="index"
      class="flex items-start gap-1.5 text-[11px] leading-snug"
    >
      <span
        class="mt-1.5 h-1 w-1 shrink-0 rounded-full"
        :class="notice.tone === 'danger' ? 'bg-red-500' : 'bg-amber-500'"
        aria-hidden="true"
      ></span>
      <p class="m-0">
        <span
          class="font-medium"
          :class="
            notice.tone === 'danger'
              ? 'text-red-600 dark:text-red-400'
              : 'text-amber-600 dark:text-amber-400'
          "
        >{{ notice.summary }}</span>
        <span class="text-muted-foreground"> — {{ notice.hint }}</span>
      </p>
    </li>
  </ul>
</template>
