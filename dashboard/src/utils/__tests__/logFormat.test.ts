import { describe, expect, it } from 'vitest'
import { colorizeLogLine, formatLogChunkForTerminal } from '@/utils/logFormat'

describe('formatLogChunkForTerminal', () => {
  it('prefixes every non-empty line with the same clock', () => {
    const out = formatLogChunkForTerminal(
      '11:56:55',
      '==> RUN [docker]\n  cmd  echo hi\n',
    )
    const lines = out.replace(/\r\n$/, '').split('\r\n')
    expect(lines).toHaveLength(2)
    expect(lines[0]).toContain('[11:56:55]')
    expect(lines[0]).toContain('==> RUN [docker]')
    expect(lines[1]).toContain('[11:56:55]')
    expect(lines[1]).toContain('cmd')
  })

  it('handles a single line without trailing newline', () => {
    const out = formatLogChunkForTerminal('01:02:03', 'hello')
    expect(out).toBe('\x1b[2m[01:02:03]\x1b[0m hello\r\n')
  })
})

describe('colorizeLogLine', () => {
  it('colors ERROR banners red', () => {
    expect(colorizeLogLine('==> ERROR')).toContain('\x1b[31m')
  })

  it('colors RUN banners green', () => {
    expect(colorizeLogLine('==> RUN [docker]')).toContain('\x1b[32m')
  })

  it('leaves plain output uncolored', () => {
    expect(colorizeLogLine('Unable to find image')).toBe('Unable to find image')
  })
})
