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
  it('detects deliver, render, sugar, none', () => {
    expect(detectConfigMode({})).toBe('none')
    expect(
      detectConfigMode({
        config_source: 'OSS',
        config_key_path: 'p/v1',
        config_dest: '/opt',
      }),
    ).toBe('deliver')
    expect(
      detectConfigMode({
        config_dest: '/opt',
        config_render: [{ template: 'env.example', output: '.env', mode: '0600', on_missing: 'fail' }],
      }),
    ).toBe('render')
    expect(
      detectConfigMode({
        config_source: 'OSS',
        config_key_path: 'p/v1',
        config_dest: '/opt',
        config_render: [{ template: 'env.example', output: '.env', mode: '0600', on_missing: 'fail' }],
      }),
    ).toBe('sugar')
  })

  it('applyConfigMode clears fields for none / deliver / render', () => {
    const step: Record<string, any> = {
      config_source: 'OSS',
      config_key_path: 'p',
      config_dest: '/opt',
      config_render: [{ template: 't', output: '.env', mode: '0600', on_missing: 'fail' }],
    }
    applyConfigMode(step, 'none', true)
    expect(detectConfigMode(step)).toBe('none')

    step.config_source = 'OSS'
    step.config_key_path = 'p'
    step.config_dest = '/opt'
    applyConfigMode(step, 'deliver', true)
    expect(step.config_render).toEqual([])
    expect(detectConfigMode(step)).toBe('deliver')

    applyConfigMode(step, 'render', true)
    expect(step.config_source).toBe('')
    expect(step.config_key_path).toBe('')
    expect(step.config_render.length).toBe(1)
  })
})

describe('validateStepConfig', () => {
  it('accepts empty and valid deliver / render-only', () => {
    expect(validateStepConfig({ engine: 'shell' })).toBeNull()
    expect(
      validateStepConfig({
        engine: 'ssh',
        config_source: 'OSS',
        config_key_path: 'cfg/v1',
        config_dest: '/opt/manager',
      }),
    ).toBeNull()
    expect(
      validateStepConfig({
        engine: 'shell',
        config_dest: '/opt/manager',
        config_render: [{ template: 'env.example', output: '.env', mode: '0600', on_missing: 'fail' }],
      }),
    ).toBeNull()
  })

  it('rejects dest alone, source without key, render on docker', () => {
    expect(validateStepConfig({ engine: 'shell', config_dest: '/opt' })?.includes('alone')).toBe(true)
    expect(
      validateStepConfig({ engine: 'shell', config_source: 'OSS', config_dest: '/opt' })?.includes(
        'together',
      ),
    ).toBe(true)
    expect(
      validateStepConfig({
        engine: 'docker',
        config_dest: '/opt',
        config_render: [{ template: 't', output: '.env', mode: '0600', on_missing: 'fail' }],
      })?.includes('render'),
    ).toBe(true)
  })
})

describe('serializeConfigForApi', () => {
  it('omits empty fields', () => {
    expect(serializeConfigForApi({})).toEqual({
      config_source: undefined,
      config_key_path: undefined,
      config_dest: undefined,
      config_render: undefined,
    })
    expect(
      serializeConfigForApi({
        config_source: ' OSS ',
        config_key_path: ' p ',
        config_dest: ' /opt ',
        config_render: [{ template: 't', output: '.env', mode: '0600', on_missing: 'fail' }],
      }),
    ).toEqual({
      config_source: 'OSS',
      config_key_path: 'p',
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
