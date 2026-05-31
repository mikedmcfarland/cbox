# Clipboard over SSH

Inside a cbox session you're talking to a shell over SSH, often through
host tmux. Copy/paste between the in-container agent and your host
clipboard works via **OSC 52** — a small terminal escape sequence the
shell prints, your terminal emulator interprets, and your OS clipboard
receives. Nothing in cbox or the base image disables it, but the path
crosses three pieces of software (shell, tmux, terminal emulator) and
all three need to cooperate. This page is the checklist.

## How the path works

```
agent / shell                            host tmux               terminal emulator      OS clipboard
   │                                        │                            │                    │
   │  printf '\033]52;c;<b64>\007'  ─────►  │  passthrough  ──────────►  │  write to clip ─►  │
```

- **Container → host clipboard** is OSC 52. The shell (or Claude Code,
  or any program) prints the escape; tmux either eats it, mangles it,
  or passes it through; the terminal emulator decodes the base64 and
  writes to the OS clipboard.
- **Host → container paste** is generally automatic — your terminal
  sends the text as input, the kernel pty delivers it, the program in
  the container reads it. Bracketed paste is the usual gotcha
  (multi-line pastes getting auto-indented or executed line-by-line),
  but that's a shell-config problem, not a cbox-over-SSH problem.

## Setup checklist

### Host tmux

OSC 52 passthrough is **off** in tmux by default. Add this to your
`~/.tmux.conf` and reload:

```tmux
set -g allow-passthrough on
set -g set-clipboard on
```

- `allow-passthrough on` lets the inner program's escape sequences
  reach the outer terminal at all. Without it, the OSC 52 bytes are
  dropped by tmux.
- `set-clipboard on` also makes tmux itself use OSC 52 when you copy
  with `prefix + ]`-style bindings — useful for copying from tmux's
  own copy mode.

Reload with `tmux source-file ~/.tmux.conf` or kill the server.

### iTerm2

Settings → General → Selection → **Applications in terminal may access
clipboard**. This is off by default. With it off, iTerm2 silently
ignores OSC 52.

### Ghostty

Works out of the box. OSC 52 read and write are enabled by default
(`clipboard-read = ask`, `clipboard-write = allow` in the default
config). If you've customised these, make sure write is `allow` (or
`ask`).

### Kitty

Works out of the box for writes. The default `clipboard_control` value
includes `write-clipboard write-primary`. If you've overridden it,
keep at least `write-clipboard` in the list.

### Alacritty

Works out of the box. No relevant config.

### WezTerm

Works out of the box. `enable_kitty_keyboard` and OSC 52 are on by
default.

### Terminal.app (macOS)

Does **not** support OSC 52. Use a different terminal emulator if you
want clipboard-over-SSH to work — none of the workarounds (`pbcopy`
over SSH, etc.) are worth the setup cost for cbox usage.

## Test recipe

From inside a cbox session (`cbox <name>`), run:

```sh
printf '\033]52;c;%s\a' "$(printf 'cbox clipboard works\n' | base64)"
```

Then paste on the host (Cmd+V on macOS). If you get
`cbox clipboard works`, the whole path is working. If the paste shows
your previous clipboard contents, one of the three layers above is
silently dropping the escape — re-check the host tmux and terminal
emulator settings in that order (tmux is the most common culprit).

If you don't want to type the test by hand, the same `printf` works
from any program. A quick way to copy a one-liner Claude has just
printed: select it in copy mode and paste it back — at which point the
shell prompt history holds it and you can re-emit it.

## Known limitations

- **Size cap.** Most terminal emulators cap OSC 52 payloads at ~74kB
  to ~100kB. Larger copies are truncated or dropped silently. For
  bigger payloads, use `scp`/`rsync` from the host.
- **Binary content.** OSC 52 is text-only (base64 of bytes, but the
  terminal usually treats the result as text). Don't expect to copy
  arbitrary binary data through it.
- **Nested tmux.** If you run tmux *inside* the container in addition
  to host tmux, both layers need `allow-passthrough on` and the inner
  layer needs `set-clipboard on`. Interactive cbox sessions don't use
  container tmux (they use `dtach` — see [ADR 005][adr5]), so this is
  usually only an issue if you've started tmux yourself inside the
  session, or you're attaching to an autonomous session
  (`ssh <tier> tmux -S /run/cbox/<name>.sock attach`).
- **Mosh.** Mosh does not support OSC 52 today. cbox uses plain SSH,
  but if you've layered mosh on top, OSC 52 will not pass.
- **Read-back (paste-from-host inside the container).** OSC 52 read
  (`\033]52;c;?\a`) requires the terminal to allow it; most default
  to deny or prompt. cbox doesn't rely on this — host→container paste
  uses the normal input stream, not OSC 52 reads.

## Why cbox doesn't ship a fix

The shell, tmux, and terminal emulator in this path all belong to the
host or to the user's own dotfiles. The base image already passes OSC
52 through cleanly (the in-container shell prints, sshd forwards
bytes, host tmux is the next hop). No `cbox-init` snippet would
improve the situation — the missing settings are all *outside* the
container.

If your terminal emulator is in the table above and you've enabled the
host tmux options, OSC 52 will work. If it doesn't, file an issue with
the test-recipe output and which row of the checklist you're on.

[adr5]: adr/005-dtach-interactive-tmux-autonomous.md
