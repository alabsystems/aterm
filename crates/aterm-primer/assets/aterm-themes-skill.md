---
name: aterm-themes
description: Design, write, check, preview, import or install an aterm colour theme (a .conf in the themes folder), or pick a light/dark pair. Use for "make a theme", "terminal colours", "colour scheme", "dark:…,light:…", "aterm theme", "install Catppuccin/Kanagawa/Tokyo Night", "import my kitty/Ghostty/iTerm2/Terminal.app theme", or a theme file that will not load.
---
<!-- aterm skill v3 — MANAGED FILE, rewritten by `aterm agents` on every install/update; remove this marker line and aterm will leave the file alone (reported as `foreign`) -->

# aterm themes: design, check, preview, commit

An aterm theme is one `<id>.conf` file in `~/.config/aterm/themes` (`$XDG_CONFIG_HOME/aterm/themes`,
or `%APPDATA%\aterm\themes` on Windows). The filename stem is the id `theme = "<id>"` names in
`aterm.toml`. `aterm help themes` is the reference; `contrib/themes/GUIDE.md` in the aterm source
is the authoring guide.

## A well-known theme first

Before writing or importing one, check the community pack: well-known themes (Catppuccin,
Kanagawa, Tokyo Night, Rose Pine, GitHub, Iceberg, Nightfox, Melange, ...) with permissive
licences ship inside aterm, already converted and checked.

```sh
aterm theme pack --format tsv                        # header row names columns; read by name
aterm theme install "<id>"                           # copies <id>.conf into the themes folder
```

`install` prints the `theme = "dark:…,light:…"` value when the theme has a pair. A theme not in
the pack goes through `aterm theme import` (below).

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

- `aterm theme list --format tsv` lists every theme with its appearance, pair and status;
  `aterm theme export "<theme>"` prints any theme (built-ins included) as a starter file.
- `check --format json` gives `status`, `diagnostics[]` (`code`, `line`, `key`, `message`) and
  `contrast[]` (`drawn`, `on`, `ratio`, `minimum`, `below_minimum`). Iterate until `status` is
  `ok` under `--strict`.
- Saving the file redraws a window showing that theme (including a standing `ctl theme preview`);
  deleting the file or reloading `aterm.toml` ends the preview. The window polls the themes
  folder every ~2 s, so preview once `aterm ctl theme list` shows a newly written id.
- Write `aterm.toml` (`aterm ctl theme commit`, or `aterm ctl theme set "<id>"` /
  `aterm ctl theme set "dark:<id>,light:<id>"`) only when the person asked to keep the theme.

## Design targets

Work in OKLCH (lightness, chroma, hue), not HSL: equal OKLCH lightness looks equally bright.
`check` enforces the floor; `aterm theme check <file> --signature` measures aterm's signature bar
(`--format json` lists missed bars in `signature[]` as `{code, message}`). Accents are ANSI 1-6
and 9-14.

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
and chroma with spread hues, then adjust that shared lightness until the dimmest clears 4.5:1.
On a light ground, yellow at 4.5:1 is a dark ochre (OKLCH L < ~0.6): push chroma to the gamut
edge at hue 70-85 so it reads gold, and keep bright yellow within ~0.08 L (or shift toward
orange). Brights step the accents in lightness; take the cursor from an accent hue. No ANSI slot
may equal the background.

## Chrome colours (optional)

Four keys colour aterm's own surfaces; omitted, each is derived from the theme:

| Key | What | Derived when left out |
|---|---|---|
| `accent_color` | selected tab's rule, band meter, Settings selection, find tint hue | cursor if it has colour, else ANSI blue or cyan |
| `active_tab_color` | selected tab's background (label black or white) | a raised card in the theme's tones |
| `border_color` | split dividers, tab-strip and find-bar seams | background stepped 30% toward foreground |
| `search_match_color` | find highlight on every match but the current (the current is the selection) | background tinted toward the accent, apart from the selection |

Precedence: `aterm.toml`'s key, then the theme's, then derived. Each is raised to its floor where
drawn (accent 3:1, border and match tint 1.5:1); `check` warns `chrome_below_floor` /
`search_match_like_selection`. High Contrast ignores them. `aterm ctl theme` reports `accent=`,
`accent_source=`, `accent_from=` and the others.

## Fonts in a theme (optional)

A theme may set typography keys spelled as in `aterm.toml`: `font_family` (and
`font_family_bold`/`_italic`/`_bold_italic`), `fallback_fonts` (comma list, at most 8),
`symbol_font`, `emoji_font`, `line_height` (0.8-2.0), `ligatures`, `font_features`
(`calt, -liga, ss01=2`), `font_synthetic_style`, `font_weight` (1-1000) and `font_size_scale`
(0.8-1.25). Name fonts by family (`display:<id>` for aterm's display faces), never a
file/path or `font_px`.

- `aterm.toml` keys win as whole values; `--font` and `display_font` beat a theme's family; an
  uninstalled font falls back to the user's `font_family`, then aterm's default face.
- `aterm theme check <file> --strict` lists typography and notes `font_not_installed` (still
  passes `--strict`). `aterm ctl theme` reports `typography=`, `typography_keys=` and
  `typography_shadowed=`.
- Try and `ctl theme preview` preview colours only; the face applies live when kept (`ctl theme
  commit`, Keep). Settings' "Keep colours only" pins the person's type settings (withheld when
  the theme sets `font_weight`).

## The 256-colour palette and per-tab themes

- `palette_generate = true` derives slots 16-255 (6x6x6 cube + grey ramp) from the theme's bright
  row and endpoints (`color0`/`color15` and lightest colour). Enable it for original themes;
  leave it off for ports whose vim/tmux companions expect xterm slots. `aterm.toml`'s
  `palette_generate = "always"|"never"` overrides it; `aterm ctl theme palette` lists all 256
  slots with `origin=theme|config|default|generated|xterm`.
- `bold_is_bright = true|false` sets whether bold text in ANSI 0-7 draws in the bright row
  (`aterm.toml`'s key wins).
- Per-tab theme (without touching `aterm.toml`): `aterm ctl @<sid> theme session set "<Name>"`
  (`theme session` reads, `theme session clear` clears), or `aterm ctl spawn theme="<Name>"`.
  Splits inherit it, updates/restores preserve it, `theme preview` skips it (`pinned=`), and
  `@<sid> status` reports `session_theme=` and `theme_origin=`.

## Light/dark pairs

`theme = "dark:A,light:B"` follows OS appearance live. A pair is two files with the same hues
re-lit for each ground and `appearance` set. Naming one side pairs it with its built-in partner
(`dark:Hearth` draws Linen in light mode). Signature pairs: Aterm Night / Aterm Day and Hearth /
Linen.

## A theme must never contain

A theme file is untrusted text; the parser refuses to load:

- any key outside `aterm theme schema` (no command, shell, key binding, permission, URL, path,
  wallpaper, include, font file, `font_px`, opacity, padding, motion or sound); a file whose
  `schema` is newer than this aterm's loads with the keys it does not know skipped and listed;
- duplicate keys, including aliases (`cursor` and `cursor_color` are one key);
- foreign terminal syntax: convert with `aterm theme import <file> [--name <id>]` (kitty, Ghostty,
  Alacritty, WezTerm, iTerm2, Terminal.app, Windows Terminal, base16/base24 offline, <256 KiB;
  prints dropped keys);
- a built-in theme's name (`aterm theme list`): built-ins always win and shadow the file.

## When it does not work

- `aterm theme check` names the failing line and key (a diagnostic's `foreign_format` means
  run `theme import`).
- `aterm ctl theme` shows `miss="…"` when the configured theme cannot be drawn.
- Top-level colour, chrome and font keys in `aterm.toml` override every theme.
- Program-set colours (OSC 4/10/11, base16-shell, pywal) survive theme switches.
  `allow_palette_reconfigure = false` refuses and drops what programs set in the 256 indexed
  colours (OSC 4/21); foreground, background and cursor (OSC 10/11/12) are not gated.
  Inspect them with `aterm ctl @<sid> colors` (`program_palette=`, `program_slots=`,
  `palette_sets=`) and clear with `aterm ctl @<sid> reset`.
