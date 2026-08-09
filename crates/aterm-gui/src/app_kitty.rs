// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PER-APP CURSOR KITTIES (owner spec, 2026-08-07): each major app gets its
//! own cursor kitty — the claude cat, the codex cat, and a cat for every
//! command. Same app ⇒ same colour/breed every time; unknown apps look
//! auto-generated deterministically from the app NAME.
//!
//! IDENTITY SOURCE: the focused pane's shell block. While a command is
//! `Executing`, its OSC 633;E commandline names the app (first meaningful
//! token, basename'd, canonicalized through
//! [`aterm_effects::kitty_registry::canonical_app_id`]); at the prompt
//! (`PromptOnly`/`EnteringCommand`/`Complete`) the user is talking to the
//! SHELL, which is itself an app — the one "shell" kitty regardless of which
//! shell binary renders the prompt. A pane with no shell integration (no
//! block at all) resolves nothing and keeps its session kitty.
//!
//! THE PRECEDENCE LAW lives in [`companion_precedence`]; the per-pane cache
//! lives in [`AppKittySlot`] on `SessionCtx`.

use aterm_core::terminal::{BlockState, OutputBlock};
use aterm_effects::kitty_registry::{KittyLook, app_basename, canonical_app_id};

/// The shell-integration loader's load-once guard variable — the env var the
/// shipped zsh/bash scripts check (`[[ -n … ]] && return`) and then `export`.
///
/// Named HERE because this guard is the app kitty's lifeline: the identity
/// source is the pane's OSC 133/633 shell block, and a NESTED aterm (spawned
/// from a shell that itself runs inside aterm) inherits the parent shell's
/// exported guard — the child loader then bails, no marks are ever emitted,
/// `derive_identity` never sees a block, and the per-app breeds sit dead on
/// the session kitty (gauntlet F3, 0.19.0). The spawn env assembly in
/// `lib.rs` overrides the inherited value with an EMPTY string for every
/// integrated session; a test below pins this name against the shipped
/// scripts so the two can never drift apart.
pub(crate) const SHELL_INTEGRATION_LOADED_GUARD: &str = "ATERM_SHELL_INTEGRATION_INSTALLED";

/// The resolved app identity of one pane: canonical id, the raw basename it
/// came from (diagnostics — `id` may canonicalize it), and the breed the id
/// resolves to. The look is a pure function of `id`, carried here so the
/// render rung never re-hashes per frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppIdentity {
    /// Canonical app id (`"claude"`, `"shell"`, or the raw basename).
    pub id: String,
    /// The first-token basename the id was derived from.
    pub basename: String,
    /// The resolved breed for `id`.
    pub look: KittyLook,
}

/// Per-session cache for the pane's app identity, keyed by
/// `(block id, block state, commandline present)` so a commandline is parsed
/// only on shell-block TRANSITIONS — never per frame. The third key component
/// covers a late OSC 633;E that lands after 133;C already flipped the block
/// to `Executing`.
#[derive(Default)]
pub struct AppKittySlot {
    key: Option<(u64, BlockState, bool)>,
    identity: Option<AppIdentity>,
}

impl AppKittySlot {
    /// Resolve the pane's app identity from its current shell block,
    /// re-deriving only when the `(block, state, commandline)` key moves.
    pub fn resolve(&mut self, block: Option<&OutputBlock>) -> Option<&AppIdentity> {
        let key = block.map(|b| (b.id, b.state, b.commandline.is_some()));
        if key != self.key {
            self.key = key;
            self.identity = block.and_then(derive_identity);
        }
        self.identity.as_ref()
    }
}

/// Derive the app identity for one shell block. `None` means "no claim" —
/// the caller falls through to the next precedence rung (the session kitty).
fn derive_identity(block: &OutputBlock) -> Option<AppIdentity> {
    match block.state {
        // While a command RUNS the pane belongs to that app. No parseable
        // commandline (shell integration without OSC 633;E, or a bare Enter)
        // means no claim: pretending it's the shell would be a lie, and the
        // session kitty is the established face of "no information".
        BlockState::Executing => {
            let basename = app_basename(block.commandline.as_deref().unwrap_or(""))?;
            let id = canonical_app_id(&basename).to_owned();
            let look = KittyLook::for_app(&id);
            Some(AppIdentity { id, basename, look })
        }
        // At the prompt — before, while, and after typing a command — the
        // user is talking to the SHELL. That is the app.
        BlockState::PromptOnly | BlockState::EnteringCommand | BlockState::Complete => {
            Some(shell_identity())
        }
        // `BlockState` is non_exhaustive: an unknown future state claims
        // nothing and the session kitty carries the pane.
        _ => None,
    }
}

/// The one shell identity every prompt resolves to (the canonical table folds
/// zsh/bash/fish/… onto the single "shell" app).
fn shell_identity() -> AppIdentity {
    AppIdentity {
        id: "shell".to_owned(),
        basename: "shell".to_owned(),
        look: KittyLook::for_app("shell"),
    }
}

/// THE COMPANION PRECEDENCE LAW (owner spec, 2026-08-07), the ONE place the
/// order is stated — both render rungs (single-pane and split/compose) call
/// through here:
///
///   favourite > app > discovery > session
///
///   1. A PINNED FAVOURITE owns the companion look (standing owner law): the
///      user chose that cat, and only a reason outranks a choice.
///   2. THE APP KITTY: the focused pane's resolved [`AppIdentity`] — while
///      `claude` runs, the claude cat rides the cursor; at the prompt, the
///      shell's own cat.
///   3. A DISCOVERY companion (ambient/typed collection, no pin): earned, but
///      not chosen — the live app identity is the stronger reason.
///   4. THE SESSION KITTY: the session's own deterministic breed, the face of
///      "no stronger claim".
#[must_use]
pub fn companion_precedence(
    favourite: Option<KittyLook>,
    app: Option<KittyLook>,
    discovery: Option<KittyLook>,
    session: u64,
) -> KittyLook {
    favourite
        .or(app)
        .or(discovery)
        .unwrap_or_else(|| KittyLook::for_session(session))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(state: BlockState, commandline: Option<&str>) -> OutputBlock {
        let mut b = OutputBlock::new(7, 0, 0);
        b.state = state;
        b.commandline = commandline.map(Box::from);
        b
    }

    /// (e) Shell-owned block states resolve to the ONE "shell" id — including
    /// `EnteringCommand`, where a half-typed `claude` is still just prompt
    /// text — and `Executing` hands the pane to the named app.
    #[test]
    fn shell_states_resolve_to_shell_and_executing_to_the_app() {
        let mut slot = AppKittySlot::default();
        let ident = slot
            .resolve(Some(&block(BlockState::PromptOnly, None)))
            .expect("a prompt is the shell");
        assert_eq!(ident.id, "shell");
        assert_eq!(ident.look, KittyLook::for_app("shell"));

        for state in [BlockState::EnteringCommand, BlockState::Complete] {
            let ident = slot
                .resolve(Some(&block(state, Some("claude --resume"))))
                .expect("prompt-side states are the shell");
            assert_eq!(ident.id, "shell", "{state:?} still belongs to the shell");
        }

        let ident = slot
            .resolve(Some(&block(
                BlockState::Executing,
                Some("/usr/local/bin/claude --resume"),
            )))
            .expect("an executing commandline names the app");
        assert_eq!(ident.id, "claude");
        assert_eq!(ident.basename, "claude");
        assert_eq!(ident.look, KittyLook::for_app("claude"));
    }

    /// Executing with no commandline claims nothing (the session kitty rides),
    /// and no block at all claims nothing.
    #[test]
    fn no_commandline_and_no_block_claim_nothing() {
        let mut slot = AppKittySlot::default();
        assert!(slot.resolve(Some(&block(BlockState::Executing, None))).is_none());
        assert!(slot.resolve(None).is_none());
    }

    /// The slot re-derives only on key transitions: the same (block, state,
    /// commandline-present) shape keeps the cached identity, a state flip
    /// re-resolves, and a late OSC 633;E (commandline arriving mid-Executing)
    /// is its own transition.
    #[test]
    fn slot_caches_by_block_state_and_commandline_presence() {
        let mut slot = AppKittySlot::default();
        let executing = block(BlockState::Executing, None);
        assert!(slot.resolve(Some(&executing)).is_none());
        assert!(slot.resolve(Some(&executing)).is_none(), "stable across frames");

        let late_e = block(BlockState::Executing, Some("codex exec"));
        let ident = slot.resolve(Some(&late_e)).expect("late 633;E re-resolves");
        assert_eq!(ident.id, "codex");

        let done = block(BlockState::Complete, Some("codex exec"));
        assert_eq!(
            slot.resolve(Some(&done)).expect("complete → shell").id,
            "shell",
            "the state flip is a transition back to the shell"
        );
    }

    /// GAUNTLET F3 INTEGRATION PIN — the SEAM, not the parts. The unit tests
    /// above all passed while the feature was dead on real probes, because
    /// nothing drove a real `Terminal` through the real resolver and the real
    /// dressing surface. This does: real OSC 133/633 bytes build the shell
    /// block, `App::app_kitty_look`/`App::companion_verdict` (the live render-
    /// path helpers) resolve it, and the capture splice — the only dresser a
    /// headless instance has — must wear the same verdict.
    #[test]
    fn real_osc_blocks_dress_the_companion_through_the_live_seams() {
        let now = std::time::Instant::now();
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.recompute_sparkle();

        let term = app.pool.get(0).expect("session 0").term.clone();
        {
            let mut t = crate::term_lock(&term);
            // The REAL shell-integration byte stream (the blocks_api fixture
            // recipe): prompt start → prompt text → command entered → the
            // explicit OSC 633;E commandline → CRLF → 133;C flips the block
            // to Executing. This is exactly what the zsh integration script
            // emits around `sleep 6`.
            t.process(b"\x1b]133;A\x07");
            t.process(b"user@host repo % ");
            t.process(b"\x1b]133;B\x07");
            t.process(b"sleep 6");
            t.process(b"\x1b]633;E;sleep 6\x07");
            t.process(b"\r\n\x1b]133;C\x07");
        }

        // The render-path resolver names the app from the REAL Executing block…
        let sleep_look = KittyLook::for_app("sleep");
        assert_ne!(
            sleep_look,
            KittyLook::for_session(0),
            "fixture must distinguish the app breed from the session floor, \
             or the pin proves nothing"
        );
        assert_eq!(
            app.app_kitty_look(0),
            Some(sleep_look),
            "an Executing block with an OSC 633;E commandline names the app"
        );
        // …the one companion verdict carries it (hermetic log: no favourite,
        // no discovery — the app rung is the strongest claim)…
        assert_eq!(app.companion_verdict(0), sleep_look);
        // …and the capture splice DRESSES the companion with it.
        app.splice_word_decorations(wid, now);
        let dressed = app
            .windows
            .get_mut(&wid)
            .expect("window")
            .cursor_cat
            .static_frame(now)
            .look;
        assert_eq!(
            dressed, sleep_look,
            "the capture seam wears the app breed while the command runs"
        );

        // BREED HANDOFF: the command completes and the next prompt opens —
        // the pane belongs to the SHELL again, wearing the flagship charcoal
        // tuxedo (the default head, coat 1, moss iris 5), never hash luck.
        {
            let mut t = crate::term_lock(&term);
            t.process(b"\x1b]133;D;0\x07");
            t.process(b"\x1b]133;A\x07user@host repo % \x1b]133;B\x07");
        }
        let shell_look = KittyLook::for_app("shell");
        assert_eq!(
            (
                shell_look.variant,
                shell_look.coat,
                shell_look.iris,
                shell_look.age
            ),
            (
                KittyLook::default().variant,
                1,
                5,
                aterm_effects::genome::CatAge::Adult
            ),
            "the shell flagship tuple is pinned (S103 baseline head, charcoal 1, moss 5)"
        );
        assert_eq!(
            app.companion_verdict(0),
            shell_look,
            "back at the prompt the verdict hands the pane to the shell"
        );
        app.splice_word_decorations(wid, now);
        let dressed = app
            .windows
            .get_mut(&wid)
            .expect("window")
            .cursor_cat
            .static_frame(now)
            .look;
        assert_eq!(
            dressed, shell_look,
            "the capture seam wears the shell tuxedo at the prompt"
        );
    }

    /// The NESTED-LAUNCH lifeline (gauntlet F3 root cause): the spawn env
    /// overrides the inherited [`SHELL_INTEGRATION_LOADED_GUARD`] with an
    /// EMPTY value, which only defuses the loader guard if the shipped
    /// scripts (a) use exactly this variable name and (b) test it with a
    /// non-empty check (`[[ -n … ]]`). Pin both so the script and the spawn
    /// scrub can never drift apart silently.
    #[test]
    fn nested_launch_guard_name_matches_the_shipped_scripts() {
        use aterm_core::shell_integration::scripts;
        let guard_test = format!("[[ -n \"${SHELL_INTEGRATION_LOADED_GUARD}\" ]]");
        for (shell, script) in [("zsh", scripts::ZSH), ("bash", scripts::BASH)] {
            assert!(
                script.contains(SHELL_INTEGRATION_LOADED_GUARD),
                "{shell}: the loader guard variable was renamed — update \
                 SHELL_INTEGRATION_LOADED_GUARD and the lib.rs spawn scrub"
            );
            assert!(
                script.contains(&guard_test),
                "{shell}: the loader guard is no longer a `[[ -n … ]]` check, \
                 so an empty-string override would not defuse it — rework the \
                 nested-launch scrub"
            );
        }
    }

    /// (d) The precedence law, rung by rung: favourite beats app kitty, app
    /// kitty beats discovery companion, discovery beats the session kitty,
    /// and the session kitty is the floor.
    #[test]
    fn precedence_favourite_beats_app_beats_discovery_beats_session() {
        let favourite = KittyLook {
            coat: 2,
            ..KittyLook::default()
        }
        .normalized();
        let app = KittyLook::for_app("claude");
        let discovery = KittyLook {
            coat: 11,
            ..KittyLook::default()
        }
        .normalized();
        let session = 42;

        assert_eq!(
            companion_precedence(Some(favourite), Some(app), Some(discovery), session),
            favourite,
            "a pinned favourite owns the companion look"
        );
        assert_eq!(
            companion_precedence(None, Some(app), Some(discovery), session),
            app,
            "the app kitty outranks a mere discovery"
        );
        assert_eq!(
            companion_precedence(None, None, Some(discovery), session),
            discovery,
            "an earned discovery still beats the default"
        );
        assert_eq!(
            companion_precedence(None, None, None, session),
            KittyLook::for_session(session),
            "the session kitty is the floor"
        );
    }
}
