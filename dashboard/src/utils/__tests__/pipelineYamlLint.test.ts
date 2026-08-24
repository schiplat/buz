import { describe, expect, it } from 'vitest'
import {
  diagnosticsFromValidation,
  extractUnknownFieldKey,
  parseLineColumn,
  positionForDiagnostic,
  positionForField,
  splitWarningField,
} from '../pipelineYamlLint'

const sample = `name: demo
steps:
  - name: one
    run: echo 1
  - name: two
    uses: docker
    image: alpine
    ru: echo typo
`

describe('positionForField', () => {
  it('finds top-level key token only', () => {
    const pos = positionForField(sample, 'name')
    expect(sample.slice(pos.from, pos.to)).toBe('name')
  })

  it('finds step field by index', () => {
    const pos = positionForField(sample, 'steps[1].uses')
    expect(sample.slice(pos.from, pos.to)).toBe('uses')
  })

  it('highlights typo key from field path', () => {
    const pos = positionForField(sample, 'steps[1].ru')
    expect(sample.slice(pos.from, pos.to)).toBe('ru')
  })

  it('falls back for yaml syntax field', () => {
    const pos = positionForField(sample, 'yaml')
    expect(pos.from).toBe(0)
  })
})

describe('positionForDiagnostic', () => {
  it('uses unknown field in message when field is yaml.keys', () => {
    const pos = positionForDiagnostic(
      sample,
      'yaml.keys',
      'YAML error: steps[1]: unknown field `ru`, expected one of `name` at line 8 column 5',
    )
    expect(sample.slice(pos.from, pos.to)).toBe('ru')
  })

  it('parses helpers', () => {
    expect(extractUnknownFieldKey('unknown field `ru`, expected')).toBe('ru')
    expect(parseLineColumn('at line 8 column 5')).toEqual({ line: 8, column: 5 })
  })
})

describe('splitWarningField', () => {
  it('parses field prefix from field-scoped warnings', () => {
    const { field, message } = splitWarningField(
      "on.manual.inputs.tag: no 'type' declared — defaults to 'string'",
    )
    expect(field).toBe('on.manual.inputs.tag')
    expect(message).toContain("no 'type' declared")
  })

  it('keeps global warnings without a field', () => {
    const { field, message } = splitWarningField(
      'Multiple trigger types defined — only the first will be used',
    )
    expect(field).toBeNull()
    expect(message).toContain('Multiple trigger types')
  })
})

describe('diagnosticsFromValidation', () => {
  const inputYaml = `name: demo
on:
  manual:
    inputs:
      tag:
        description: ACR tag
      replicas:
        type: number
steps:
  - name: one
    run: echo 1
`

  it('positions field-scoped warnings on the input key line', () => {
    const result = {
      valid: true,
      name: 'demo',
      errors: [],
      warnings: [
        "on.manual.inputs.tag: no 'type' declared — defaults to 'string'; declare 'type:' to enable number/boolean/host/choice validation and matching UI control",
      ],
    }
    const diags = diagnosticsFromValidation(inputYaml, result)
    expect(diags).toHaveLength(1)
    expect(diags[0].severity).toBe('warning')
    const hit = inputYaml.slice(diags[0].from, diags[0].to)
    expect(hit).toBe('tag')
  })

  it('keeps global warnings at the top', () => {
    const result = {
      valid: true,
      name: 'demo',
      errors: [],
      warnings: ['Multiple trigger types defined — only the first will be used'],
    }
    const diags = diagnosticsFromValidation(inputYaml, result)
    expect(diags).toHaveLength(1)
    expect(diags[0].from).toBe(0)
    expect(diags[0].message).toContain('Multiple trigger types')
  })
})
