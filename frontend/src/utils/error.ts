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
