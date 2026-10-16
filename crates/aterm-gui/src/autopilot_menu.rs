// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AUTOPILOT MENU, set from the chip
//! (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.3): Cruise, Land now,
//! Off, the timed Cruises, the cancel and `⚙ Harness settings…`, as cells
//! over the pane — a popup, never a chrome row — opened by a right-click on
//! the Autopilot chip, or `↓`, shift+F10 or the Menu key while the chip is
//! selected.
//!
//! THE SEAM (design §9.6.0): there is no menu yet. Its model, geometry,
//! paint, keys and its accessibility node are slice B's (§9.6.2); until it
//! lands a right-click on the chip is swallowed as it is today, and the
//! chip's title does not offer it ([`OPENS`]).

/// Whether a right-click on the Autopilot chip opens the menu: the chip's
/// whole title ends ` · right-click: more` only where it does
/// (`claude_lights::autopilot_title`). No menu yet.
pub(crate) const OPENS: bool = false;
