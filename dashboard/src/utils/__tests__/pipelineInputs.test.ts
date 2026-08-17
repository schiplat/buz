import { describe, expect, it } from 'vitest'
import {
  emptyInputDef,
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
})

describe('emptyInputDef', () => {
  it('starts with masking off', () => {
    expect(emptyInputDef().masking).toBe(false)
  })
})
