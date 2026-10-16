<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Community themes

This folder holds aterm colour themes that people share. To use one, copy its `.conf` into your
themes folder (`~/.config/aterm/themes` on macOS and Linux, `%APPDATA%\aterm\themes` on
Windows) and pick it in Settings ▸ Themes, or set `theme = "<id>"` in `aterm.toml`, where the id
is the file name without `.conf`.

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
   licence. A port of another project's palette names that project in `description`.

A theme file holds colours and its identity, nothing else. No key runs a command, names a path,
or sets a font or a size, and an unknown key stops the file loading, so a theme can't do more
than recolour the terminal.
