---
name: aterm-themes
description: Design, write, check, preview or import an aterm colour theme (a .conf in the themes folder), or pick a light/dark pair. Use for "make a theme", "terminal colours", "colour scheme", "dark:…,light:…", "aterm theme", "import my kitty/Ghostty/iTerm2 theme", or a theme file that will not load.
---

<!-- aterm skill v1 — MANAGED FILE, written by `aterm agents install` and by aterm itself.
     Edits are overwritten on update. To keep your own version, remove this
     marker line and aterm will leave the file alone (reported as `foreign`). -->

# aterm themes: design, check, preview, commit

An aterm theme is one `<id>.conf` file in the themes folder (`~/.config/aterm/themes`, or
`$XDG_CONFIG_HOME/aterm/themes`; `%APPDATA%\aterm\themes` on Windows). The file name without
`.conf` is the id that `theme = "<id>"` names in `aterm.toml`. `aterm help themes` is the
reference. The authoring guide is `contrib/themes/GUIDE.md` in the aterm source.

## The loop

```sh
aterm theme schema                                   # every key, as JSON: read this, don't guess keys
aterm theme new "<Name>" --from "<closest theme>"    # prints the path it wrote; every key commented
# edit the file
aterm theme check "<path>" --strict --format json    # exit 0 = loads clean; 1 = warnings; 2 = does not load
aterm ctl theme preview "<Name>" ttl=300             # draw it in the running window; nothing written
aterm ctl theme                                      # read back what is on screen (id, source, palette)
aterm ctl theme revert                               # or, when the person wants it kept:
aterm ctl theme commit                               # writes theme = "<Name>" to aterm.toml
```

- `aterm theme list --format tsv` lists every theme with its appearance, pair and status.
  `aterm theme export "<theme>"` prints any theme, built-ins included, as a file to start from.
- `check --format json` gives `status`, `diagnostics[]` (`code`, `line`, `key`, `message`) and
  `contrast[]` (`drawn`, `on`, `ratio`, `minimum`, `below_minimum`). Iterate until `status` is
  `ok` with `--strict`.
- Saving the file redraws a window showing that theme, a standing `ctl theme preview` of it
  included: the preview stays up across the save. Deleting the file, or any `aterm.toml`
  reload, ends the preview. `ctl theme preview` needs a running aterm window.
- The window polls the themes folder about every 2 s, so a file `theme new` just wrote is
  refused as unavailable until then: preview once `aterm ctl theme list` shows its id.
- Write `aterm.toml` (`aterm ctl theme commit`, or `aterm ctl theme set "<id>"` /
  `aterm ctl theme set "dark:<id>,light:<id>"`) only when the person asked for the theme to be
  kept. Preview is the way to show a theme.

## Design targets

Work in OKLCH (lightness, chroma, hue), not HSL: equal OKLCH lightness looks equally bright.
`check` enforces the floor. Aim for aterm's signature bar, and measure it with
`aterm theme check <file> --signature` (with `--format json`, `signature[]` lists each missed bar
as `{code, message}`; empty means every bar is cleared). "Accents" are ANSI 1-6 and 9-14.

| What | Floor (`check --strict`) | Target (aterm's own themes) |
|---|---|---|
| foreground on background | 3:1 | 7:1 |
| each accent (ANSI 1-6, 9-14) on background | — | 4.5:1 each |
| hue spacing within 1-6, and within 9-14 | — | at least 25° apart |
| each bright vs its normal | — | OkLab distance at least 0.05 |
| `color8` (comments, suggestions) | — | between 3:1 and 4.5:1 |
| `color0` on background | not equal to it | at least 1.15:1 |
| `color7`, `color15` on background | not equal to it | 4.5:1 |
| selection band on background | 1.3:1 | 1.5:1, with foreground on it at 4.5:1 |
| cursor on background | 3:1 | 3:1, OKLCH chroma at least 0.08, within 15° of an accent hue (1-6) |
| accent chroma, both tiers (1-6 and 9-14) | — | 0.07-0.17, highest at most 1.75 times the lowest |
| `appearance` | matches the measured background | set it explicitly |

Method: choose the ground, then the ink at 7:1. Put all six accents at **one** OKLCH lightness
and chroma, with spread hues, and raise or lower that shared lightness until the dimmest clears
4.5:1. A light theme's yellow at 4.5:1 is necessarily a dark ochre (OKLCH L under about 0.6):
push its chroma to the gamut edge at hue 70-85 so it reads gold, not mud, and keep bright yellow
within about 0.08 L of it (or shift it toward orange). Brights are the accents moved one step
in lightness. Take the cursor from an accent hue. No ANSI slot may equal
the background.

## Light/dark pairs

`theme = "dark:A,light:B"` follows the OS appearance live. A pair is two files with the same
hues, re-lit for each ground, each with `appearance` set. A value naming one side only draws
that side's built-in pair on the other (`dark:Hearth` is Linen in light mode), else the named
side. The signature pairs are Aterm Night / Aterm Day and Hearth / Linen.

## A theme must never contain

A theme file is untrusted text, and the parser refuses (the file does not load):

- any key not in `aterm theme schema`: no command, shell, key binding, permission, URL or path
  (no wallpaper, include or import), no font, size, opacity or padding, and nothing that moves
  or makes a sound;
- a key written twice, in any spelling (`cursor` and `cursor_color` are one key);
- another terminal's syntax. Convert it with `aterm theme import <file> [--name <id>]`, which
  reads kitty, Ghostty, Alacritty, iTerm2, Windows Terminal and base16/base24 offline, under
  256 KiB, and lists every dropped key.

Do not name a file after a built-in (`aterm theme list` shows them). The built-in always wins,
and the file is reported as shadowed.

## When it does not work

- `aterm theme check` names the line and key. A `foreign_format` field means use
  `aterm theme import`.
- `aterm ctl theme` shows `miss="…"` when the configured name cannot be drawn (Default is drawn
  instead), and why.
- Colour keys in `aterm.toml` (`foreground`, `background`, `cursor_color`, `selection_color`,
  `selection_foreground`, `palette`) override every theme. Check for them before blaming the
  theme.
- Colours a program set itself (an editor's background, a base16 shell palette) survive a theme
  switch by design.
