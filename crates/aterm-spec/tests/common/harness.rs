// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The ONE place a property-combinator instance is declared. Both the Tier-0 `ty`
//! suite (`derived_ring_ty.rs`) and the Tier-1 interpreter-BMC suite
//! (`introspection_bmc.rs`) iterate this table, so adding a verified property is a
//! generator instance (≈3 lines in `derive::props`) + ONE row here — zero new test
//! functions. Each row carries its proof CLASS: a `Safety` invariant (ty
//! prove+catch / BMC prove+catch) or a `Liveness` deadlock-freedom check (ty
//! CHECK_DEADLOCK / BMC no-successor wedge), the latter with its work-complete
//! `is_final` predicate.

#![allow(dead_code)] // each test binary uses one driver; the other's helpers warn.

use std::collections::BTreeMap;

use aterm_spec::derive::Model;

pub type State = BTreeMap<&'static str, i64>;

/// How an instance is verified.
pub enum Class {
    /// A safety invariant: ty proves it (Buggy=0) + counterexample (Buggy=1);
    /// the BMC twin enumerates the bounded space.
    Safety,
    /// Liveness / deadlock-freedom: ty with CHECK_DEADLOCK + the BMC no-successor
    /// wedge check. `is_final` marks the legitimate work-complete terminal so it is
    /// not mistaken for a wedge.
    Liveness { is_final: fn(&State) -> bool },
}

/// A verified property instance: the derived model + its proof class.
pub struct Instance {
    pub model: Model,
    pub class: Class,
}

/// THE TABLE — the introspection control-plane property suite. A new property:
/// add its `derive::props` generator instance + one row here.
pub fn instances() -> Vec<Instance> {
    use aterm_spec::derive::{
        authorize_soundness_model, capability_secrecy_model, dispatch_complete_model,
        forward_handshake_model, no_transitive_authority_model, proxy_registry_model,
        publish_ordering_model, relay_teardown_model, reply_fidelity_model,
    };
    vec![
        Instance {
            model: dispatch_complete_model(),
            class: Class::Safety,
        },
        Instance {
            model: relay_teardown_model(),
            class: Class::Safety,
        },
        Instance {
            model: proxy_registry_model(),
            class: Class::Safety,
        },
        Instance {
            model: capability_secrecy_model(),
            class: Class::Safety,
        },
        Instance {
            model: publish_ordering_model(),
            class: Class::Safety,
        },
        Instance {
            model: reply_fidelity_model(),
            class: Class::Safety,
        },
        // Capability-layer audit: authorization soundness (the decide_edge predicate).
        Instance {
            model: authorize_soundness_model(),
            class: Class::Safety,
        },
        // Deep-nesting safety: forwarding needs Owner scope (no transitive authority).
        Instance {
            model: no_transitive_authority_model(),
            class: Class::Safety,
        },
        Instance {
            model: forward_handshake_model(),
            class: Class::Liveness {
                is_final: |s| s.get("client_waiting") == Some(&0),
            },
        },
    ]
}
