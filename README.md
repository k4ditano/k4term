# k4term

A terminal emulator built on Ghostty's VT core and Zed's GPUI, and the house
terminal of the [k4 bar](https://github.com/k4ditano/k4) — though it does not
need it: without a bar in front, the bridge stays quiet and you get a plain,
fast terminal.

## Credit where it is due

This is **not** a from-scratch stack. The embedding work underneath — the Zig
build and C ABI over Ghostty's terminal core, the safe Rust wrapper, and the
GPUI `TerminalView` with its input, selection and rendering glue — comes from
[Xuanwo's `gpui-ghostty`](https://github.com/Xuanwo/gpui-ghostty), Apache-2.0,
whose copyright notice this repository keeps in `LICENSE`. k4term is what got
built on top: the two applications, the k4 bridge, and the additions to the
embedding layer that they needed.

The stack it inherits is minimal, pinned and testable:

- VT parsing/state: Ghostty's terminal core (vendored as a submodule)
- Rendering/UI: GPUI (from Zed), with a custom renderer (no Ghostty renderer reuse)

## The two binaries

On top of that stack this repo ships the terminal itself, in two binaries that
are the same session seen two ways:

- `apps/k4term`: the window. GPUI, the house font, live theming from the k4 bar.
- `apps/k4term-isla`: the same session minus the window. It serves the grid as
  JSON lines on stdout and takes keys on stdin, which is how the bar embeds a
  real terminal inside its island. The bar starts it on its own, so it has to
  be on `PATH`.

Install both, plus the icon and the desktop entry, into your home — no sudo,
nothing that collides with distro packages:

```sh
./instalar              # build in release and copy the binaries
./instalar --enlace     # symlink to target/release instead (for development)
./instalar --sin-shell  # skip the shell integration
./instalar --quitar     # undo it
```

Shell integration goes in automatically, into every shell it has a script for
whose rc file exists — not just `$SHELL`, because having your login shell in
one and your terminal in another is normal, and doing only one leaves it half
done. It is one marked block, it backs up what it replaces to `*.k4.bak`, it is
idempotent, and `--quitar` takes it out and leaves the file byte-identical.

Without it k4term still works, but the bar cannot know which command is running
or how long it has been going: that is the shell talking OSC.

The icon is drawn, not shipped as a blob: `python3 assets/icono.py` renders
every size from `assets/icono.py` itself. `assets/referencia.png` is the
generated design it was traced from.

With the k4 bar installed you also get `SUPER + Shift + T` (the session in the
island) and `SUPER + Alt + T` (pop it out into a window, in whatever directory
it was left).

## Workspace Layout

- `crates/ghostty_vt_sys`: Zig build + C ABI for the Ghostty VT core
- `crates/ghostty_vt`: safe Rust wrapper over the C ABI
- `crates/gpui_ghostty_terminal`: GPUI `TerminalView` + input/selection/rendering glue
- `examples/vt_dump`: feed bytes into VT and print the viewport
- `examples/basic_terminal`: minimal GPUI view that renders a `TerminalSession`
- `examples/pty_terminal`: login shell PTY wired to `TerminalView`
- `examples/split_pty_terminal`: two PTYs in split panes

## Version Pinning

- Ghostty is vendored at `vendor/ghostty` and pinned to tag `v1.2.3`.
- Zig is pinned to `0.14.1` (required to build the vendored Ghostty core).
- GPUI is consumed from Zed pinned to commit `6016d0b8c6a22e586158d3b6f810b3cebb136118`.

## Build Prerequisites

1. Initialize submodules:

```sh
git submodule update --init --recursive
```

2. Install Zig (pinned) into `.context/zig/zig`:

```sh
./scripts/bootstrap-zig.sh
```

3. Build and test:

```sh
cargo test
```

Notes:

- `crates/ghostty_vt_sys` requires `zig`. If `zig` is not in `PATH`, it will use `.context/zig/zig`.
- You can also set `ZIG=/path/to/zig` to override discovery.

## Running Examples

VT dump:

```sh
printf '\033[31mred\033[0m\n' | cargo run -p vt_dump
```

GPUI demos:

```sh
cargo run -p basic_terminal
cargo run -p pty_terminal
cargo run -p split_pty_terminal
```

## Public API (gpui_ghostty_terminal)

Crate root re-exports the stable entry points:

- `TerminalConfig`
- `TerminalSession`
- `default_terminal_font`, `default_terminal_font_features`
- `view::{TerminalView, TerminalInput, Copy, Paste, SelectAll}`

Embed-friendly options:

- Disable window title updates (useful when embedding into a host app that owns titles):

```rust
use gpui_ghostty_terminal::TerminalConfig;

let config = TerminalConfig {
    update_window_title: false,
    ..TerminalConfig::default()
};
```

## Compatibility Notes

This implementation includes common terminal behaviors needed by modern TUIs:

- DSR replies (`CSI 5n` / `CSI 6n`) for cursor position/status queries
- OSC title tracking (OSC 0/2), OSC 52 clipboard write
- OSC 10/11 default foreground/background queries
- SGR mouse modes + scrollback navigation bindings
- IME composition support (commit + preedit overlay)
- DEC Special Graphics (ACS line drawing) + box drawing (procedural quads)

The examples set `TERM=xterm-256color` and `COLORTERM=truecolor` to help apps enable richer output.

## Settings

`ctrl+,` — or the little gear in the corner — opens k4term's own settings:
font size, glass, cursor trail and quiet mode. Pick with `↑↓`, change with
`←→`, `esc` closes. It works with the mouse too: click a row to cycle its
value (right-click goes back), and click outside to close. The server picker
is the same — click a server to connect, right-click to just select it, click
a field of the form to jump to it.

They are written to `~/.config/k4term/k4term.conf`, line by line, leaving your
comments and any keys not offered here untouched — that file stays the source
of truth and can still be edited by hand (font family, padding, shell, corner
radius). The window watches it, so a change shows up immediately in every open
window, no restart.

k4's bar writes the same file from its own Settings panel; the two coexist on
purpose. But k4term no longer *needs* the bar to be configurable: that was the
gap — with the terminal alone, the gear opened a panel that was not there.

## Saved servers and passwords

`ctrl+shift+S` opens the server list, read from `~/.ssh/config` (plus k4's own
extras in `~/.config/k4term/hosts.json`: favourites, tags, tint, tunnels).

A server may also carry a **password**, for machines that ask for one instead of
using a key. Two things are worth knowing before you use it:

- It is stored in `~/.config/k4term/claves.json` with `600` permissions **in
  clear text** — the same deal as an SSH key without a passphrase: anyone who
  has your user account has it. It never goes into `~/.ssh/config` nor into
  `hosts.json`, because those get opened, shown and copied around without a
  second thought. If a working Secret Service ever shows up on the machine,
  `servidores::ruta_claves` is the single place to change.
- It is delivered by the terminal itself: k4term watches the PTY and types it
  when the other side asks. No `sshpass`, no extra binary — and, for the same
  reason, it only works inside k4term (the window or the island session), not
  in someone else's terminal.

Saved hosts get `StrictHostKeyChecking accept-new` in their block, so the
first-time fingerprint question does not interrupt the connection. A key that
*changes* still stops it, which is the case that matters.

## Agents and SSH

Anything running inside k4term — an AI agent, a script, a build — already has
your shell, so it can run `ssh` on its own. What it cannot do is type a
password: its commands go through pipes, not through the PTY the terminal
watches. Handing it your password would hand it everything.

So it gets something else. `ctrl+G` on a server opens **the agents' door**:

- a **dedicated key**, `~/.ssh/k4-agentes`, created on the spot if it is not
  there and sent with `ssh-copy-id` — you see the command run, because it asks
  for your password and that is not done behind your back;
- a **dedicated alias**, `<server>-agentes`, pointing at the same machine but
  with `IdentitiesOnly yes`, so that key and only that key gets offered. On the
  server side you can then restrict it (`restrict`, `command=…`) in
  `authorized_keys` and know that whoever came in through that door came as the
  agent, not as you;
- revoking is `ctrl+G` again: the alias goes, and the terminal runs the command
  that deletes that key's line over there. The key carries a mark
  (`k4-agentes@<your machine>`) precisely so the line can be found.

Servers with the door open show a `⚙` in the list. The `-agentes` aliases do
not show up as entries of their own: they are a permission on a server, not
another place to go.

Every session also gets the **names** of your servers in its environment —
`K4_SERVIDORES`, and `K4_SERVIDORES_AGENTES` for the ones with the door open.
Names only: no hosts, no users, no secrets. It is enough for an agent to know
that `casa` exists and offer `ssh casa` instead of asking you for the IP. The
list is read when the session starts, so a server added afterwards shows up in
the next one.

## License

This project is licensed under the Apache License, Version 2.0. See `LICENSE`.

This repository vendors Ghostty as a git submodule under `vendor/ghostty`; third-party code remains under its respective licenses.
