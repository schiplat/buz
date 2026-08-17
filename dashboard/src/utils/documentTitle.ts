const APP_NAME = 'Buz'

/**
 * Build the browser tab title as `{page} · Buz`.
 *
 * Falls back to the bare app name when `page` is empty, so auth pages
 * (`/login`, `/setup`) and redirects still render a sane title.
 */
export function pageTitle(page?: string): string {
  const trimmed = page?.trim()
  return trimmed ? `${trimmed} · ${APP_NAME}` : APP_NAME
}
