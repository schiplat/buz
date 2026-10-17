import { CompletionContext, type CompletionResult } from '@codemirror/autocomplete'
import { EditorState } from '@codemirror/state'
import { describe, expect, it } from 'vitest'
import { createPipelineYamlCompletionSource } from '../pipelineYamlCompletions'

const source = createPipelineYamlCompletionSource({
  secretNames: ['REGISTRY_PASSWORD', 'CONFIG_BUNDLE_URL'],
  variableNames: ['IMAGE_TAG'],
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
      for (const field of ['registry_secret', 'source', 'key_path']) expect(options).not.toContain(field)
    }
    const config = complete('steps:\n  - name: deploy\n    ')!.options.find((option) => option.label === 'config')!
    expect(config.info).toContain('buz/config-fetch')
  })

  it.each(['', 'buz/', 'buz/docker', 'buz/docker-login@', 'buz/config-fetch@1.']) (
    'replaces the full uses reference for prefix %s', (prefix) => {
      const doc = `steps:\n  - uses: ${prefix}`
      expect(labels(doc)).toEqual(['buz/docker-login@1.1.0', 'buz/docker-logout@1.0.0', 'buz/config-fetch@1.1.0'])
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

  it.each(['', 'REG'])('preserves the Secret expression when completing %s', (prefix) => {
    const doc = `steps:\n  - uses: buz/docker-login@1.1.0\n    with:\n      password: \${secrets.${prefix}`
    expect(labels(doc)).toEqual(['REGISTRY_PASSWORD', 'CONFIG_BUNDLE_URL'])
    expect(apply(doc, 'REGISTRY_PASSWORD')).toContain('password: ${secrets.REGISTRY_PASSWORD')
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

  it('keeps top-level fields and allowlist completions', () => {
    expect(labels('')).toContain('steps')
    expect(labels('secrets:\n  - ')).toEqual(['REGISTRY_PASSWORD', 'CONFIG_BUNDLE_URL'])
    expect(labels('variables:\n  - ')).toEqual(['IMAGE_TAG'])
    expect(complete('steps:\n  - name: deploy\n    ', false)).toBeNull()
  })
})
