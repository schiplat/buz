import { describe, expect, it } from 'vitest'
import {
  extractUnknownFieldKey,
  parseLineColumn,
  positionForDiagnostic,
  positionForField,
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
