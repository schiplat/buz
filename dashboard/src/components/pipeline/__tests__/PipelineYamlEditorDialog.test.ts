import { afterEach, describe, expect, it, vi } from 'vitest'
import { createApp, h, nextTick, ref } from 'vue'
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
  it('recommends only plugin-based authentication and config delivery, followed by render-only', async () => {
    const mounted = mountDialog()
    await mounted.openCreate()
    const buttons = [...document.body.querySelectorAll<HTMLButtonElement>('button')]
    buttons.find((button) => button.textContent?.trim() === 'Quick YAML Reference')?.click()
    await nextTick()
    const text = document.body.textContent || ''
    const reference = document.body.querySelector('pre')?.textContent || ''
    expect(text).toContain('Prefer a pinned uses: plugin')
    for (const deprecated of ['config.source', 'key_path', 'purpose slot', 'Manager builds {prefix}']) {
      expect(reference).not.toContain(deprecated)
    }
    expect(reference).toContain('uses: buz/docker-login@1.1.0')
    expect(reference).toContain('registry: ${secrets.REGISTRY_HOST}')
    expect(reference).toContain('username: ${secrets.REGISTRY_USERNAME}')
    expect(reference).toContain('password: ${secrets.REGISTRY_PASSWORD}')
    expect(reference.indexOf('uses: buz/docker-login@1.1.0')).toBeLessThan(reference.indexOf('name: Run Private Image'))
    expect(reference).toContain('uses: buz/docker-logout@1.0.0')
    expect(reference).toContain('uses: buz/config-fetch@1.1.0')
    expect(reference).toContain('url: ${secrets.CONFIG_BUNDLE_URL}')
    expect(reference).toContain('with:\n      url: ${secrets.CONFIG_BUNDLE_URL}')
    expect(reference).toContain('config:\n      dest: config\n      render:')
    expect(reference).toContain('with.dest is workspace-relative')
    expect(reference).toContain('Local fetch/render above does not transfer files')
    expect(reference).not.toMatch(/uses:[^\n]+\n\s+engine:/)
    expect(reference).not.toMatch(/debug:\s*["']?true/)
    expect(text).toContain('appended after the template')
    expect(text).toContain('rejected by the validator')
    expect(text).toContain('overrides the template')
    expect(reference).toContain('source: dist/app.tar.gz')
    for (const plugin of ['buz/checkout@1.2.0', 'buz/setup-rust@1.1.0', 'buz/setup-node@1.1.0', 'buz/docker@1.1.0', 'buz/docker-compose@1.0.0']) {
      expect(reference).toContain(`uses: ${plugin}`)
    }
    expect(reference).toContain('token: ${secrets.GIT_TOKEN}')
    expect(reference).toContain('project_dir: /opt/app')
    expect(reference).not.toContain('uses: buz/git-clone@1.0.0')
    expect(reference).not.toContain('uses: buz/setup-rust@1.0.0')
    expect(reference).not.toContain('uses: buz/setup-node@1.0.0')
    expect(reference).not.toContain('uses: buz/docker@1.0.0')
    expect(reference).toContain('into: source')
    expect(reference).toContain('into: private-source')
    expect(reference).toContain('work_dir: source')
    expect(reference).toContain('work_dir: source/frontend')
    expect(reference).toContain('default_dir')
    expect(reference).toMatch(/work_dir: source[^\n]*\n\s+with:\n\s+context: \./)
    expect(reference).toContain('>> "$BUZ_ENV"')
    expect(reference).toContain('tags: myapp:${env.IMAGE_TAG}')
    expect(reference).toContain('${env.KEY}')
    expect(reference).toContain('docker_socket_allowed')
    expect(reference).toContain('Runner-admin authorization is separate')
    expect(text).toContain('Runner administrator')
    expect(text).toContain('docker_socket_allowed = true')
    expect(reference).not.toContain('/var/run/docker.sock')
    expect(reference).not.toContain('not auth for the container\'s Docker CLI')
    for (const forbidden of ['git clone --depth', 'docker push ', 'docker compose up', 'StrictHostKeyChecking=no', 'curl -X POST', '# notify:', 'capabilities: [docker-socket]']) {
      expect(reference).not.toContain(forbidden)
    }
    mounted.unmount()
  })

  it('includes pinned examples for common marketplace steps', async () => {
    localStorage.removeItem('buz_guide_new_pipeline_yaml_ref')
    const mounted = mountDialog()

    await mounted.openCreate()
    const buttons = [...document.body.querySelectorAll<HTMLButtonElement>('button')]
    buttons.find((button) => button.textContent?.trim() === 'Quick YAML Reference')?.click()

    const reference = document.body.textContent || ''
    expect(reference).toContain('local step index')
    expect(reference).toContain('buz/http-check@1.1.0')
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
