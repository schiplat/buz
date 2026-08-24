import { describe, expect, it } from 'vitest'
import {
  emptyInputDef,
  findMissingRequiredInputs,
  normalizeInputDefsForForm,
  normalizeInputDefsForSave,
} from '../pipelineInputs'

describe('normalizeInputDefsForSave', () => {
  it('drops blank names and omits falsey optional flags', () => {
    const saved = normalizeInputDefsForSave([
      { name: '  ', description: 'x' },
      { name: ' version ', description: ' Tag ', required: false, masking: false, default: '' },
      { name: 'token', required: true, masking: true, default: 'abc' },
    ])
    expect(saved).toEqual([
      { name: 'version', description: 'Tag' },
      { name: 'token', required: true, masking: true, default: 'abc' },
    ])
  })

  it('carries declared constraints (type / pattern / options)', () => {
    const saved = normalizeInputDefsForSave([
      {
        name: 'tag',
        type: 'string',
        pattern: '^[A-Za-z0-9._-]+$',
      },
      {
        name: 'env',
        type: 'choice',
        options: ['staging', 'production'],
      },
    ])
    expect(saved).toEqual([
      { name: 'tag', type: 'string', pattern: '^[A-Za-z0-9._-]+$' },
      { name: 'env', type: 'choice', options: ['staging', 'production'] },
    ])
  })

  it('omits empty pattern and empty options', () => {
    const saved = normalizeInputDefsForSave([
      { name: 'a', pattern: '', options: [] },
      { name: 'b', pattern: 'x', options: [] },
    ])
    expect(saved).toEqual([{ name: 'a' }, { name: 'b', pattern: 'x' }])
  })

  it('returns undefined for empty list', () => {
    expect(normalizeInputDefsForSave([])).toBeUndefined()
    expect(normalizeInputDefsForSave(undefined)).toBeUndefined()
  })
})

describe('normalizeInputDefsForForm', () => {
  it('coerces masking/required to booleans', () => {
    expect(
      normalizeInputDefsForForm([
        { name: 'a', masking: true },
        { name: 'b' },
      ]),
    ).toEqual([
      { name: 'a', description: '', required: false, default: '', masking: true },
      { name: 'b', description: '', required: false, default: '', masking: false },
    ])
  })

  it('preserves declared constraints for the trigger form', () => {
    expect(
      normalizeInputDefsForForm([
        { name: 'env', type: 'choice', options: ['staging', 'production'] },
        { name: 'tag', pattern: '^[0-9]+$' },
      ]),
    ).toEqual([
      {
        name: 'env',
        description: '',
        required: false,
        default: '',
        masking: false,
        type: 'choice',
        options: ['staging', 'production'],
      },
      { name: 'tag', description: '', required: false, default: '', masking: false, pattern: '^[0-9]+$' },
    ])
  })
})

describe('emptyInputDef', () => {
  it('starts with masking off', () => {
    expect(emptyInputDef().masking).toBe(false)
  })
})

describe('findMissingRequiredInputs', () => {
  it('flags required inputs with blank values', () => {
    const missing = findMissingRequiredInputs([
      { key: 'tag', value: '', required: true },
      { key: 'host', value: '   ', required: true },
      { key: 'env', value: '', required: false },
      { key: 'ok', value: 'x', required: true },
    ])
    expect(missing).toEqual(['tag', 'host'])
  })

  it('ignores blank optional and filled required values', () => {
    expect(
      findMissingRequiredInputs([
        { key: 'a', value: '', required: false },
        { key: 'b', value: '0', required: true },
        { key: 'c', value: undefined, required: false },
      ]),
    ).toEqual([])
  })
})
