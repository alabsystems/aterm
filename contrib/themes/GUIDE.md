<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Writing an aterm theme

This guide is for anyone who writes a theme for aterm, person or coding agent. It covers the
file format, fonts in a theme, the bar a good theme clears and why, light/dark pairs, the edit
loop, importing from other terminals, and how to share a theme. `aterm help themes` is the short reference, and
Claude Code gets a condensed version as the `aterm-themes` skill (`aterm agents install`).

## Quick start

```sh
aterm theme new "Harbour Night" --from "Aterm Night"   # prints the path it wrote
"$EDITOR" ~/.config/aterm/themes/"Harbour Night.conf"   # that path (macOS and Linux)
aterm theme check ~/.config/aterm/themes/"Harbour Night.conf" --strict
aterm ctl theme preview "Harbour Night"                # in a running window; `theme revert` undoes it
aterm ctl theme commit                                 # or: theme = "Harbour Night" in aterm.toml
```

A running aterm reloads the themes folder when a file in it changes (it polls about every 2 s),
so every save redraws a window showing the theme, a standing `ctl theme preview` of it included.
Preview a new file once `aterm ctl theme list` shows its id; until then it is refused as
unavailable.

## Where themes live

| Platform | Folder |
|---|---|
| macOS, Linux | `$XDG_CONFIG_HOME/aterm/themes`, else `~/.config/aterm/themes` |
| Windows | `%APPDATA%\aterm\themes` |

Each `<id>.conf` there is a theme. The file name without `.conf` is the **id**, the value
`theme = "<id>"` names in `aterm.toml`. The `name =` key inside is only the label people see.
`aterm theme list` shows every theme aterm can draw, with each file's status and, for one that
did not load, the reason. A file named after a built-in, in any case, is **shadowed**: the
built-in always wins, so rename the file.

## The format

```conf
# Harbour Night.conf
name        = Harbour Night
description = Deep harbour blue with lamp-amber accents
author      = Jane Doe
appearance  = dark

foreground  = #d8e1ec
background  = #121721
cursor      = #56d2df
selection   = #2b3a57
color0      = #1f2532
# … color1 through color15
```

- One `key = value` per line. Keys are case-insensitive. A line starting with `#` or `;` is a
  comment. After a value, a comment needs a space or tab before its `#` or `;`
  (`foreground = #d8e1ec  # note`); for the same reason, ` #` or ` ;` inside a name or
  description ends the value there.
- `foreground` and `background` are required. Everything else is optional, and
  `aterm theme schema --format text` says what leaving each key out means.
- A colour is `#RRGGBB` (the `#` may be left out) or `#RGB`.
- An unknown key, a key written twice in any of its spellings, or a bad value is an error that
  names the line, and the file does not load. That is deliberate: a typo should be a diagnostic,
  not a silently ignored colour.

| Key | Also spelled | Value |
|---|---|---|
| `name` | | the label; at most 256 characters |
| `description` | | one line under the label |
| `author` | | who made it |
| `appearance` | | `dark` or `light`; measured from the background when left out: light when black text has more contrast on it than white (greys `#767676` and lighter) |
| `foreground` | `fg` | default text colour (required) |
| `background` | `bg` | default background and window padding (required) |
| `cursor` | `cursor_color` | cursor; follows the foreground when left out |
| `selection` | `selection_color` | selection band; derived from the background when left out |
| `selection_foreground` | | text in the selection; selected text keeps its own colour when left out |
| `color0` … `color15` | `palette<N>`, `ansi<N>` | ANSI 0-7 (black, red, green, yellow, blue, magenta, cyan, white), 8-15 their bright forms |
| `accent_color` | | chrome: the accent; the cursor when it has colour, else the ANSI blue or cyan, when left out |
| `active_tab_color` | | chrome: the selected tab's background; a raised card in the theme's tones when left out |
| `border_color` | | chrome: split dividers and the chrome bands' seams; the background stepped 30% toward the foreground when left out |
| `search_match_color` | | chrome: find's highlight on every match but the current one; a tint toward the accent when left out |
| `bold_is_bright` | | `true` or `false`: bold text in ANSI 0-7 drawn in 8-15 |
| `schema` | | the theme format version the file was written for (see [Fonts in a theme](#fonts-in-a-theme)) |

The typography keys, all optional (see [Fonts in a theme](#fonts-in-a-theme)):

| Key | Value | aterm.toml key that wins |
|---|---|---|
| `font_family` | a family name, never a file or a path; `display:<id>` names an aterm display face | `font_family` (and `display_font`, and `--font`) |
| `font_family_bold`, `font_family_italic`, `font_family_bold_italic` | a family name | the same names |
| `fallback_fonts` | family names, comma-separated, at most 8 | `fallback_fonts` |
| `symbol_font`, `emoji_font` | a family name | the same names |
| `line_height` | 0.8 to 2.0, at most 3 decimals | `line_height` |
| `ligatures` | `true` or `false` | `ligatures` |
| `font_features` | OpenType features (`calt`, `-liga`, `ss01=2`), comma or space separated, at most 16 | `font_features` |
| `font_synthetic_style` | `true` or `false` | `font_synthetic_style` |
| `font_weight` | 1 to 1000 (400 is regular) | `font_weight` |
| `font_size_scale` | 0.8 to 1.25: scales your size or aterm's default | `font_size_scale` |

`aterm theme schema` prints these tables as JSON, from the same table the parser reads keys by,
so a tool built on it cannot disagree with aterm. The colour keys are spelled the way
`aterm.toml` spells its own overrides, except the palette, which `aterm.toml` writes as
`palette = [ … ]`.

### The 256-colour palette (optional)

Programs that paint in the 256-colour palette (vim and tmux colour schemes, `ls` and `git`
with 256-colour settings, many prompts) use slots 16-255: a 6×6×6 colour cube and a 24-step
grey ramp whose xterm values are fixed, so by default they ignore your theme. Add

```conf
palette_generate = true
```

and aterm derives those slots from your theme instead. The cube's colour corners are your
bright row (`color9` red through `color14` cyan); its black and white corners, and the two ends
of the grey ramp, are your darkest ink (`color0` or `color15`, whichever is darker) and your
lightest colour (of `color0`, `color7`, `color15`, foreground and background). Everything in
between is blended in OkLab at xterm's own spacing, so slot 16 is your black, slot 231 your
white, and slot 196 exactly your bright red.

When to opt in: for a theme with a strong identity whose users mostly run programs that pick
from the 16 ANSI colours and the cube's corners. Leave it off for a port of an existing theme
whose 256-colour companions (a vim scheme, a tmux theme) were written against xterm's exact
values. A person can override it either way with `palette_generate = "always"` or `"never"`
in `aterm.toml`, and `aterm ctl theme palette` lists every slot and where it came from.

Polarity: on a light theme `color15` ("bright white") is often the darkest ink, and that is
fine: the anchors are chosen by lightness, never by slot number, so dark slots stay dark and
the grey ramp runs dark to light on every theme. `aterm theme check` names the two anchors and
warns (`palette_anchors_close`) when they are too close in lightness for the cube to be
readable.

### Chrome colours (optional)

Four keys colour aterm's own surfaces rather than the terminal's text. Leave them out and aterm
derives each one from the theme; set one when the derived colour is not the one your theme
means.

- `accent_color` is the chrome accent: the selected tab's rule, the message band's progress
  meter, the selected rows in Settings, the hue find's highlight is tinted toward, the
  drop-target ring. Derived, it is the cursor when the cursor has a colour, else the theme's
  ANSI blue (or cyan, where only cyan carries the band's words), else the grey cursor.
- `active_tab_color` is the selected tab's background. Its label is black or white, whichever
  reads.
- `border_color` draws split-pane dividers and the seams under the tab strip and over the find
  bar and banners.
- `search_match_color` is find's highlight on every match except the current one. The current
  match is the live selection, so it always wears `selection`; keep this apart from it.

Each is still held to a floor where it is drawn: the accent to 3:1 (WCAG 1.4.11) on every
ground it meets, a border or a match tint to 1.5:1. aterm moves a colour that misses its floor
by as little as it can, and `aterm theme check` warns (`chrome_below_floor`, and
`search_match_like_selection` for a match tint that sits on the selection) so you can see where.
Windows High Contrast ignores all four: the system palette owns the chrome then. On macOS the
native tab bar keeps the system accent unless `accent_color` is set. A colour key in `aterm.toml`
spelled the same way (`accent_color = "#..."`) beats the theme's, as the other colour keys do.

An aterm older than the release that added these keys reads them as unknown keys and refuses
the file, so leave them out of a theme you share with people on older versions.

### Fonts in a theme

A theme may name fonts and set the type around them: the typography keys in the table above.
Each one is a setting you can also make in `aterm.toml`, under the same name.

- **Your config wins.** A key set in `aterm.toml` wins over the theme's, as a whole value (an
  empty `fallback_fonts = []` there blocks the theme's list). `--font` wins over both, and so
  does `display_font`. A key neither sets keeps aterm's default.
- **A font that is not installed** falls to the next rung: your `font_family`, else aterm's own
  default face. It never keeps whatever face was loaded before, so switching to a theme gives the
  same result every time. `aterm theme check` notes such a font (`font_not_installed`, a note:
  `--strict` still passes), and the window says so once in the message band.
- **A trial is colours only.** The row highlighted in Settings ▸ Themes, the palette and
  `aterm ctl theme preview` never change the window's face; Settings' live demo draws the face
  Keep would give. Keeping the theme (Keep on the strip, `theme commit`, `theme = ` in
  `aterm.toml`) applies the face, live. **Keep colours only** (Keep ▾) keeps the theme's colours
  and pins your current type settings in `aterm.toml`, so the face does not change. It is not
  offered for a theme that sets `font_weight`: no `aterm.toml` value keeps a font's own weight
  apart from the theme.
- **Editing the file applies live**, face included, and so does a light/dark flip when the two
  sides of `dark:…,light:…` name different type settings.
- **`schema`** says which theme format version a file was written for. A file whose `schema` is
  newer than the aterm reading it still loads: keys that aterm does not know are skipped and
  listed (in `aterm theme list`, `aterm theme check` and Settings), while every key it does know
  stays strict. An aterm older than the one that added fonts refuses a file with any typography
  key, `schema` included; if that matters to you, ship a colours-only file beside it.

### What a theme can never contain

Theme files get downloaded and shared, so aterm treats every one as untrusted text. A theme
holds colours, its own name, description, author and appearance, and type settings by name and
value, and nothing else. It holds no command, shell, key binding or permission; no font file and
no path of any kind (a font name with `/`, `\`, a leading `~` or `.`, a drive letter or a
`.ttf`/`.otf`/`.ttc`/`.otc`/`.dfont`/`.woff`/`.woff2` ending is an error); no `font_px` (size
reaches a theme only as `font_size_scale`); no padding or opacity; and nothing that moves or
makes a sound. Any of those is an unknown key or a bad value, so the file does not load. aterm
also skips symlinks in the folder, refuses a file over 256 KiB, and reads a bounded number of
files (`aterm help themes` has the numbers). `docs/DESIGN-theme-packages.md` §3.3 gives the
reasoning key by key.

## The quality bar

### The floor: what `aterm theme check` measures

`aterm theme check <file>` reads a file exactly as the themes folder will. It prints errors and
warnings by line, then the WCAG contrast of the text, each ANSI colour, the selection band, the
text on the band and the cursor. A theme that loads is admitted even with warnings, but
`--strict` turns every warning into exit 1, and a contribution must pass `--strict`.

| Warning | Bar | Why |
|---|---|---|
| `low_text_contrast` | text on background at least 3:1 | below 3:1 body text is hard to read for most people, in any light |
| `faint_selection` | selection band on background at least 1.3:1 | below that you cannot see what you selected |
| `ansi_matches_background` | no ANSI colour equal to the background | text in that colour is invisible, and `minimum_contrast` is off by default, so nothing rescues it |
| `low_cursor_contrast` | cursor on background at least 3:1 | you have to find the cursor at a glance |
| `appearance_mismatch` | `appearance` matches the measured background (light when black text has more contrast on it than white, e.g. `#767676` and lighter greys) | a "dark" theme with a light background lands on the wrong side of every light/dark pair, and programs are told the wrong thing |

The file name is checked too: `not_conf`, `invalid_id`, `shadowed_by_builtin` and
`symlink_not_followed`.

### The signature bar: what aterm's own themes clear

aterm's signature themes (Aterm Night / Aterm Day and Hearth / Linen) were designed against a
stricter bar. Aim for it, and measure it with **`aterm theme check <file> --signature`**: it
lists every bar the theme misses (in words, and as `signature[]` `{code, message}` rows with
`--format json`), and with `--strict` a miss fails the check. It is the same code that holds the
built-ins to the bar (`aterm_types::scheme_signature::signature_report`), so the hue, chroma and
OkLab rows need no picker of your own. "Accents" below are ANSI 1-6 and their brights 9-14.

| Bar | Why |
|---|---|
| text at 7:1 (WCAG AAA) | you read this colour all day, so it gets the most headroom |
| each accent (ANSI 1-6 and 9-14) at 4.5:1 | prompts, diffs and compiler errors are text, and AA is the floor for text |
| the six hues in each tier at least 25° apart (OKLCH) | red and magenta, or green and cyan, must stay distinct even at a glance or to a colour-weak eye |
| each bright at OkLab distance 0.05 or more from its normal | `ls` and many TUIs use bright as a second colour, not a duplicate |
| `color8` between 3:1 and 4.5:1 | "bright black" is the dim colour (comments, autosuggestions): readable, but quieter than text |
| `color0` at 1.15:1 or more | programs paint panels in color0, and a panel that disappears into the background has lost its edge |
| `color7` and `color15` at 4.5:1 | some programs write text in "white" on a light theme |
| selection band at 1.5:1, with text on it at 4.5:1 | the selection must show, and selected text must still read |
| a cursor at 3:1, OKLCH chroma 0.08 or more, within 15° of one of the accent hues (1-6) | the cursor is the theme's accent, and an accent from inside the palette keeps it calm |
| accent chroma (both tiers, 1-6 and 9-14) 0.07 to 0.17, the highest at most 1.75 times the lowest | no hue shouts over the others, and none fades to grey |

Faint text (SGR 2) is computed by aterm, not by the theme. At the default `faint_opacity`, every
colour at 4.3:1 or more stays at 3:1 or more when faint, and faint is always visibly lighter than
the colour itself, on dark and light themes alike. You need not tune for it, but a colour under
4.3:1 (`color8`, for example) goes faint below 3:1.

## Designing in OKLCH

Pick colours in OKLCH, not in RGB or HSL, because its lightness axis matches what the eye sees.
Two colours at the same OKLCH lightness look equally bright, while two at the same HSL lightness
can differ a lot (compare HSL yellow and blue).

1. **Ground and ink.** Choose the background first, then a foreground on the same hue family at
   7:1. Dark grounds read best a little lifted off black (OKLCH L around 0.18-0.24). Light
   grounds read best slightly off white.
2. **Six accents.** Put red, green, yellow, blue, magenta and cyan at **one** lightness and one
   chroma band, and spread their hues so each pair is at least 25° apart. Then raise or lower
   that shared lightness until the dimmest one clears 4.5:1. On light themes, yellow is the hard
   one: at 4.5:1 on a near-white ground any yellow sits below OKLCH L of about 0.6, so it is
   necessarily a dark ochre (aterm itself classes it as brown). Push its chroma to the gamut
   edge at hue 70-85 so it reads as gold rather than mud, and keep bright yellow within about
   0.08 L of it, or move it toward orange in hue rather than darker.
3. **Brights.** Move every accent the same step in lightness, away from the background, and
   check each against its normal (OkLab distance 0.05 or more).
4. **Greys.** `color0` sits a step off the background, `color8` between 3:1 and 4.5:1, and
   `color7`/`color15` near the foreground.
5. **Selection and cursor.** Blend the background toward the foreground for the band (1.5:1 on
   the background), and take the cursor from one accent hue.
6. Run `aterm theme check --strict --signature` and look at the theme on real output: `git diff`, `ls
   --color`, a compiler error, an editor. A number cannot tell you a theme is pleasant.

## Light/dark pairs

`theme = "dark:Harbour Night,light:Harbour Day"` draws one side by the OS appearance and switches
live when the OS does. Write the pair as two files with the same accents, re-lit for each ground:
keep each hue, and move the lightness so the light side clears the same bars on its own
background. Set `appearance` in both files. If a value names one side only, aterm draws that
side's **built-in** pair on the other, else the named side, so a user theme with no partner
simply stays put. To start the partner, copy the first side
(`aterm theme new "Harbour Day" --from "Harbour Night"`) and re-light it, or start from a light
theme (`--from "Aterm Day"`). With `--appearance light`, a `--from` theme drawn for the other
appearance is swapped for its built-in pair, and refused when it has none.

## The edit loop

| Step | Command |
|---|---|
| start | `aterm theme new <name> [--from <theme>] [--appearance light]`, or `aterm theme export <theme> --out <file>` to copy any theme, built-ins included |
| check | `aterm theme check <file> --strict` (`--format json` for scripts and agents: status, diagnostics with line and key, every contrast row) |
| see it | `aterm ctl theme preview <id> [ttl=<secs>]` draws any theme without touching `aterm.toml` (120 s by default) and stays up while you save, so each save redraws it; `aterm ctl theme revert`, deleting the file or an `aterm.toml` reload ends it |
| compare | Settings ▸ Themes (one chooser list beside a live demo: the highlighted row is tried, Keep or Revert on the strip, Keep ▾ for Keep colours only, Customize, Save as theme…, the Themes folder), or View ▸ Choose Theme… in the command palette |
| keep it | `aterm ctl theme commit`, Keep on Settings ▸ Themes' strip, or `theme = "<id>"` in `aterm.toml` |
| inspect | `aterm ctl theme` names the theme on screen, its source and file, the side a split drew, what programs are told (DSR 996 / mode 2031), why a name misses, the palette, and the typography it applies (`typography=`, `typography_keys=`, and `typography_shadowed=` for keys your config or `--font` beat); per session, `aterm ctl status` shows `reported_scheme=` and `scheme_from=` (a program's own background outranks the theme) |

Colour keys in `aterm.toml` (`foreground`, `background`, `cursor_color`, `selection_color`,
`selection_foreground`, `palette`) and its font keys override the theme. Clear them while you
judge a theme, or you are judging your overrides.

## Themes set by escape sequence

base16-shell, tinted-shell, pywal (`wal -R`) and theme.sh do not write a theme: they print
OSC 4 (palette) and OSC 10/11/12 (foreground, background, cursor) to the tab. aterm draws them
by default as **session colours over your theme**. They last for that tab (a seamless update
keeps them, since the shell is the same), they are never saved, and they never touch the
window chrome. A theme switch keeps them, and every colour they did not set follows the new
theme.

To put your theme back: run `printf '\e]104\a\e]110\a\e]111\a\e]112\a'`, choose
Edit ▸ Reset Terminal, or run `aterm ctl @<sid> reset`. `aterm ctl @<sid> colors` lists what a
program set (`program_palette=`, `program_slots=`). `allow_palette_reconfigure = false` in
`aterm.toml` refuses palette sets and takes them out of every open tab.

To keep a scheme like this, make it a `.conf` theme: `aterm theme import` reads base16/base24
YAML, and the result applies to every tab and survives a restart.

## Importing from another terminal

```sh
aterm theme import ~/Downloads/tokyonight.conf             # kitty
aterm theme import "Some Scheme.itermcolors" --name "Some Scheme"
```

`aterm theme import` reads kitty, Ghostty, Alacritty and WezTerm (TOML), iTerm2
(`.itermcolors`), Terminal.app (`.terminal`), Windows Terminal (JSON) and base16/base24 (YAML).
It works offline, follows no URL or include, refuses files over 256 KiB, and lists every key it
dropped. It writes a native `.conf` only after reading it back through aterm's own parser. An
import keeps colours only: another terminal's font lines are often paths or font patterns, so
they are listed as dropped, never translated (add the typography keys yourself). A foreign file
dropped straight into the folder does not load, and its error names `aterm theme import`. Run
`aterm theme check --strict` on the result: ported palettes often miss the floor, which is why
aterm's built-in ports carry their own fixed selection bands.

## Sharing a theme

Community themes live in [`contrib/themes/`](README.md) in the aterm
source. The rules:

- one `.conf` per theme, named for its id, with `name`, `description`, `author` and `appearance`
  set;
- it passes `aterm theme check --strict` (a test runs the checker over every file there);
- it is your work, or you have the right to share it under the repository's licence.

Someone else's palette is different. A port of another project's theme (a Vim colour scheme, a
VS Code theme) goes in the community pack, [`contrib/themes/pack/`](pack/README.md), not at the
top level, and only when that project carries a permissive licence (MIT, Apache-2.0, BSD, ISC).
There, the collection's file is vendored byte for byte with the project's licence beside it, the
shipped `.conf` is generated from it with the notices in its header, and it must pass `--strict`
unchanged: a palette is never edited to pass. `pack/README.md` walks through it.

`contrib/themes/TEMPLATE.conf` is a complete theme with every key under a comment. For worked
examples of the signature bar, `aterm theme export "<name>"` prints any of aterm's signature
themes (`aterm theme list` names them) as a file. Read it rather than installing it: a file named
after a built-in is shadowed by it.

The same `.conf` text works in a web embed of aterm (the aterm-wasm and aterm-gpu-web modules):
hand it to `register_theme(text)` and select it with `set_theme_by_name("<name>")`, or with a
`dark:<name>,light:<name>` split. There the id is the file's `name =`, since a web page has no
file name, and the same rules hold: built-in names are refused, and an invalid file is refused
with the line at fault.
