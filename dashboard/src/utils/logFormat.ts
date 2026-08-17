//! Job terminal log formatting (xterm).
//!
//! Runner chunks are often multi-line (RUN echo, ERROR). The UI previously
//! prefixed only the first line with `[HH:MM:SS]`, which looked inconsistent
//! next to single-line stream chunks. This module prefixes every line and
//! lightly colors banner lines (`==> …`).

/** Dim clock + optional ANSI for banner / body lines. */
export function formatLogChunkForTerminal(ts: string, content: string): string {
  const dim = '\x1b[2m'
  const reset = '\x1b[0m'
  const normalized = content.replace(/\r\n/g, '\n').replace(/\r/g, '\n')
  const lines = normalized.split('\n')
  if (lines.length > 0 && lines[lines.length - 1] === '') {
    lines.pop()
  }
  if (lines.length === 0) {
    return `${dim}[${ts}]${reset}\r\n`
  }
  return (
    lines.map((line) => `${dim}[${ts}]${reset} ${colorizeLogLine(line)}`).join('\r\n') +
    '\r\n'
  )
}

/** Color `==> BANNER` lines; leave process output plain. */
export function colorizeLogLine(line: string): string {
  const trimmed = line.trimStart()
  if (!trimmed.startsWith('==>')) {
    // Indent continuation / key lines slightly dim for structure
    if (/^\s+(note|cwd|env|cmd|command|hint)\b/i.test(line)) {
      return `\x1b[2m${line}\x1b[0m`
    }
    return line
  }
  const upper = trimmed.toUpperCase()
  if (upper.includes('ERROR') || upper.includes('FAILED')) {
    return `\x1b[31m${line}\x1b[0m`
  }
  if (upper.includes('CANCELLED') || upper.includes('TIMEOUT')) {
    return `\x1b[33m${line}\x1b[0m`
  }
  if (upper.includes('LOGIN')) {
    return `\x1b[36m${line}\x1b[0m`
  }
  if (upper.includes('RUN')) {
    return `\x1b[32m${line}\x1b[0m`
  }
  // Generic banner (SSH step name, etc.)
  return `\x1b[34m${line}\x1b[0m`
}
