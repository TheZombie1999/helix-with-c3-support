<div align="center">

<h1>
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="logo_dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="logo_light.svg">
  <img alt="Helix" height="128" src="logo_light.svg">
</picture>
</h1>

[![Build status](https://github.com/helix-editor/helix/actions/workflows/build.yml/badge.svg)](https://github.com/helix-editor/helix/actions)
[![GitHub Release](https://img.shields.io/github/v/release/helix-editor/helix)](https://github.com/helix-editor/helix/releases/latest)
[![Documentation](https://shields.io/badge/-documentation-452859)](https://docs.helix-editor.com/)
[![GitHub contributors](https://img.shields.io/github/contributors/helix-editor/helix)](https://github.com/helix-editor/helix/graphs/contributors)
[![Matrix Space](https://img.shields.io/matrix/helix-community:matrix.org)](https://matrix.to/#/#helix-community:matrix.org)

</div>

![Screenshot](./screenshot.png)

A [Kakoune](https://github.com/mawww/kakoune) / [Neovim](https://github.com/neovim/neovim) inspired editor, written in Rust.

The editing model is very heavily based on Kakoune; during development I found
myself agreeing with most of Kakoune's design decisions.

For more information, see the [website](https://helix-editor.com) or
[documentation](https://docs.helix-editor.com/).

All shortcuts/keymaps can be found [in the documentation on the website](https://docs.helix-editor.com/keymap.html).

[Troubleshooting](https://github.com/helix-editor/helix/wiki/Troubleshooting)

# Features

- Vim-like modal editing
- Multiple selections
- Built-in language server support
- Smart, incremental syntax highlighting and code editing via tree-sitter
- Remote control over D-Bus on Unix, with an `hx-remote` CLI (see
  [Remote control](#remote-control))

Although it's primarily a terminal-based editor, I am interested in exploring
a custom renderer (similar to Emacs) using wgpu.

Note: Only certain languages have indentation definitions at the moment. Check
`runtime/queries/<lang>/` for `indents.scm`.

# Remote control

On Unix, a running `hx` instance exposes a D-Bus interface (enabled by
default via the `dbus` cargo feature) that lets other processes open,
close, reload, and watch files in it. See
[`helix-dbus/helix1.xml`](./helix-dbus/helix1.xml) for the interface
definition, and
[`helix-term/src/dbus/mod.rs`](./helix-term/src/dbus/mod.rs) for the server
implementation.

`hx-remote` is a lightweight CLI client for it:

```
hx-remote open <path>[:LINE[:COL]]...   # open (and switch to) one or more files
hx-remote edit <path>...                # like `open --wait`; usable as $EDITOR
hx-remote changed                       # open every file `git status` reports as changed
hx-remote close <path>...               # close open buffers
hx-remote reload [<path>...]            # reload buffers from disk (defaults to everything open)
hx-remote watch                         # auto-reload files as they change on disk
hx-remote list-files                    # list a running instance's open buffers
hx-remote list-instances                # list running instances
```

Every command accepts `--instance NAME` or `--pid PID` to target one
specific running instance, or `--all` to target every running instance at
once. Without any of those, a command targets whichever instance claimed
the shared `org.helix_editor.Helix` bus name (normally the first one
started). Run `hx-remote help` for the full reference, including `open`'s
`--wait`/`--no-focus` flags and `watch`'s `--interval`.

Bash completion for `hx-remote` is available at
[`contrib/completion/hx-remote.bash`](./contrib/completion/hx-remote.bash).
[`justfile`](./justfile) also has `busctl`-based recipes (`just open`,
`just edit`, `just open-files`, `just list`) for talking to the interface
directly, without building `hx-remote`.

# Installation

[Installation documentation](https://docs.helix-editor.com/install.html).

[![Packaging status](https://repology.org/badge/vertical-allrepos/helix-editor.svg?exclude_unsupported=1)](https://repology.org/project/helix-editor/versions)

# Contributing

Contributing guidelines can be found [here](./docs/CONTRIBUTING.md).

# Getting help

Your question might already be answered on the [FAQ](https://github.com/helix-editor/helix/wiki/FAQ).

Discuss the project on the community [Matrix Space](https://matrix.to/#/#helix-community:matrix.org) (make sure to join `#helix-editor:matrix.org` if you're on a client that doesn't support Matrix Spaces yet).

# Credits

Thanks to [@jakenvac](https://github.com/jakenvac) for designing the logo!
