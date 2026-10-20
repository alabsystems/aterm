<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Community themes

This folder holds aterm colour themes that people share. To use one, copy its `.conf` into your
themes folder (`~/.config/aterm/themes` on macOS and Linux, `%APPDATA%\aterm\themes` on
Windows) and pick it in Settings ▸ Themes, or set `theme = "<id>"` in `aterm.toml`, where the id
is the file name without `.conf`.

## The community pack

[`pack/`](pack/README.md) holds well-known themes by other authors (Catppuccin, Kanagawa,
Tokyo Night, Rose Pine, the GitHub themes, Iceberg and more) whose own projects carry a
permissive licence. aterm carries them inside it: `aterm theme pack` lists them,
`aterm theme install "<id>"` (or `--all`) copies one into your themes folder, and Settings ▸
Themes ▸ Community adds one when you Keep it. `aterm theme pack --licences` prints each palette's
attribution and licence. Nothing is downloaded. [`pack/README.md`](pack/README.md) says which
themes qualify and how to add one.

## Contributing a theme

1. Start from [`TEMPLATE.conf`](TEMPLATE.conf), or run `aterm theme new "<Your Theme>"`, which
   writes the same commented layout into your themes folder.
2. Design it against the bar in the [authoring guide](GUIDE.md).
   `aterm theme check <file> --signature` measures the stricter bar aterm's own themes clear,
   and `aterm theme export "Aterm Night"` (or any name `aterm theme list` shows) prints a
   built-in as a file to read.
3. Add **one `.conf` per theme** to this folder, named for its id. Set `name`, `description`,
   `author` and `appearance`. A light/dark pair is two files.
4. It must pass **`aterm theme check <file> --strict`**. The test
   `contrib_themes_pass_the_strict_check` in `crates/aterm-cli/src/theme_author.rs` runs the
   checker over every `.conf` here, so a contribution that fails it fails the build.
5. Contribute only your own work, or work you have the right to share under this repository's
   licence. A port of another project's palette goes in the [pack](pack/README.md) instead,
   vendored with its upstream file and licence, and only when that project's licence is
   permissive.

A theme file holds colours, its identity and, optionally, type settings: font families by name
and values such as `line_height` or `font_size_scale`. No key runs a command, names a path or a
font file, or sets a size in pixels, and an unknown key stops the file loading (unless the file
declares a newer `schema`), so a theme can't do more than restyle the terminal.
