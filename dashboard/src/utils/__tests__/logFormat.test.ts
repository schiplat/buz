import { describe, expect, it } from 'vitest'
import { colorizeLogLine, formatLogChunkForTerminal } from '@/utils/logFormat'

describe('formatLogChunkForTerminal', () => {
  it('prefixes every non-empty line with the same clock and strips banner marker', () => {
    const out = formatLogChunkForTerminal(
      '11:56:55',
      '==> RUN [docker]\n  cmd  echo hi\n',
    )
    const lines = out.replace(/\r\n$/, '').split('\r\n')
    expect(lines).toHaveLength(2)
    expect(lines[0]).toContain('[11:56:55]')
    expect(lines[0]).toContain('RUN [docker]')
    expect(lines[0]).not.toContain('==>')
    expect(lines[1]).toContain('[11:56:55]')
    expect(lines[1]).toContain('cmd')
  })

  it('handles a single line without trailing newline', () => {
    const out = formatLogChunkForTerminal('01:02:03', 'hello')
    expect(out).toBe('\x1b[2m[01:02:03]\x1b[0m hello\r\n')
  })
})

describe('colorizeLogLine — banner classification', () => {
  it('strips the `==>` marker from banner lines', () => {
    expect(colorizeLogLine('==> RUNNER')).not.toContain('==>')
    expect(colorizeLogLine('==> RUN [docker]')).not.toContain('==>')
    expect(colorizeLogLine('==> ERROR')).not.toContain('==>')
  })

  it('colors ERROR banners bold red', () => {
    expect(colorizeLogLine('==> ERROR')).toBe('\x1b[1;31mERROR\x1b[0m')
  })

  it('colors RUN banners bold green', () => {
    expect(colorizeLogLine('==> RUN [docker]')).toBe('\x1b[1;32mRUN [docker]\x1b[0m')
  })

  it('colors RUNNER bold cyan, not the RUN green', () => {
    const line = colorizeLogLine('==> RUNNER')
    expect(line).toContain('\x1b[1;36m')
    expect(line).not.toContain('\x1b[1;32m')
  })

  it('colors LOGIN cyan', () => {
    expect(colorizeLogLine('==> LOGIN [registry.example.com]')).toContain('\x1b[36m')
  })

  it('colors SSH magenta', () => {
    expect(colorizeLogLine('==> SSH [deploy@10.0.0.1:22]')).toContain('\x1b[35m')
  })

  it('colors CONFIG / CONFIG PREP / CONFIG READY blue', () => {
    expect(colorizeLogLine('==> STEP CONFIG')).toContain('\x1b[34m')
    expect(colorizeLogLine('==> CONFIG PREP')).toContain('\x1b[34m')
    expect(colorizeLogLine('==> CONFIG READY')).toContain('\x1b[34m')
  })

  it('dims STAGE markers', () => {
    expect(colorizeLogLine('==> STAGE exec')).toContain('\x1b[2m')
  })

  it('colors STEP RESULT bold white', () => {
    expect(colorizeLogLine('==> STEP RESULT')).toContain('\x1b[1;37m')
  })

  it('colors generic `==> <step_name>` banner bold blue', () => {
    expect(colorizeLogLine('==> deploy')).toBe('\x1b[1;34mdeploy\x1b[0m')
  })
})

describe('colorizeLogLine — field lines', () => {
  it('dims the key and keeps the value plain for aligned fields', () => {
    const out = colorizeLogLine('  exec_type        shell')
    expect(out).toBe('\x1b[2m  exec_type\x1b[0m        shell')
  })

  it('dims short keys like note/cwd/env/cmd', () => {
    expect(colorizeLogLine('  note  GIT_TERMINAL_PROMPT=0')).toBe(
      '\x1b[2m  note\x1b[0m  GIT_TERMINAL_PROMPT=0',
    )
    expect(colorizeLogLine('  cmd   echo hi')).toBe('\x1b[2m  cmd\x1b[0m   echo hi')
  })

  it('matches config_dest over config (longest-first)', () => {
    const out = colorizeLogLine('  config_dest      /opt/manager')
    expect(out).toBe('\x1b[2m  config_dest\x1b[0m      /opt/manager')
  })

  it('matches hosts_strategy over host/hosts', () => {
    const out = colorizeLogLine('  hosts_strategy   chain')
    expect(out).toBe('\x1b[2m  hosts_strategy\x1b[0m   chain')
  })

  it('handles the two-word `exit code` key', () => {
    const out = colorizeLogLine('  exit code  -1')
    expect(out).toBe('\x1b[2m  exit code\x1b[0m  -1')
  })

  it('dims indented continuation lines without a field key', () => {
    const out = colorizeLogLine('        more…')
    expect(out).toBe('\x1b[2m        more…\x1b[0m')
  })

  it('dims multi-host separator lines', () => {
    const out = colorizeLogLine('--- host-a ---')
    expect(out).toBe('\x1b[2m--- host-a ---\x1b[0m')
  })
})

describe('colorizeLogLine — process output', () => {
  it('leaves plain output uncolored', () => {
    expect(colorizeLogLine('Unable to find image')).toBe('Unable to find image')
  })

  it('requires >=2 spaces after the key to split a field line', () => {
    // Single space after `cmd` → not a field line; falls back to dim whole line.
    expect(colorizeLogLine('  cmd echo')).toBe('\x1b[2m  cmd echo\x1b[0m')
    // No recognized key → dim whole indented line.
    expect(colorizeLogLine('  hello world')).toBe('\x1b[2m  hello world\x1b[0m')
  })
})
