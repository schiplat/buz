import { afterEach, describe, expect, it, vi } from 'vitest'
import { createApp, h, ref } from 'vue'
import PipelineYamlEditorDialog from '@/components/pipeline/PipelineYamlEditorDialog.vue'

vi.mock('@/api/pipelines', () => ({
  pipelinesApi: {
    listYamlFiles: vi.fn().mockResolvedValue({ scan_dir: '', files: [] }),
  },
}))

vi.mock('@/api/secrets', () => ({
  secretsApi: { list: vi.fn().mockResolvedValue([]) },
}))

vi.mock('@/api/variables', () => ({
  variablesApi: { list: vi.fn().mockResolvedValue([]) },
}))

vi.mock('@/components/pipeline/PipelineYamlCodeEditor.vue', () => ({
  default: { template: '<div />' },
}))

function mountDialog() {
  const el = document.createElement('div')
  document.body.appendChild(el)
  const dialogRef = ref<any>(null)
  const app = createApp({
    render: () => h(PipelineYamlEditorDialog, { ref: dialogRef }),
  })
  app.mount(el)
  return {
    el,
    openCreate: () => dialogRef.value.openCreate(),
    unmount: () => {
      app.unmount()
      el.remove()
    },
  }
}

afterEach(() => {
  localStorage.clear()
  document.body.innerHTML = ''
  vi.clearAllMocks()
})

describe('PipelineYamlEditorDialog', () => {
  it('includes pinned examples for common marketplace steps', async () => {
    localStorage.removeItem('buz_guide_new_pipeline_yaml_ref')
    const mounted = mountDialog()

    await mounted.openCreate()
    const buttons = [...document.body.querySelectorAll<HTMLButtonElement>('button')]
    buttons.find((button) => button.textContent?.trim() === 'Quick YAML Reference')?.click()

    const reference = document.body.textContent || ''
    expect(reference).toContain('local step index')
    expect(reference).toContain('buz/http-check@1.0.0')
    expect(reference).toContain('buz/slack@1.0.0')
    expect(reference).toContain('buz/oss-upload@1.0.2')
    expect(reference).toContain('access_key_id: ${secrets.OSS_ACCESS_KEY_ID}')
    expect(reference).toContain('access_key_secret: ${secrets.OSS_ACCESS_KEY_SECRET}')

    mounted.unmount()
  })

  it('keeps the safe plugin marketplace link available on both tabs', async () => {
    localStorage.removeItem('buz_guide_new_pipeline_yaml_ref')
    const mounted = mountDialog()

    await mounted.openCreate()

    const link = document.body.querySelector<HTMLAnchorElement>(
      'a[href="https://plugins.buz.schiplat.com"]',
    )
    expect(link).not.toBeNull()
    expect(link?.textContent?.trim()).toBe('Plugin Marketplace')
    expect(link?.target).toBe('_blank')
    expect(link?.rel).toBe('noopener noreferrer')

    const buttons = [...document.body.querySelectorAll<HTMLButtonElement>('button')]
    buttons.find((button) => button.textContent?.trim() === 'Quick YAML Reference')?.click()
    expect(link?.isConnected).toBe(true)

    buttons.find((button) => button.textContent?.trim() === 'Editor')?.click()
    expect(link?.isConnected).toBe(true)

    mounted.unmount()
  })
})
