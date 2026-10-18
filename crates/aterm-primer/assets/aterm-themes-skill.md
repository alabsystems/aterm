---
name: aterm-themes
description: Design, write, check, preview, import or install an aterm colour theme (a .conf in the themes folder), or pick a light/dark pair. Use for "make a theme", "terminal colours", "colour scheme", "dark:…,light:…", "aterm theme", "install Catppuccin/Kanagawa/Tokyo Night", "import my kitty/Ghostty/iTerm2 theme", or a theme file that will not load.
---

<!-- aterm skill v1 — MANAGED FILE, written by `aterm agents install` and by aterm itself.
     Edits are overwritten on update. To keep your own version, remove this
     marker line and aterm will leave the file alone (reported as `foreign`). -->

# aterm themes: design, check, preview, commit

An aterm theme is one `<id>.conf` file in the themes folder (`~/.config/aterm/themes`, or
`$XDG_CONFIG_HOME/aterm/themes`; `%APPDATA%\aterm\themes` on Windows). The file name without
`.conf` is the id that `theme = "<id>"` names in `aterm.toml`. `aterm help themes` is the
reference. The authoring guide is `contrib/themes/GUIDE.md` in the aterm source.

## A well-known theme first

Before writing or importing one, look in the community pack: well-known themes (Catppuccin,
Kanagawa, Tokyo Night, Rose Pine, GitHub, Iceberg, Nightfox, Melange, ...) whose projects carry
a permissive licence ship inside aterm, already converted and checked.

```sh
aterm theme pack --format tsv                        # a header row names the columns; read by name
aterm theme install "<id>"                           # copies <id>.conf into the themes folder
```

`install` prints the pair's `theme = "dark:…,light:…"` value when the theme has one. A theme
not in the pack goes through `aterm theme import` (below).

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
| `appearance` | matches the measured background (light when black text out-contrasts white on it) | set it explicitly |

Method: choose the ground, then the ink at 7:1. Put all six accents at **one** OKLCH lightness
and chroma, with spread hues, and raise or lower that shared lightness until the dimmest clears
4.5:1. A light theme's yellow at 4.5:1 is necessarily a dark ochre (OKLCH L under about 0.6):
push its chroma to the gamut edge at hue 70-85 so it reads gold, not mud, and keep bright yellow
within about 0.08 L of it (or shift it toward orange). Brights are the accents moved one step
in lightness. Take the cursor from an accent hue. No ANSI slot may equal
the background.

## Chrome colours (optional)

Four keys colour aterm's own surfaces, not the terminal text; leave them out and aterm derives
each from the theme:

| Key | What | Derived when left out |
|---|---|---|
| `accent_color` | selected tab's rule, band meter, Settings selection, find tint hue | cursor if it has colour, else ANSI blue or cyan |
| `active_tab_color` | selected tab's background (label black or white) | a raised card in the theme's tones |
| `border_color` | split dividers, tab-strip and find-bar seams | background stepped 30% toward foreground |
| `search_match_color` | find highlight on every match but the current (the current is the selection) | background tinted toward the accent, apart from the selection |

Precedence: `aterm.toml`'s key of the same name, then the theme's, then the derived colour.
Each is still raised to its floor where drawn (accent 3:1, border and match tint 1.5:1);
`check` warns `chrome_below_floor` / `search_match_like_selection`. High Contrast ignores them.
`aterm ctl theme` reports `accent=`, `accent_source=`, `accent_from=` and the others. Older
aterm refuses a file that sets them (unknown key).

## Fonts in a theme (optional)

A theme may name fonts and set the type around them, each key spelled as in `aterm.toml`:
`font_family` (and `font_family_bold`/`_italic`/`_bold_italic`), `fallback_fonts` (comma list,
at most 8), `symbol_font`, `emoji_font`, `line_height` (0.8-2.0), `ligatures`, `font_features`
(`calt, -liga, ss01=2`), `font_synthetic_style`, `font_weight` (1-1000) and `font_size_scale`
(0.8-1.25). A font is named by family, never a file or a path (`display:<id>` names aterm's
display faces); a theme never sets `font_px`. `bold_is_bright` and `schema` are optional too.

- The `aterm.toml` key of the same name wins (as a whole value), and `--font` and
  `display_font` beat a theme's family.
- A font that is not installed falls to the user's `font_family`, else aterm's default face.
- Verify: `aterm theme check <file> --strict` lists the typography and notes
  `font_not_installed` (a note: `--strict` still passes). In a window, `aterm ctl theme` shows
  `typography=`, `typography_keys=` (what applies) and `typography_shadowed=` (what the config or
  `--font` beat).
- Try and `ctl theme preview` are colours only; the face applies when the theme is kept
  (`ctl theme commit`, Keep), live. Settings' Keep colours only pins the person's current type
  settings instead, and is withheld for a theme that sets `font_weight` (nothing can pin a
  font's own weight). An older aterm refuses a file with any of these keys: ship a
  colours-only file beside it when that matters.

## The 256-colour palette and per-tab themes

- `palette_generate = true` in a theme derives slots 16-255 (the cube and grey ramp 256-colour
  programs paint in) from the theme: the bright row colours the cube, the darker of
  `color0`/`color15` and the theme's lightest colour anchor black and white. Opt in for a theme
  of your own; leave it off for a port whose vim/tmux companions assume xterm's values.
  `aterm.toml`'s `palette_generate = "always"|"never"` overrides it.
  `aterm ctl theme palette` lists all 256 slots with `origin=theme|config|generated|xterm`.
- One tab can wear its own theme without touching `aterm.toml`:
  `aterm ctl @<sid> theme session set "<Name>"` (`theme session` reads it, `theme session
  clear` hands it back). `aterm ctl spawn theme="<Name>"` gives a newborn one; a split from a
  themed pane inherits it; it survives an update and a restore; `theme preview` does not reach
  it (`ctl theme` shows `pinned=`). `@<sid> status` carries `session_theme=` and
  `theme_origin=`. A name that does not draw is refused and nothing changes.
- `identity_themes = { worker = "Hearth" }` in `aterm.toml` themes every session spawned
  under that identity (`spawn identity=worker`) unless it has a theme of its own.

## Light/dark pairs

`theme = "dark:A,light:B"` follows the OS appearance live. A pair is two files with the same
hues, re-lit for each ground, each with `appearance` set. A value naming one side only draws
that side's built-in pair on the other (`dark:Hearth` is Linen in light mode), else the named
side. The signature pairs are Aterm Night / Aterm Day and Hearth / Linen.

## A theme must never contain

A theme file is untrusted text, and the parser refuses (the file does not load):

- any key not in `aterm theme schema`: no command, shell, key binding, permission, URL or path
  (no wallpaper, include, import or font file), no `font_px`, opacity or padding, and nothing
  that moves or makes a sound;
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
  `selection_foreground`, `palette`, and the chrome keys `accent_color`, `active_tab_color`,
  `border_color`, `search_match_color`) and its font keys override every theme. Check for them
  before blaming the theme.
- Colours a program set itself (an editor's background, a base16 shell palette) survive a theme
  switch by design. base16-shell, pywal and theme.sh work by default (OSC 4); only
  `allow_palette_reconfigure = false` refuses them.
- To see what a program set, read `aterm ctl @<sid> colors`: `program_palette=` lists the
  indexed colours over the theme, `program_slots=` the named ones, `palette_sets=` the gate.
  `aterm ctl @<sid> reset` puts the theme's colours back.
