import type { PipelineInputDef } from '@/api/pipelines'

/** Normalize form input defs for create/update API payloads. */
export function normalizeInputDefsForSave(
  defs: PipelineInputDef[] | null | undefined,
): PipelineInputDef[] | undefined {
  if (!defs?.length) return undefined
  const out: PipelineInputDef[] = []
  for (const d of defs) {
    const name = (d.name || '').trim()
    if (!name) continue
    const item: PipelineInputDef = { name }
    const description = (d.description || '').trim()
    if (description) item.description = description
    if (d.required) item.required = true
    const defVal = (d.default || '').trim()
    if (defVal) item.default = defVal
    if (d.masking) item.masking = true
    if (d.type) item.type = d.type
    if (d.pattern) item.pattern = d.pattern
    if (d.options?.length) item.options = [...d.options]
    out.push(item)
  }
  return out.length > 0 ? out : undefined
}

/** Normalize API/YAML defs into editable form rows. */
export function normalizeInputDefsForForm(
  defs: PipelineInputDef[] | null | undefined,
): PipelineInputDef[] {
  if (!defs?.length) return []
  return defs.map((d) => ({
    name: d.name || '',
    description: d.description || '',
    required: !!d.required,
    default: d.default || '',
    masking: !!d.masking,
    type: d.type,
    pattern: d.pattern,
    options: d.options,
  }))
}

export function emptyInputDef(): PipelineInputDef {
  return { name: '', description: '', required: false, default: '', masking: false }
}

/**
 * Keys of required inputs whose value is blank. Blank == not filled, so
 * required fields are flagged client-side before submission; blank optional
 * values are simply not sent (backend merges declared defaults).
 */
export function findMissingRequiredInputs(
  pairs: { key: string; value?: string; required?: boolean }[],
): string[] {
  return pairs
    .filter((p) => p.required && !(p.value ?? '').trim())
    .map((p) => p.key)
}
