// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for native-front control routing.
//!
//! This test drives the pure classifier used by the shipping control dispatcher.
//! The projection calls that classifier independently for every principal/target
//! pair, so the model is bound to the actual zero-terminal routing decisions
//! rather than to a second handwritten truth table.

#![cfg(test)]

use aterm_spec::derive::{Model, native_control_routing_model};
use aterm_spec::interp::{State, admits};

use crate::control::{
    NativeControlDecision, NativeControlPrincipal, NativeControlTarget, native_control_decision,
};
use crate::{App, WindowId};

#[derive(Clone, Copy)]
struct RealRouting {
    front_has_terminal: bool,
    explicit_session_live: bool,
}

impl RealRouting {
    const fn initial() -> Self {
        Self {
            front_has_terminal: true,
            explicit_session_live: true,
        }
    }

    fn allowed(self, principal: NativeControlPrincipal, target: NativeControlTarget) -> i64 {
        i64::from(matches!(
            native_control_decision(
                self.front_has_terminal,
                self.explicit_session_live,
                principal,
                target,
            ),
            NativeControlDecision::WithoutSession | NativeControlDecision::ResolveSession
        ))
    }

    fn project(self, model: &Model) -> State {
        let mut state = model.init_state();
        state.insert("front_kind", if self.front_has_terminal { 1 } else { 2 });
        state.insert("active_terminal", i64::from(self.front_has_terminal));
        state.insert(
            "explicit_session_live",
            i64::from(self.explicit_session_live),
        );
        state.insert(
            "owner_app_allowed",
            self.allowed(NativeControlPrincipal::Owner, NativeControlTarget::App),
        );
        state.insert(
            "owner_meta_allowed",
            self.allowed(NativeControlPrincipal::Owner, NativeControlTarget::Meta),
        );
        state.insert(
            "bare_session_allowed",
            self.allowed(
                NativeControlPrincipal::Owner,
                NativeControlTarget::BareSession,
            ),
        );
        state.insert(
            "explicit_session_allowed",
            self.allowed(
                NativeControlPrincipal::Owner,
                NativeControlTarget::ExplicitSession,
            ),
        );
        state.insert(
            "edge_app_allowed",
            self.allowed(NativeControlPrincipal::Edge, NativeControlTarget::App),
        );
        state.insert(
            "edge_meta_allowed",
            self.allowed(NativeControlPrincipal::Edge, NativeControlTarget::Meta),
        );
        state.insert("hidden_terminal_fallback", 0);
        state.insert("session_without_target", 0);
        state
    }
}

fn assert_transition(model: &Model, before: &State, after: &State, action: &'static str) {
    assert_eq!(
        model.successors(action, before).as_slice(),
        std::slice::from_ref(after),
        "real transition must conform specifically to {action}"
    );
    assert_eq!(admits(model, before, after), Some(action));
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "post-state violates {}::{}: {after:?}",
            model.name,
            invariant.name,
        );
    }
}

fn drive(
    model: &Model,
    real: &mut RealRouting,
    action: &'static str,
    operation: impl FnOnce(&mut RealRouting),
) {
    let before = real.project(model);
    operation(real);
    let after = real.project(model);
    assert_transition(model, &before, &after, action);
}

#[test]
fn shipping_classifier_conforms_across_focus_and_session_lifecycle() {
    let model = native_control_routing_model();
    let mut real = RealRouting::initial();
    assert_eq!(real.project(&model), model.init_state());

    drive(&model, &mut real, "FocusNative", |real| {
        real.front_has_terminal = false;
    });

    // An explicitly named live session remains reachable behind native focus,
    // while an implicit session request gets the typed no-terminal decision.
    assert_eq!(
        native_control_decision(
            false,
            true,
            NativeControlPrincipal::Owner,
            NativeControlTarget::BareSession,
        ),
        NativeControlDecision::NoActiveTerminal,
    );
    assert_eq!(
        native_control_decision(
            false,
            true,
            NativeControlPrincipal::Owner,
            NativeControlTarget::ExplicitSession,
        ),
        NativeControlDecision::ResolveSession,
    );

    drive(&model, &mut real, "RetireExplicitSession", |real| {
        real.explicit_session_live = false;
    });
    assert_eq!(
        native_control_decision(
            false,
            false,
            NativeControlPrincipal::Owner,
            NativeControlTarget::ExplicitSession,
        ),
        NativeControlDecision::NoSuchSession,
    );
    drive(&model, &mut real, "RestoreExplicitSession", |real| {
        real.explicit_session_live = true;
    });
    drive(&model, &mut real, "FocusTerminal", |real| {
        real.front_has_terminal = true;
    });

    // Negative control: retaining a hidden terminal while a native view is
    // focused is neither emitted by the classifier projection nor admitted by
    // the correct model.
    let before_native = real.project(&model);
    let mut hidden_fallback = before_native.clone();
    hidden_fallback.insert("front_kind", 2);
    hidden_fallback.insert("active_terminal", 1);
    hidden_fallback.insert("bare_session_allowed", 1);
    hidden_fallback.insert("hidden_terminal_fallback", 1);
    hidden_fallback.insert("session_without_target", 1);
    assert_eq!(admits(&model, &before_native, &hidden_fallback), None);
    assert!(!model.check_invariant("FrontKindMatchesTerminalMirror", &hidden_fallback));
    assert!(!model.check_invariant("NoHiddenTerminalFallback", &hidden_fallback));
    assert!(!model.check_invariant("NoSessionWithoutTarget", &hidden_fallback));

    // Negative control: an app/meta edge bypass fails both authority invariants.
    let mut edge_bypass = before_native;
    edge_bypass.insert("edge_app_allowed", 1);
    edge_bypass.insert("edge_meta_allowed", 1);
    assert_eq!(admits(&model, &real.project(&model), &edge_bypass), None);
    assert!(!model.check_invariant("EdgeAppDenied", &edge_bypass));
    assert!(!model.check_invariant("EdgeMetaDenied", &edge_bypass));
}

/// Bind the model's focus transition to the real window resolver, not only the
/// pure control classifier. The terminal session deliberately remains live in
/// the pool while Settings is focused; authority must still become absent.
#[test]
fn real_window_focus_projects_exact_optional_terminal_capability() {
    let model = native_control_routing_model();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let project = |app: &App| RealRouting {
        front_has_terminal: app.front_terminal(wid).is_some(),
        explicit_session_live: app.pool.get(0).is_some(),
    };

    let terminal = project(&app).project(&model);
    assert_eq!(terminal, model.init_state());

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let native = project(&app).project(&model);
    assert_transition(&model, &terminal, &native, "FocusNative");
    assert!(app.windows[&wid].active_terminal.is_none());
    assert!(
        app.pool.get(0).is_some(),
        "hidden shell remains explicitly addressable"
    );

    assert!(app.close_settings_tabs());
    let terminal_again = project(&app).project(&model);
    assert_transition(&model, &native, &terminal_again, "FocusTerminal");
}
