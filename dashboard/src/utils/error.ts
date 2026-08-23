type ErrorMessages = Record<number, string>

const messages: ErrorMessages = {
  1001: 'Authentication failed',
  1002: 'Token expired',
  1003: 'Insufficient permissions',
  2001: 'Not found',
  2002: 'Resource conflict',
  3001: 'Validation failed',
  5000: 'Internal server error',
}

export function getErrorMessage(code: number): string {
  return messages[code] || 'Unknown error'
}

export type ApiErrorItem = {
  /** YAML / form field path when present (e.g. `notify[0].webhook`). */
  field?: string
  message: string
}

/** Structured error for dialogs — title + optional summary + bullet items. */
export type FormattedApiError = {
  title: string
  summary?: string
  items: ApiErrorItem[]
}

/**
 * Extract a human-readable message from an Axios / AppError response.
 * Prefers the server `message` field; strips enum prefixes like `ValidationFailed: `.
 */
export function formatApiError(err: unknown, fallback = 'Request failed'): string {
  const e = err as {
    response?: { data?: { message?: unknown; error?: unknown; code?: unknown } }
    message?: unknown
  }
  const body = e?.response?.data
  const raw =
    (typeof body?.message === 'string' && body.message.trim())
    || (typeof body?.error === 'string' && body.error.trim())
    || (typeof e?.message === 'string' && e.message.trim())
    || ''
  if (!raw) {
    const code = typeof body?.code === 'number' ? body.code : NaN
    if (!Number.isNaN(code)) return getErrorMessage(code)
    return fallback
  }
  return stripErrorPrefix(raw) || fallback
}

function stripErrorPrefix(raw: string): string {
  return raw
    .replace(
      /^(ValidationFailed|NotFound|Conflict|Unauthorized|AuthFailed|Internal|DatabaseError):\s*/i,
      '',
    )
    .trim()
}

/** Split `field: message; field2: message2` into structured items. */
export function splitFieldMessages(blob: string): ApiErrorItem[] {
  return blob
    .split(/\s*;\s*/)
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => {
      // Prefer `path.with[0].dots: rest` — stop at first ": " after a path-like token.
      const m = part.match(/^([A-Za-z_][\w.\[\]]*):\s*(.+)$/s)
      if (m) {
        return { field: m[1], message: m[2].trim() }
      }
      return { message: part }
    })
}

/**
 * Parse API errors into a title / summary / item list for clear UI display.
 * Understands Manager trigger YAML validation messages.
 */
export function parseApiError(err: unknown, fallback = 'Request failed'): FormattedApiError {
  const raw = formatApiError(err, fallback)

  // YAML file 'foo.yml' failed validation — fix the file before triggering: a: b; c: d
  const yamlFail = raw.match(
    /^YAML file '([^']+)' failed validation\s*[—\-–:]\s*(?:fix the file before triggering:\s*)?(.+)$/is,
  )
  if (yamlFail) {
    const filename = yamlFail[1]
    const rest = yamlFail[2].trim()
    // Drop leading "fix the file…" if the dash branch left it in.
    const detail = rest.replace(/^fix the file before triggering:\s*/i, '').trim()
    const items = splitFieldMessages(detail)
    return {
      title: 'YAML validation failed',
      summary: filename,
      items: items.length ? items : [{ message: detail || raw }],
    }
  }

  // YAML syntax error in 'foo.yml': ...
  const yamlSyntax = raw.match(/^YAML syntax error in '([^']+)':\s*(.+)$/is)
  if (yamlSyntax) {
    return {
      title: 'YAML syntax error',
      summary: yamlSyntax[1],
      items: [{ message: yamlSyntax[2].trim() }],
    }
  }

  // invalid trigger inputs — tag: value does not match…; env: must be one of…
  const triggerFail = raw.match(/^invalid trigger inputs\s*[—\-–:]\s*(.+)$/is)
  if (triggerFail) {
    const items = splitFieldMessages(triggerFail[1].trim())
    return {
      title: 'Invalid inputs',
      summary: 'Fix the highlighted fields and trigger again.',
      items: items.length ? items : [{ message: triggerFail[1].trim() }],
    }
  }

  if (raw.includes(';')) {
    const items = splitFieldMessages(raw)
    if (items.length > 1 || items.some((i) => i.field)) {
      return { title: fallback, items }
    }
  }

  return {
    title: fallback,
    items: [{ message: raw }],
  }
}
