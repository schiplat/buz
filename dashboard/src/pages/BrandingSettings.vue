<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Image as ImageIcon, RotateCcw, Upload } from '@lucide/vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import BrandMark from '@/components/brand/BrandMark.vue'
import { useBrandingStore } from '@/stores/branding'

// Branding settings, layout follows the host project's Branding view: one
// section, a preview at the true display size, and the actions beside it.
// Upload applies immediately (one PUT, no save step); reset is likewise
// immediate — removing an upload is reversible by uploading again.
// The built-in mark is always the default; nothing here renames the product
// or changes the theme.
const branding = useBrandingStore()

const fileInput = ref<HTMLInputElement | null>(null)
const uploading = ref(false)
const resetting = ref(false)
const errors = ref<string[]>([])
const notice = ref<{ tone: 'success' | 'danger'; text: string } | null>(null)

const ALLOWED_TYPES = new Set(['image/svg+xml', 'image/png', 'image/jpeg', 'image/webp'])
const MAX_BYTES = computed(() => branding.recommendations?.max_bytes ?? 512 * 1024)
const maxKb = computed(() => Math.round(MAX_BYTES.value / 1024))
const formats = computed(
  () => branding.recommendations?.formats.join(' · ') ?? 'SVG · PNG · WebP · JPEG',
)
const accept = '.svg,.png,.jpg,.jpeg,.webp,image/svg+xml,image/png,image/jpeg,image/webp'

const status = computed(() => {
  if (!branding.hasCustom) return 'Built-in mark'
  const info = branding.info
  const size = info?.size_bytes != null ? ` · ${formatBytes(info.size_bytes)}` : ''
  const by = info?.updated_by ? ` · saved by ${info.updated_by}` : ''
  return `Custom logo · ${info?.mime ?? 'image'}${size}${by}`
})

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  return `${Math.round(bytes / 1024)} KB`
}

function typeFromName(name: string): string {
  const ext = name.split('.').pop()?.toLowerCase() ?? ''
  switch (ext) {
    case 'svg':
      return 'image/svg+xml'
    case 'png':
      return 'image/png'
    case 'jpg':
    case 'jpeg':
      return 'image/jpeg'
    case 'webp':
      return 'image/webp'
    default:
      return ''
  }
}

function pickFile() {
  errors.value = []
  notice.value = null
  fileInput.value?.click()
}

async function onFileChange(event: Event) {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  input.value = '' // allow picking the same file again after a failure
  if (!file) return

  errors.value = []
  notice.value = null

  const type = file.type || typeFromName(file.name)
  if (!ALLOWED_TYPES.has(type)) {
    errors.value = ['Unsupported image type — use SVG, PNG, WebP or JPEG.']
    return
  }
  if (file.size > MAX_BYTES.value) {
    errors.value = [
      `That file is ${formatBytes(file.size)} — the maximum is ${maxKb.value} KB.`,
    ]
    return
  }

  uploading.value = true
  try {
    await branding.uploadLogo(file)
    notice.value = { tone: 'success', text: 'Logo updated — every operator sees it now.' }
  } catch (e: any) {
    // axios unwraps the shared envelope; a bare `{ code, message }` error
    // still carries the server's explanation on `.message`.
    const message =
      e?.response?.data?.message || e?.response?.data?.error || e?.message || ''
    errors.value = [message || 'The upload failed. Try again.']
  } finally {
    uploading.value = false
  }
}

async function resetLogo() {
  errors.value = []
  notice.value = null
  resetting.value = true
  try {
    await branding.reset()
    notice.value = { tone: 'success', text: 'Back to the built-in mark.' }
  } catch (e: any) {
    const message =
      e?.response?.data?.message || e?.response?.data?.error || e?.message || ''
    notice.value = {
      tone: 'danger',
      text: message || 'Removing the logo failed. Try again.',
    }
  } finally {
    resetting.value = false
  }
}

onMounted(() => {
  // Another administrator may have changed the logo since boot.
  void branding.refresh()
})
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="Branding"
      description="The logo shown in the sidebar and on sign-in pages. The built-in mark is the default."
    />

    <p
      v-if="notice"
      :class="notice.tone === 'success' ? 'notice-success' : 'notice-danger'"
      class="text-sm"
      role="status"
    >
      {{ notice.text }}
    </p>

    <section class="surface-card p-6">
      <div class="flex items-center gap-3">
        <span class="flex h-8 w-8 flex-none items-center justify-center rounded-lg bg-muted/40 text-muted-foreground">
          <ImageIcon class="h-4 w-4" />
        </span>
        <div class="min-w-0">
          <h2>Sidebar logo</h2>
          <p class="type-meta">
            A custom logo replaces the built-in mark for every operator of this instance.
          </p>
        </div>
      </div>

      <div class="mt-6 flex flex-col gap-6 sm:flex-row sm:items-start">
        <!-- Preview at the true display size. The sidebar's own background
             varies (light/dark), so faking it here would add a competing
             surface — a neutral inset is the honest backdrop. -->
        <div class="surface-inset flex shrink-0 flex-col items-center p-6">
          <BrandMark size="sm" />
          <p class="mt-2 text-center text-[11px] text-muted-foreground">Display size · 32 px</p>
        </div>

        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-3">
            <UiButton type="button" :disabled="uploading" @click="pickFile">
              <Upload class="h-4 w-4" />
              {{ uploading ? 'Uploading…' : 'Upload logo' }}
            </UiButton>
            <UiButton
              type="button"
              variant="ghost"
              class="text-destructive hover:bg-destructive/10"
              :disabled="resetting || !branding.hasCustom"
              @click="resetLogo"
            >
              <RotateCcw class="h-4 w-4" />
              {{ resetting ? 'Resetting…' : 'Reset to default' }}
            </UiButton>
            <input
              ref="fileInput"
              type="file"
              :accept="accept"
              class="hidden"
              @change="onFileChange"
            />
          </div>

          <p class="type-meta mt-3">{{ status }}</p>

          <div v-if="errors.length" class="notice-danger mt-4 text-sm" role="alert">
            <ul class="list-disc space-y-1 pl-4">
              <li v-for="error in errors" :key="error">{{ error }}</li>
            </ul>
          </div>

          <div class="surface-inset mt-4 p-3 text-xs leading-relaxed text-muted-foreground">
            <p class="font-medium text-foreground">Recommended</p>
            <ul class="mt-1 list-disc space-y-1 pl-4">
              <li>
                <span class="text-foreground">Type:</span> {{ formats }}; SVG or PNG with a
                transparent background preferred.
              </li>
              <li>
                <span class="text-foreground">Size:</span> 512 × 512 px square (minimum
                128 × 128 px) — the sidebar renders it at 32 px.
              </li>
              <li><span class="text-foreground">File:</span> up to {{ maxKb }} KB.</li>
              <li>
                <span class="text-foreground">Margin:</span>
                {{ branding.recommendations?.safe_margin ?? 'at least 10% on every side' }}.
              </li>
            </ul>
          </div>
        </div>
      </div>
    </section>
  </div>
</template>
