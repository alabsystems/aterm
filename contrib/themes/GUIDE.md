<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Writing an aterm theme

This guide is for anyone who writes a colour theme for aterm, person or coding agent. It covers
the file format, the bar a good theme clears and why, light/dark pairs, the edit loop, importing
from other terminals, and how to share a theme. `aterm help themes` is the short reference, and
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
| `appearance` | | `dark` or `light`; measured from the background when left out |
| `foreground` | `fg` | default text colour (required) |
| `background` | `bg` | default background and window padding (required) |
| `cursor` | `cursor_color` | cursor; follows the foreground when left out |
| `selection` | `selection_color` | selection band; derived from the background when left out |
| `selection_foreground` | | text in the selection; selected text keeps its own colour when left out |
| `color0` … `color15` | `palette<N>`, `ansi<N>` | ANSI 0-7 (black, red, green, yellow, blue, magenta, cyan, white), 8-15 their bright forms |

`aterm theme schema` prints this table as JSON, from the same table the parser reads keys by,
so a tool built on it cannot disagree with aterm. The colour keys are spelled the way
`aterm.toml` spells its own overrides, except the palette, which `aterm.toml` writes as
`palette = [ … ]`.

### What a theme can never contain

Theme files get downloaded and shared, so aterm treats every one as untrusted text. A theme
holds colours and its own name, description, author and appearance, and nothing else. It holds
no command, shell, key binding or permission; no path (wallpaper, sprite, include); no font,
size, padding or opacity; and nothing that moves or makes a sound. Any of those is an unknown
key, so the file does not load. aterm also skips symlinks in the folder, refuses a file over
256 KiB, and reads a bounded number of files (`aterm help themes` has the numbers).
`docs/DESIGN-theme-packages.md` §3.3 gives the reasoning key by key (its typography keys are
a proposal; none of them loads today).

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
| `appearance_mismatch` | `appearance` matches the measured background | a "dark" theme with a light background lands on the wrong side of every light/dark pair, and programs are told the wrong thing |

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
| compare | Settings ▸ Themes (the Theme Showcase: gallery, Try with Keep/Revert, Customize, Save as theme…), or View ▸ Choose Theme… in the command palette |
| keep it | `aterm ctl theme commit`, Keep in the showcase, or `theme = "<id>"` in `aterm.toml` |
| inspect | `aterm ctl theme` names the theme on screen, its source and file, the side a split drew, what programs are told (DSR 996 / mode 2031), why a name misses, and the palette |

Colour keys in `aterm.toml` (`foreground`, `background`, `cursor_color`, `selection_color`,
`selection_foreground`, `palette`) override the theme. Clear them while you judge a theme, or
you are judging your overrides.

## Importing from another terminal

```sh
aterm theme import ~/Downloads/tokyonight.conf             # kitty
aterm theme import "Some Scheme.itermcolors" --name "Some Scheme"
```

`aterm theme import` reads kitty, Ghostty, Alacritty (TOML), iTerm2 (`.itermcolors`), Windows
Terminal (JSON) and base16/base24 (YAML). It works offline, follows no URL or include, refuses
files over 256 KiB, and lists every key it dropped. It writes a native `.conf` only after
reading it back through aterm's own parser. A foreign file dropped straight into the folder does
not load, and its error names `aterm theme import`. Run `aterm theme check --strict` on the
result: ported palettes often miss the floor, which is why aterm's built-in ports carry their
own fixed selection bands.

## Sharing a theme

Community themes live in [`contrib/themes/`](README.md) in the aterm
source. The rules:

- one `.conf` per theme, named for its id, with `name`, `description`, `author` and `appearance`
  set;
- it passes `aterm theme check --strict` (a test runs the checker over every file there);
- it is your work, or you have the right to share it under the repository's licence.

`contrib/themes/TEMPLATE.conf` is a complete theme with every key under a comment. For worked
examples of the signature bar, `aterm theme export "<name>"` prints any of aterm's signature
themes (`aterm theme list` names them) as a file. Read it rather than installing it: a file named
after a built-in is shadowed by it.
