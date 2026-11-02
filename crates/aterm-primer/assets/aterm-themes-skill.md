---
name: aterm-themes
description: Design, write, check, preview, import or install an aterm colour theme (a .conf in the themes folder), or pick a light/dark pair. Use for "make a theme", "terminal colours", "colour scheme", "dark:…,light:…", "aterm theme", "install Catppuccin/Kanagawa/Tokyo Night", "import my kitty/Ghostty/iTerm2/Terminal.app theme", or a theme file that will not load.
---
<!-- aterm skill v4 — MANAGED FILE, rewritten by `aterm agents` on every install/update; remove this marker line and aterm will leave the file alone (reported as `foreign`) -->

# aterm themes: design, check, preview, commit

A theme is one `<id>.conf` in `$XDG_CONFIG_HOME/aterm/themes` (else `~/.config/aterm/themes`;
`%APPDATA%\aterm\themes` on Windows). The file name stem is the id `theme = "<id>"` in
`aterm.toml` names. Reference: `aterm help themes` and `aterm theme help`.

## A well-known theme first

Check the built-ins (`aterm theme list`) and the community pack (Catppuccin, Kanagawa, Rose
Pine, Nightfox, ...):

```sh
aterm theme pack --format tsv      # read columns by the header row's names
aterm theme install "<id>"         # copies <id>.conf into the folder
```

`install` prints the `theme = "dark:…,light:…"` value when the theme has a pair. `ctl theme
preview` draws a pack id uninstalled; `commit` installs it. Anything else: `aterm theme import`.

## The loop

```sh
aterm theme schema                                  # every key, as JSON: never guess keys
aterm theme new "<Name>" --from "<closest theme>"   # prints the path; every key commented
# edit the file
aterm theme check "<path>" --strict --format json   # exit 0 clean, 1 warnings, 2 does not load
aterm ctl theme preview "<Name>" ttl=300            # live, nothing written (default 120 s, max 3600)
aterm ctl theme                                     # what is on screen
aterm ctl theme revert                              # or, when the person wants it kept:
aterm ctl theme commit                              # writes theme = "<Name>" to aterm.toml
```

- JSON has `status`, `diagnostics[]` (`code`, `line`, `key`, `message`) and `contrast[]`
  (`drawn`, `on`, `ratio`, `minimum`, `below_minimum`). Iterate until `status` is `ok`.
- The window rescans the folder about every 2 s and refuses to preview an unknown name: preview
  a new id once `aterm ctl theme list` shows it. Saving the file redraws a window showing it; a
  config change or deleting the file ends a preview.
- Write `aterm.toml` (`commit`, or `aterm ctl theme set "<id>"` / `"dark:<id>,light:<id>"`) only
  when the person asked to keep the theme.

## Design targets

Work in OKLCH, not HSL: equal OKLCH lightness looks equally bright. Accents are ANSI 1-6 and
9-14.

- Floors (`check` warns below one; `--strict` fails): foreground 3:1, selection band 1.3:1,
  cursor 3:1; no ANSI slot equal to the background; `appearance` matching the background (light
  when black text out-contrasts white on it), else `appearance_mismatch`.
- aterm's own bar (`--signature`; a miss fails `--strict`, JSON lists it in `signature[]`):
  foreground 7:1; each accent 4.5:1; `color8` 3:1 to under 4.5:1; `color0` 1.15:1; `color7` and
  `color15` 4.5:1; hues within 1-6, and within 9-14, 25° apart; each bright 0.05 OkLab from its
  normal; selection band 1.5:1 with foreground 4.5:1 on it; cursor 3:1 with OKLCH chroma 0.08 or
  more, within 15° of an accent (1-6) hue; accent chroma 0.07-0.17 in both tiers, highest at
  most 1.75 times lowest.

Method: pick the ground, then ink at 7:1. Put all six accents at **one** OKLCH lightness and
chroma with spread hues; move that lightness until the dimmest clears 4.5:1. On a light ground
yellow at 4.5:1 is dark ochre (L below ~0.6): push its chroma to the gamut edge at hue 70-85,
and keep bright yellow within ~0.08 L (or shift it toward orange). Brights step the accents in
lightness; take the cursor from an accent hue.

## Optional keys

- Chrome: `accent_color`, `active_tab_color`, `border_color` and `search_match_color` (find
  matches but the current). Omitted, each is derived. Each is raised to its floor (accent 3:1,
  border and match 1.5:1); `check` warns `chrome_below_floor` or `search_match_like_selection`.
- Fonts: the typography keys `aterm.toml` spells the same way (`font_family`, `line_height`,
  `font_weight`, ...; ranges in `schema`). Name a font by family (`display:<id>` for aterm's
  display faces). An uninstalled one falls back (note `font_not_installed`, which passes
  `--strict`). A preview shows colours only; the face applies once kept. `--font` beats a
  theme's family; `display_font` draws its layer over that family, which stays the font.
  `aterm ctl theme` reports `typography=` and `typography_shadowed=`.
- `palette_generate = true` derives slots 16-255 from the theme: on for original themes, off for
  ports whose vim/tmux companions expect xterm slots. `aterm ctl theme palette` lists each
  slot's `origin=`.
- `bold_is_bright = true|false`: whether bold ANSI 0-7 draws in the bright row.

## Per-tab themes and light/dark pairs

- One tab, leaving `aterm.toml` alone: `aterm ctl @<sid> theme session set "<Name>"` (`theme
  session` reads, `theme session clear` clears), or `aterm ctl spawn theme="<Name>"`. Splits
  inherit it, and `theme preview` skips it (`pinned=` counts such tabs).
- `theme = "dark:A,light:B"` follows the OS appearance live: two files, the same hues re-lit for
  each ground, each with `appearance` set. One side alone draws its built-in partner on the other
  (`dark:Hearth` draws Linen in light mode).

## A theme must never contain

A theme file is untrusted text. The parser refuses to load:

- any key outside `aterm theme schema` (no command, key binding, URL, path, include, font file
  or `font_px`), though a file with a newer `schema` loads, listing the keys it skipped;
- a key written twice, aliases included (`cursor` and `cursor_color` are one key);
- another terminal's syntax: `aterm theme import <file> [--name <id>]` converts kitty, Ghostty,
  Alacritty, WezTerm, iTerm2, Terminal.app, Windows Terminal and base16/base24 (a local file up
  to 256 KiB) and lists the keys it dropped.

A file named after a built-in (in any case) is shadowed: the built-in wins. Rename it.

## When it does not work

- `aterm theme check` names the failing line and key; a diagnostic with `foreign_format` set
  means run `theme import`.
- `aterm ctl theme` shows `miss="…"` when the configured theme cannot be drawn (Default is drawn).
- A colour, chrome, font or `bold_is_bright` key in `aterm.toml` beats every theme's, and so
  does its `palette_generate` set to `"always"` or `"never"`.
- Colours a program set (OSC 4/10/11, pywal) survive theme switches: `aterm ctl @<sid> colors`
  shows them (`program_palette=`), `aterm ctl @<sid> reset` clears them.
  `allow_palette_reconfigure = false` in `aterm.toml` refuses and drops OSC 4/21 sets of the
  256 indexed colours; OSC 10/11/12 are not gated.
