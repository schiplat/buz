import { describe, it, expect } from 'vitest'
import { pageTitle } from '../documentTitle'

describe('pageTitle', () => {
  it('appends the app name to a page title', () => {
    expect(pageTitle('Runners')).toBe('Runners · Buz')
  })

  it('trims surrounding whitespace', () => {
    expect(pageTitle('  Pipelines  ')).toBe('Pipelines · Buz')
  })

  it('falls back to the bare app name for empty input', () => {
    expect(pageTitle('')).toBe('Buz')
    expect(pageTitle(undefined)).toBe('Buz')
    expect(pageTitle('   ')).toBe('Buz')
  })
})
