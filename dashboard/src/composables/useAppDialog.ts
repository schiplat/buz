import { ref } from 'vue'

export type AlertOptions = {
  title?: string
  confirmLabel?: string
}

export type ConfirmOptions = {
  title?: string
  confirmLabel?: string
  cancelLabel?: string
  /** Destructive confirm button styling */
  danger?: boolean
}

export type PromptOptions = {
  title?: string
  label?: string
  placeholder?: string
  defaultValue?: string
  /** When true, empty submit is ignored (OK stays disabled). */
  required?: boolean
  multiline?: boolean
  confirmLabel?: string
  cancelLabel?: string
}

type AlertState = {
  kind: 'alert'
  title: string
  message: string
  confirmLabel: string
  resolve: () => void
}

type ConfirmState = {
  kind: 'confirm'
  title: string
  message: string
  confirmLabel: string
  cancelLabel: string
  danger: boolean
  resolve: (ok: boolean) => void
}

type PromptState = {
  kind: 'prompt'
  title: string
  message: string
  label: string
  placeholder: string
  required: boolean
  multiline: boolean
  confirmLabel: string
  cancelLabel: string
  value: string
  resolve: (value: string | null) => void
}

export type AppDialogState = AlertState | ConfirmState | PromptState | null

const dialog = ref<AppDialogState>(null)
const queue: Array<() => void> = []

function enqueue(run: () => void) {
  if (dialog.value) {
    queue.push(run)
  } else {
    run()
  }
}

function settle() {
  dialog.value = null
  const next = queue.shift()
  if (next) next()
}

/** In-app dialogs — replaces browser alert / confirm / prompt. */
export function useAppDialog() {
  function alert(message: string, opts: AlertOptions = {}): Promise<void> {
    return new Promise((resolve) => {
      enqueue(() => {
        dialog.value = {
          kind: 'alert',
          title: opts.title ?? 'Notice',
          message,
          confirmLabel: opts.confirmLabel ?? 'OK',
          resolve: () => {
            settle()
            resolve()
          },
        }
      })
    })
  }

  function confirm(message: string, opts: ConfirmOptions = {}): Promise<boolean> {
    return new Promise((resolve) => {
      enqueue(() => {
        dialog.value = {
          kind: 'confirm',
          title: opts.title ?? 'Confirm',
          message,
          confirmLabel: opts.confirmLabel ?? 'Confirm',
          cancelLabel: opts.cancelLabel ?? 'Cancel',
          danger: opts.danger ?? false,
          resolve: (ok) => {
            settle()
            resolve(ok)
          },
        }
      })
    })
  }

  function prompt(message: string, opts: PromptOptions = {}): Promise<string | null> {
    return new Promise((resolve) => {
      enqueue(() => {
        dialog.value = {
          kind: 'prompt',
          title: opts.title ?? 'Input',
          message,
          label: opts.label ?? 'Value',
          placeholder: opts.placeholder ?? '',
          required: opts.required ?? false,
          multiline: opts.multiline ?? false,
          confirmLabel: opts.confirmLabel ?? 'OK',
          cancelLabel: opts.cancelLabel ?? 'Cancel',
          value: opts.defaultValue ?? '',
          resolve: (value) => {
            settle()
            resolve(value)
          },
        }
      })
    })
  }

  return { alert, confirm, prompt, dialog }
}

/** Shared state for AppDialogHost (same module singleton). */
export function useAppDialogState() {
  return dialog
}
