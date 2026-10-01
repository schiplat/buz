import { describe, expect, it } from 'vitest'
import {
  formatApiError,
  getErrorMessage,
  parseApiError,
  splitFieldMessages,
} from '@/utils/error'

describe('formatApiError', () => {
  it('prefers response.message and strips ValidationFailed prefix', () => {
    const err = {
      response: {
        data: {
          code: 3001,
          message:
            "ValidationFailed: YAML file 'x.yml' failed validation — fix the file before triggering: secrets: secret 'FEISHU_WEBHOOK' not found",
        },
      },
      message: 'Request failed with status code 422',
    }
    expect(formatApiError(err)).toContain("secret 'FEISHU_WEBHOOK' not found")
    expect(formatApiError(err)).not.toMatch(/^ValidationFailed/i)
  })

  it('falls back to axios message', () => {
    expect(formatApiError({ message: 'Network Error' })).toBe('Network Error')
  })

  it('falls back to code map then default', () => {
    expect(formatApiError({ response: { data: { code: 3001 } } })).toBe(
      getErrorMessage(3001),
    )
    expect(formatApiError({})).toBe('Request failed')
  })
})

describe('splitFieldMessages', () => {
  it('splits field: message pairs', () => {
    expect(
      splitFieldMessages(
        "secrets: secret 'FEISHU_WEBHOOK' not found; notify[0].webhook: must be a secret",
      ),
    ).toEqual([
      { field: 'secrets', message: "secret 'FEISHU_WEBHOOK' not found" },
      { field: 'notify[0].webhook', message: 'must be a secret' },
    ])
  })
})

describe('parseApiError', () => {
  it('structures YAML validation failures into title + bullets', () => {
    const err = {
      response: {
        data: {
          message:
            "ValidationFailed: YAML file 'demo-app-deploy-host-b.yml' failed validation — fix the file before triggering: secrets: secret 'FEISHU_WEBHOOK' not found; variables: unknown variable 'X'",
        },
      },
    }
    const parsed = parseApiError(err, 'Trigger failed')
    expect(parsed.title).toBe('YAML validation failed')
    expect(parsed.summary).toBe('demo-app-deploy-host-b.yml')
    expect(parsed.items).toEqual([
      { field: 'secrets', message: "secret 'FEISHU_WEBHOOK' not found" },
      { field: 'variables', message: "unknown variable 'X'" },
    ])
  })

  it('structures declarative input validation failures into field bullets', () => {
    const err = {
      response: {
        data: {
          message:
            "ValidationFailed: invalid trigger inputs — tag: value does not match pattern '^[A-Za-z0-9._-]+$'; deploy_host: host 'ghost' is not registered",
        },
      },
    }
    const parsed = parseApiError(err, 'Trigger failed')
    expect(parsed.title).toBe('Invalid inputs')
    expect(parsed.items).toEqual([
      { field: 'tag', message: "value does not match pattern '^[A-Za-z0-9._-]+$'" },
      { field: 'deploy_host', message: "host 'ghost' is not registered" },
    ])
  })

  it('keeps plain messages as a single item', () => {
    const parsed = parseApiError({ message: 'Network Error' }, 'Trigger failed')
    expect(parsed.title).toBe('Trigger failed')
    expect(parsed.items).toEqual([{ message: 'Network Error' }])
  })
})
