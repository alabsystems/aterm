<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# The community theme pack

Well-known terminal themes that aterm carries inside it, so you can add them without going
looking: Catppuccin Frappe and Macchiato, Kanagawa, Tokyo Night, Rose Pine, the GitHub themes,
Iceberg, Nightfox and its family, Melange, Everforest, Ayu, Doom One, Snazzy and more.

```sh
aterm theme pack                      # the pack, and which ones you have
aterm theme install "Kanagawa Wave"   # add one (or several, or --all)
aterm theme pack --licences           # every palette's attribution and licence
```

Settings ▸ Themes ▸ **Community** lists the ones you don't have yet: the highlighted one is tried
in the live demo, and **Keep** adds it and selects it.

A pack theme is not a built-in. Installing one copies its file into your themes folder as an
ordinary `<id>.conf`, where it loads, previews and can be edited like any theme of yours. A copy
you have edited is kept unless you pass `--replace`. Nothing is downloaded: the files are built
into aterm (the `aterm-theme-pack` crate embeds this folder).

## Which themes are here, and why

A palette belongs to its author, not to the collection that gathered it. The
[iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes) collection, which this
pack converts from, is MIT licensed, but its licence says "the copyright/license for each
individual theme belongs to the author of that theme". So a theme ships here only when:

1. **its own project carries a clear permissive licence**, an SPDX id in MIT, Apache-2.0,
   BSD-2-Clause, BSD-3-Clause, ISC, 0BSD, CC0-1.0 or Unlicense, in a licence file in the
   author's repository. Copyleft (GPL, LGPL: Modus, Bluloco), proprietary (Monokai Pro) and
   unlicensed projects (Jellybeans, Zenburn, Tomorrow, gruvbox's own variants) are out;
2. **it passes `aterm theme check --strict` exactly as converted.** No palette is edited to make
   it pass: that would ship someone else's work under their name with our changes in it. On
   2026-10-16, 21 otherwise eligible themes failed (a faint selection, an ANSI colour that
   vanishes into the background, a cursor too faint to find) and are left out.

Each theme file carries its notices as comments: the palette's author, project and licence, and
the collection's MIT licence. [`ATTRIBUTION.md`](ATTRIBUTION.md) lists everything, and also
covers the palettes of aterm's built-in ports (Dracula, Nord, Tokyo Night, Catppuccin Mocha and
Latte, Gruvbox, Solarized, One Dark, GitHub Light).

## Any other theme

Every theme in the collection is one command away. Download its file from the collection's
[`ghostty/`](https://github.com/mbadolato/iTerm2-Color-Schemes/tree/master/ghostty) folder, then:

```sh
aterm theme import "Gruvbox Dark Hard" --name "Gruvbox Dark Hard"
aterm theme check ~/.config/aterm/themes/"Gruvbox Dark Hard.conf"
```

`import` reads only a local file and never fetches a URL. Check the theme's own licence before
you share the result.

## What is in this folder

| Path | What it is |
|---|---|
| `manifest.tsv` | One row per theme (below), plus the collection and the built-in ports' licence files. |
| `upstream/<Name>` | The collection's `ghostty/<Name>` file at the pinned commit, byte for byte. |
| `LICENSES/` | Each project's licence, verbatim (sources below). |
| `<id>.conf` | The shipped theme: generated from its upstream file, never edited by hand. |
| `ATTRIBUTION.md` | Generated: `aterm theme pack --licences` prints the same text. |

`manifest.tsv` is UTF-8, tab-separated, with `#` comment lines. Its one directive comes first:
`@collection <repo> <commit> <licence file>` (where `upstream/` comes from). A built-in port's
source licence needs no row: it is `LICENSES/<spdx>-<repo>.txt`, found from the built-in's own
source, and the build refuses a missing one, or any file in this folder that nothing names.
Then a header row, and one row per theme with nine columns:

| Column | Meaning |
|---|---|
| `id` | The theme's id: its file name in the themes folder, what `theme = "<id>"` names. |
| `upstream` | Its file under `upstream/` (`ghostty/<upstream>` in the collection). |
| `appearance` | `dark` or `light`, declared in the file and checked by `--strict`. |
| `pair` | Its family's theme for the other appearance (a pack id or a built-in), or `-`. |
| `author` | Who the palette is by. |
| `project` | The palette's project, `owner/repo` on GitHub. |
| `project_url` | The project's home page. |
| `licence` | The project's SPDX licence id. |
| `licence_files` | The licence files (`LICENSES/…`, `;`-separated) whose notices the file carries. |

The build refuses a row whose licence is outside the list above, whose id the themes folder could
not hold, or whose files are missing.

## Adding or refreshing a theme

1. Put the collection's `ghostty/<Name>` file in `upstream/`, at the pinned commit. To move to a
   newer commit, change the `@collection` row and refresh every upstream file.
2. Add the project's licence file, verbatim, to `LICENSES/` as `<SPDX>-<repo>.txt`, and its row
   to the sources table below.
3. Add a manifest row.
4. Generate the theme file and ATTRIBUTION.md:
   `ATERM_BLESS_GOLDEN=1 targo --unverified test -p aterm-theme-pack`, then run it again
   without the variable to embed what it wrote.
5. `targo --unverified test -p aterm-theme-pack -p aterm-cli` must pass.
   `pack_themes_pass_the_strict_check` runs the strict checker over every pack theme. A theme it
   rejects is removed from the manifest, never edited. That also holds when the checker grows
   stricter later.

The Everforest file in the collection comes from Nikolai Osaulenko's
[everforest.iterm2](https://github.com/icewind/everforest.iterm2) port of sainnhe's palette, so it
carries both projects' MIT notices.

## Licence sources

Each file in `LICENSES/` is the project's licence file verbatim, fetched on 2026-10-16 with
`gh api repos/<repo>/contents/<path>?ref=<commit> --jq .content | base64 -d`.
`MIT-iTerm2-Color-Schemes.txt` is the collection's `LICENSE` at
`21f281d0834846eef984f75774638e7d16bac068`, its per-theme sentence included.

| Project | Licence | File | Commit | Here |
|---|---|---|---|---|
| [EliverLara/Andromeda](https://github.com/EliverLara/Andromeda) | MIT | `LICENSE.md` | `d1abb48c6949` | `LICENSES/MIT-Andromeda.txt` |
| [ayu-theme/ayu-colors](https://github.com/ayu-theme/ayu-colors) | MIT | `license` | `b0fd979a1ddf` | `LICENSES/MIT-ayu-colors.txt` |
| [catppuccin/catppuccin](https://github.com/catppuccin/catppuccin) | MIT | `LICENSE` | `d09787dd98ca` | `LICENSES/MIT-catppuccin.txt` |
| [EdenEast/nightfox.nvim](https://github.com/EdenEast/nightfox.nvim) | MIT | `LICENSE` | `4dacd3f0185a` | `LICENSES/MIT-nightfox.nvim.txt` |
| [doomemacs/themes](https://github.com/doomemacs/themes) | MIT | `LICENSE` | `a59202912ad5` | `LICENSES/MIT-doomemacs-themes.txt` |
| [sainnhe/everforest](https://github.com/sainnhe/everforest) | MIT | `LICENSE` | `85a86eb62409` | `LICENSES/MIT-everforest.txt` |
| [icewind/everforest.iterm2](https://github.com/icewind/everforest.iterm2) | MIT | `LICENSE` | `95dd64db6a69` | `LICENSES/MIT-everforest.iterm2.txt` |
| [kepano/flexoki](https://github.com/kepano/flexoki) | MIT | `LICENSE` | `8d723bac4a9a` | `LICENSES/MIT-flexoki.txt` |
| [primer/github-vscode-theme](https://github.com/primer/github-vscode-theme) | MIT | `LICENSE` | `cd78e5e4e7bc` | `LICENSES/MIT-github-vscode-theme.txt` |
| [jolaleye/horizon-theme-vscode](https://github.com/jolaleye/horizon-theme-vscode) | MIT | `LICENSE` | `5ae91b6d49bf` | `LICENSES/MIT-horizon-theme-vscode.txt` |
| [cocopon/iceberg.vim](https://github.com/cocopon/iceberg.vim) | MIT | `LICENSE.txt` | `23835d5ed696` | `LICENSES/MIT-iceberg.vim.txt` |
| [rebelot/kanagawa.nvim](https://github.com/rebelot/kanagawa.nvim) | MIT | `LICENSE` | `bb85e4bfc8d8` | `LICENSES/MIT-kanagawa.nvim.txt` |
| [savq/melange-nvim](https://github.com/savq/melange-nvim) | MIT | `LICENSE` | `ce42f6b629be` | `LICENSES/MIT-melange-nvim.txt` |
| [bluz71/vim-moonfly-colors](https://github.com/bluz71/vim-moonfly-colors) | MIT | `LICENSE` | `2c883835f5b7` | `LICENSES/MIT-vim-moonfly-colors.txt` |
| [rose-pine/rose-pine-theme](https://github.com/rose-pine/rose-pine-theme) | MIT | `LICENSE` | `781bb844aae0` | `LICENSES/MIT-rose-pine-theme.txt` |
| [jan-warchol/selenized](https://github.com/jan-warchol/selenized) | MIT | `LICENSE.txt` | `9a753d5c575c` | `LICENSES/MIT-selenized.txt` |
| [sindresorhus/iterm2-snazzy](https://github.com/sindresorhus/iterm2-snazzy) | MIT | `license` | `27b3156992a8` | `LICENSES/MIT-iterm2-snazzy.txt` |
| [sainnhe/sonokai](https://github.com/sainnhe/sonokai) | MIT | `LICENSE` | `b023c5280b16` | `LICENSES/MIT-sonokai.txt` |
| [folke/tokyonight.nvim](https://github.com/folke/tokyonight.nvim) | Apache-2.0 | `LICENSE` | `cdc07ac78467` | `LICENSES/Apache-2.0-tokyonight.nvim.txt` |
| [dracula/dracula-theme](https://github.com/dracula/dracula-theme) | MIT | `LICENSE` | `e22ebcc048b8` | `LICENSES/MIT-dracula-theme.txt` |
| [nordtheme/nord](https://github.com/nordtheme/nord) | MIT | `license` | `1cef71605416` | `LICENSES/MIT-nord.txt` |
| [altercation/solarized](https://github.com/altercation/solarized) | MIT | `LICENSE` | `62f656a02f93` | `LICENSES/MIT-solarized.txt` |
| [atom/one-dark-syntax](https://github.com/atom/one-dark-syntax) | MIT | `LICENSE.md` | `9c96f4454362` | `LICENSES/MIT-one-dark-syntax.txt` |
| [morhetz/gruvbox](https://github.com/morhetz/gruvbox) | MIT (README, "License: MIT/X11"; no licence file) | `README.md` lines 115-119 | `ef8864bb42bf` | `LICENSES/MIT-gruvbox.txt` |

gruvbox (morhetz/gruvbox, the source of the built-in Gruvbox Dark and Light) has no licence file.
Its README states "License: [MIT/X11]", and `LICENSES/MIT-gruvbox.txt` holds that excerpt
verbatim. The pack ships no gruvbox theme, partly for this reason; the built-in ports credit it
under MIT.
