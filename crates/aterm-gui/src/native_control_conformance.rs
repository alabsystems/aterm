// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for native-front control routing.

#![cfg(test)]

use aterm_models_native::native_control_routing_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits};

use crate::conformance_support::assert_sole_transition as assert_transition;
use crate::control::{
    NativeControlDecision, NativeControlPrincipal, NativeControlTarget, Scope,
    native_control_decision, session_route_decision,
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
        state.insert("hidden_terminal_fallback", 0);
        state.insert("session_without_target", 0);
        state
    }

    fn project_with_swapped_inputs(self, model: &Model) -> State {
        let swapped = Self {
            front_has_terminal: self.explicit_session_live,
            explicit_session_live: self.front_has_terminal,
        };
        let mut state = self.project(model);
        state.insert(
            "bare_session_allowed",
            swapped.allowed(
                NativeControlPrincipal::Owner,
                NativeControlTarget::BareSession,
            ),
        );
        state.insert(
            "explicit_session_allowed",
            swapped.allowed(
                NativeControlPrincipal::Owner,
                NativeControlTarget::ExplicitSession,
            ),
        );
        state
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
    let before_restore = real.project(&model);
    drive(&model, &mut real, "RestoreExplicitSession", |real| {
        real.explicit_session_live = true;
    });

    let swapped = real.project_with_swapped_inputs(&model);
    assert_eq!(
        (
            swapped["bare_session_allowed"],
            swapped["explicit_session_allowed"]
        ),
        (1, 0)
    );
    assert_eq!(admits(&model, &before_restore, &swapped), None);
    assert!(!model.check_invariant("BareSessionIffFrontTerminal", &swapped));
    assert!(!model.check_invariant("ExplicitSessionIffLive", &swapped));
    drive(&model, &mut real, "FocusTerminal", |real| {
        real.front_has_terminal = true;
    });

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

    let mut edge_bypass = before_native;
    edge_bypass.insert("edge_app_allowed", 1);
    assert_eq!(admits(&model, &real.project(&model), &edge_bypass), None);
    assert!(!model.check_invariant("EdgeAppDenied", &edge_bypass));
}

#[test]
fn real_window_focus_projects_exact_optional_terminal_capability() {
    let model = native_control_routing_model();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let route = |app: &App, line: &str| {
        session_route_decision(line, &app.active_handle, &app.store, Scope::Owner)
    };
    let project = |app: &App| {
        let mut state = RealRouting {
            front_has_terminal: app.front_terminal(wid).is_some(),
            explicit_session_live: app.pool.get(0).is_some(),
        }
        .project(&model);
        let allowed = |line| i64::from(route(app, line) == NativeControlDecision::ResolveSession);
        state.insert("bare_session_allowed", allowed("text"));
        state.insert("explicit_session_allowed", allowed("@0 text"));
        state
    };

    let terminal = project(&app);
    assert_eq!(terminal, model.init_state());
    assert_eq!(route(&app, "text"), NativeControlDecision::ResolveSession);
    assert_eq!(
        route(&app, "@99 text"),
        NativeControlDecision::NoSuchSession
    );

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let native = project(&app);
    assert_transition(&model, &terminal, &native, "FocusNative");
    assert!(app.windows[&wid].active_terminal.is_none());
    assert!(
        app.pool.get(0).is_some(),
        "hidden shell remains explicitly addressable"
    );
    assert_eq!(route(&app, "text"), NativeControlDecision::NoActiveTerminal);
    assert_eq!(
        route(&app, "@0 text"),
        NativeControlDecision::ResolveSession
    );

    assert!(app.close_settings_tabs());
    let terminal_again = project(&app);
    assert_transition(&model, &native, &terminal_again, "FocusTerminal");
}
