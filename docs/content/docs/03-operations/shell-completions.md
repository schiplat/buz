---
title: Shell Completions
weight: 7
description: "Tab completion for buz-manager, buz-runner, and buz-plugin"
---

Every Buz CLI prints a completion script for the shell you name:

```bash
buz-manager completions <shell>   # bash | elvish | fish | powershell | zsh
buz-runner  completions <shell>
buz-plugin  completions <shell>
```

The script is generated from the same command definition the binary parses, so
it always matches the installed version — regenerate it after an upgrade.
Output goes to stdout and never reads `manager.toml` / `runner.toml` or
contacts a Manager, so it is safe to run anywhere the binary is installed.

## bash

```bash
# current session only
source <(buz-manager completions bash)

# persist for the current user (bash-completion 2.x)
mkdir -p ~/.local/share/bash-completion/completions
buz-manager completions bash > ~/.local/share/bash-completion/completions/buz-manager
```

Repeat with `buz-runner` / `buz-plugin` for the other binaries. On
package-managed systems the system-wide location is `/etc/bash_completion.d/`.

## zsh

```bash
mkdir -p ~/.zfunc
buz-manager completions zsh > ~/.zfunc/_buz-manager
buz-runner  completions zsh > ~/.zfunc/_buz-runner
buz-plugin  completions zsh > ~/.zfunc/_buz-plugin
```

Add `~/.zfunc` to `fpath` **before** `compinit` runs, in `~/.zshrc`:

```zsh
fpath=(~/.zfunc $fpath)
autoload -Uz compinit && compinit
```

## fish

```bash
mkdir -p ~/.config/fish/completions
buz-manager completions fish > ~/.config/fish/completions/buz-manager.fish
buz-runner  completions fish > ~/.config/fish/completions/buz-runner.fish
buz-plugin  completions fish > ~/.config/fish/completions/buz-plugin.fish
```

## PowerShell

```powershell
buz-manager completions powershell | Out-String | Invoke-Expression
```

Add the same line to `$PROFILE` to keep it across sessions.

## elvish

```elvish
eval (buz-manager completions elvish | slurp)
```

Add the same line to `~/.config/elvish/rc.elv` to keep it across sessions.

## Verifying

```bash
buz-manager completions bash | grep -c gen-runner-token   # non-zero
buz-manager completions tcsh                              # exits 2: unsupported shell
```

The `completions` subcommand itself appears in the generated script, so
`buz-manager <TAB>` lists it alongside every other subcommand.
