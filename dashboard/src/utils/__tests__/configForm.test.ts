import { describe, expect, it } from 'vitest'
import {
  applyConfigMode,
  detectConfigMode,
  formatConfigRenderSummary,
  normalizeConfigRender,
  serializeConfigForApi,
  serializeConfigRender,
  validateStepConfig,
} from '../configForm'

describe('normalizeConfigRender', () => {
  it('accepts array, single object, and JSON string', () => {
    expect(
      normalizeConfigRender([{ template: 'a', output: '.env', mode: '0600', on_missing: 'fail' }]),
    ).toEqual([{ template: 'a', output: '.env', mode: '0600', on_missing: 'fail', backup_keep: 3 }])
    expect(normalizeConfigRender({ template: 't' })).toEqual([
      { template: 't', output: '.env', mode: '0600', on_missing: 'fail', backup_keep: 3 },
    ])
    expect(
      normalizeConfigRender(JSON.stringify({ template: 'x', output: 'out', mode: '0644', on_missing: 'warn' })),
    ).toEqual([{ template: 'x', output: 'out', mode: '0644', on_missing: 'warn', backup_keep: 3 }])
  })

  it('returns empty for null/blank', () => {
    expect(normalizeConfigRender(null)).toEqual([])
    expect(normalizeConfigRender('')).toEqual([])
    expect(normalizeConfigRender('not-json')).toEqual([])
  })
})

describe('serializeConfigRender', () => {
  it('drops blank templates', () => {
    expect(
      serializeConfigRender([
        { template: '', output: '.env', mode: '0600', on_missing: 'fail' },
        { template: ' env.example ', output: '', mode: '', on_missing: 'warn' },
      ]),
    ).toEqual([
      { template: 'env.example', output: '.env', mode: '0600', on_missing: 'warn', backup_keep: 3 },
    ])
  })
})

describe('detectConfigMode / applyConfigMode', () => {
  it('detects render and none', () => {
    expect(detectConfigMode({})).toBe('none')
    expect(detectConfigMode({ config_dest: '/opt' })).toBe('render')
    expect(
      detectConfigMode({
        config_dest: '/opt',
        config_render: [{ template: 'env.example', output: '.env', mode: '0600', on_missing: 'fail' }],
      }),
    ).toBe('render')
    expect(
      detectConfigMode(
        { config_dest: '/opt', config_render: [{ template: 't' }] },
        false,
      ),
    ).toBe('none')
  })

  it('applyConfigMode clears fields for none and seeds a row for render', () => {
    const step: Record<string, any> = {
      config_dest: '/opt',
      config_render: [{ template: 't', output: '.env', mode: '0600', on_missing: 'fail' }],
    }
    applyConfigMode(step, 'none', true)
    expect(detectConfigMode(step)).toBe('none')
    expect(step.config_dest).toBe('')
    expect(step.config_render).toEqual([])

    applyConfigMode(step, 'render', true)
    expect(step.config_render.length).toBe(1)
  })
})

describe('validateStepConfig', () => {
  it('accepts empty and valid render-only', () => {
    expect(validateStepConfig({ engine: 'shell' })).toBeNull()
    expect(
      validateStepConfig({
        engine: 'shell',
        config_dest: '/opt/manager',
        config_render: [{ template: 'env.example', output: '.env', mode: '0600', on_missing: 'fail' }],
      }),
    ).toBeNull()
    expect(
      validateStepConfig({
        engine: 'ssh',
        config_dest: '/opt/manager',
        config_render: [{ template: 'env.example', output: '.env', mode: '0600', on_missing: 'fail' }],
      }),
    ).toBeNull()
  })

  it('recommends fetch plus render for a dest-only block', () => {
    const error = validateStepConfig({ engine: 'shell', config_dest: 'config' })
    expect(error).toContain('buz/config-fetch')
    expect(error).toContain('config.render')
    expect(error).not.toContain('source+key_path')
  })

  it('rejects dest alone and any config on unsupported engines', () => {
    expect(validateStepConfig({ engine: 'shell', config_dest: '/opt' })?.includes('alone')).toBe(true)
    expect(
      validateStepConfig({
        engine: 'docker',
        config_dest: '/opt',
        config_render: [{ template: 't', output: '.env', mode: '0600', on_missing: 'fail' }],
      })?.includes('only valid'),
    ).toBe(true)
    expect(validateStepConfig({ engine: 'docker', config_dest: '/opt' })?.includes('only valid')).toBe(true)
  })
})

describe('serializeConfigForApi', () => {
  it('omits empty fields', () => {
    expect(serializeConfigForApi({})).toEqual({
      config_dest: undefined,
      config_render: undefined,
    })
    expect(
      serializeConfigForApi({
        config_dest: ' /opt ',
        config_render: [{ template: 't', output: '.env', mode: '0600', on_missing: 'fail' }],
      }),
    ).toEqual({
      config_dest: '/opt',
      config_render: [{ template: 't', output: '.env', mode: '0600', on_missing: 'fail', backup_keep: 3 }],
    })
  })
})

describe('formatConfigRenderSummary', () => {
  it('joins template → output pairs', () => {
    expect(
      formatConfigRenderSummary([
        { template: 'a', output: '.env', mode: '0600', on_missing: 'fail' },
        { template: 'b', output: 'c.conf', mode: '0644', on_missing: 'warn' },
      ]),
    ).toBe('a → .env, b → c.conf')
  })
})
