import { type HTMLAttributes } from 'vue'
import { cn } from '@/lib/utils'

export type ButtonVariant =
  | 'default'
  | 'secondary'
  | 'outline'
  | 'ghost'
  | 'destructive'
  | 'destructive-soft'
  | 'success'
export type ButtonSize = 'default' | 'sm' | 'icon'

const variants: Record<ButtonVariant, string> = {
  default: 'bg-primary text-primary-foreground hover:bg-primary/90 active:bg-primary/85',
  secondary: 'bg-secondary text-secondary-foreground hover:bg-secondary/80',
  outline: 'bg-muted text-foreground hover:bg-accent hover:text-accent-foreground',
  ghost: 'hover:bg-muted hover:text-foreground',
  destructive: 'bg-destructive text-destructive-foreground hover:bg-destructive/90',
  // Positive/negative toggle pair: light fills with dark matching text, so
  // the two states read as one scale. --success / --destructive are the
  // *text* tones for tinted chips, so solid buttons use lighter fills.
  success: 'bg-emerald-500 text-emerald-950 hover:bg-emerald-400 active:bg-emerald-500/90',
  'destructive-soft': 'bg-red-400 text-red-950 hover:bg-red-300 active:bg-red-400/90',
}

const sizes: Record<ButtonSize, string> = {
  default: 'h-9 px-3.5 text-sm',
  sm: 'h-8 px-2.5 text-xs',
  icon: 'h-8 w-8',
}

export function buttonClass(
  variant: ButtonVariant = 'default',
  size: ButtonSize = 'default',
  className?: HTMLAttributes['class'],
) {
  return cn(
    'inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-xl font-medium transition-colors',
    'focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring/30',
    'disabled:pointer-events-none disabled:opacity-50',
    variants[variant],
    sizes[size],
    className,
  )
}
