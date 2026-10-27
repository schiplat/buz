import { CompletionContext, type CompletionResult } from '@codemirror/autocomplete'
import { EditorState } from '@codemirror/state'
import { describe, expect, it } from 'vitest'
import { createPipelineYamlCompletionSource } from '../pipelineYamlCompletions'

const pluginRefs = [
  { uses: 'buz/checkout@1.2.0', reviewed: true, description: 'Clone the repository' },
  { uses: 'buz/docker@1.1.0', reviewed: true, description: 'Docker build with docker-socket support' },
  { uses: 'buz/docker-login@1.1.0', reviewed: false },
  { uses: 'buz/config-fetch@1.1.0', reviewed: true },
]

const source = createPipelineYamlCompletionSource({
  secretNames: ['REGISTRY_PASSWORD', 'CONFIG_BUNDLE_URL'],
  variableNames: ['IMAGE_TAG'],
  environmentSlugs: ['staging', 'production'],
  pluginRefs,
})

function complete(doc: string, explicit = true): CompletionResult | null {
  return source(new CompletionContext(EditorState.create({ doc }), doc.length, explicit))
}

function labels(doc: string) {
  return complete(doc)?.options.map((option) => option.label) || []
}

function apply(doc: string, label: string) {
  const result = complete(doc)!
  const option = result.options.find((item) => item.label === label)!
  const text = typeof option.apply === 'string' ? option.apply : option.label
  return doc.slice(0, result.from) + text
}

describe('plugin-first pipeline YAML completions', () => {
  it('keeps step and render fields without suggesting deprecated kernel delivery fields', () => {
    for (const doc of ['steps:\n  - name: deploy\n    ', 'steps:\n  - name: deploy\n    config:\n      ']) {
      const options = labels(doc)
      expect(options).toEqual(expect.arrayContaining(['uses', 'with', 'config', 'dest', 'render', 'template', 'output']))
      for (const field of ['registry_secret', 'source', 'key_path', 'capabilities']) expect(options).not.toContain(field)
    }
    const config = complete('steps:\n  - name: deploy\n    ')!.options.find((option) => option.label === 'config')!
    expect(config.info).toContain('buz/config-fetch')
  })

  it.each(['', 'buz/', 'buz/docker', 'buz/docker-login@', 'buz/config-fetch@1.']) (
    'replaces the full uses reference for prefix %s', (prefix) => {
      const doc = `steps:\n  - uses: ${prefix}`
      // The list is live data (GET /plugins), not a hardcoded catalog: the
      // options are exactly what the Manager reports.
      expect(labels(doc)).toEqual(pluginRefs.map((plugin) => plugin.uses))
      const docker = complete(doc)!.options.find((option) => option.label === 'buz/docker@1.1.0')!
      expect(docker.info).toContain('reviewed')
      expect(docker.info).toContain('docker-socket')
      const unreviewed = complete(doc)!.options.find((option) => option.label === 'buz/docker-login@1.1.0')!
      expect(unreviewed.info).toContain('not reviewed')
      expect(apply(doc, 'buz/docker-login@1.1.0')).toBe('steps:\n  - uses: buz/docker-login@1.1.0')
      expect((complete(doc)!.validFor as RegExp).test('buz/docker-login@1.1.0')).toBe(true)
    },
  )

  it.each(['engine:', 'engine: ', 'engine: sh'])('preserves the engine key for %s', (line) => {
    const doc = `steps:\n  - name: deploy\n    ${line}`
    expect(labels(doc)).toEqual(['shell', 'docker', 'ssh'])
    expect(apply(doc, 'ssh')).toBe(doc.slice(0, complete(doc)!.from) + 'ssh')
    expect(apply(doc, 'ssh')).toContain('engine:')
  })

  it('describes run as the uses-step extension point and engine as a scoped override', () => {
    const stepOptions = complete('steps:\n  - name: deploy\n    ')!.options
    const run = stepOptions.find((option) => option.label === 'run')!
    const uses = stepOptions.find((option) => option.label === 'uses')!
    const engine = stepOptions.find((option) => option.label === 'engine')!
    expect(run.info).toContain('appended after the template run block')
    expect(uses.info).toContain('run: appends')
    expect(uses.info).toContain('image stays template-owned')
    expect(engine.info).toContain('within its declared engines')
    const workDir = stepOptions.find((option) => option.label === 'work_dir')!
    expect(workDir.info).toContain('default_dir')
  })

  it('describes each suggestion from the plugin metadata the Manager reports', () => {
    const doc = 'steps:\n  - uses: '
    const checkout = complete(doc)!.options.find((option) => option.label === 'buz/checkout@1.2.0')!
    // Descriptions come from the plugin spec (index meta), not a hardcoded
    // catalog in the dashboard — the catalog went stale once already.
    expect(checkout.info).toContain('reviewed')
    expect(checkout.info).toContain('Clone the repository')

    const unreviewed = complete(doc)!.options.find((option) => option.label === 'buz/docker-login@1.1.0')!
    expect(unreviewed.info).toContain('not reviewed')
  })

  it('offers nothing when the Manager reports no plugins', () => {
    const empty = createPipelineYamlCompletionSource({})
    const doc = 'steps:\n  - uses: '
    const result = empty(new CompletionContext(EditorState.create({ doc }), doc.length, true))
    const options = result?.options.map((option) => option.label) ?? []
    expect(options).not.toContain('buz/checkout@1.2.0')
  })

  it.each(['', 'REG'])('preserves the Secret expression when completing %s', (prefix) => {
    const doc = `steps:\n  - uses: buz/docker-login@1.1.0\n    with:\n      password: \${secrets.${prefix}`
    expect(labels(doc)).toEqual(['REGISTRY_PASSWORD', 'CONFIG_BUNDLE_URL'])
    expect(apply(doc, 'REGISTRY_PASSWORD')).toContain('password: ${secrets.REGISTRY_PASSWORD')
  })

  it('permits ${env.*} dynamic references in with values', () => {
    const prefix = 'steps:\n  - uses: buz/docker@1.1.0\n    with:\n      tags: ${'
    expect(labels(prefix)).toEqual(expect.arrayContaining(['secrets.', 'vars.', 'env.']))
    const envOption = complete(prefix)!.options.find((option) => option.label === 'env.')!
    expect(envOption.info).toContain('$BUZ_ENV')
    const withEnv = apply(prefix, 'env.')
    expect(withEnv).toBe('steps:\n  - uses: buz/docker@1.1.0\n    with:\n      tags: ${env.')
    // Dynamic keys resolve on the Runner — the Manager completes nothing mid-reference.
    expect(complete(`${withEnv}.`)).toBeNull()
  })

  it('preserves variable and expression prefixes', () => {
    const variable = 'env:\n  IMAGE: ${vars.IM'
    expect(labels(variable)).toEqual(['IMAGE_TAG'])
    expect(apply(variable, 'IMAGE_TAG')).toBe('env:\n  IMAGE: ${vars.IMAGE_TAG')
    for (const prefix of ['${', '${sec']) {
      expect(apply(`env:\n  TOKEN: ${prefix}`, 'secrets.')).toBe('env:\n  TOKEN: ${secrets.')
    }
  })

  it('does not treat input names ending in uses or engine as syntax fields', () => {
    expect(labels('steps:\n  - name: deploy\n    with:\n      my_uses: buz/')).not.toContain('buz/docker-login@1.1.0')
    expect(labels('steps:\n  - name: deploy\n    env:\n      my_engine: sh')).not.toEqual(['shell', 'docker', 'ssh'])
  })

  it('suggests only registered environment slugs in the top-level allowlist', () => {
    expect(labels('environments:\n  - ')).toEqual(['staging', 'production'])
    const production = complete('environments:\n  - ')!.options.find((option) => option.label === 'production')!
    expect(production.info).toContain('Registered environment slug')
  })

  it('keeps top-level fields and allowlist completions', () => {
    expect(labels('')).toContain('steps')
    expect(labels('')).toContain('environments')
    expect(labels('secrets:\n  - ')).toEqual(['REGISTRY_PASSWORD', 'CONFIG_BUNDLE_URL'])
    expect(labels('variables:\n  - ')).toEqual(['IMAGE_TAG'])
    expect(complete('steps:\n  - name: deploy\n    ', false)).toBeNull()
  })
})
