<!-- SPDX-License-Identifier: Apache-2.0 -->
# Changelog

Test fixture for the /releases renderer (crates/aterm-release/src/site/notes.rs):
the shape of aterm's CHANGELOG.md,
with every Markdown construct the /releases renderer must handle, and one
(`*emphasis*`, raw `<b>`, a bare URL) it must NOT interpret.

## [Unreleased]

- This entry must never appear on /releases.

## [0.64.0] - 2026-08-29

### Added

- **Lean DMG returns.** `aterm.dmg` is the app alone again (~30 MB); the
  seeded image moved to `aterm-0.64.0-seeded.dmg`. See
  [the install notes](https://github.com/alabsystems/aterm#install) & the
  README.
  - nested: the updater picks it by build number
  - nested: `aterm-ctl version` shows the provenance line
- A second item with a <b>raw tag</b> and *asterisk emphasis* that stay literal,
  plus a bare URL https://example.invalid/path that is not linked.
- **`cursor_trail_style = "rainbow kitty"` now draws the KITTY** — bold that
  wraps a code span, the shape the real notes use; and `**not bold** in code`.

### Fixed

1. Numbered fix one.
2. Numbered fix two, with `code`.

> A quoted note about the release.

```
$ aterm --version
aterm 0.64.0
```

---

Trailing paragraph after a rule.

## [0.63.0] - 2026-08-27

### Fixed

- **One kitty at a time.** Sometimes a second cat was drawn on top of the
  cursor companion.

## [0.62.0] - 2026-08-26

## [0.61.0] - 2026-08-26

### Changed

- Sixty-one.

## [0.60.0] - 2026-08-26

- Sixty.

## [0.59.0] - 2026-08-26

- Fifty-nine.

## [0.58.0] - 2026-08-26

- Fifty-eight.

## [0.57.0] - 2026-08-26

- Fifty-seven.

## [0.56.0] - 2026-08-26

- Fifty-six.

## [0.55.0] - 2026-08-25

- Fifty-five.

## [0.54.0] - 2026-08-25

- Fifty-four.

## [0.53.0] - 2026-08-25

- Fifty-three.

## [0.52.0] - 2026-08-24

- Fifty-two: the thirteenth section, which the 12-section limit drops.

## [0.51.0] - 2026-08-24

- Fifty-one: also dropped.
