// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The one table of property-combinator instances. The umbrella in `derived_ring_ty.rs`
//! discharges every row on both tiers (the interpreter BMC always, `ty` where installed), so a new
//! verified property is a generator instance in `derive::props` plus one row here. Each row carries
//! its proof class: a `Safety` invariant (prove and catch) or a `Liveness` deadlock-freedom check
//! with its work-complete `is_final` predicate.

use std::collections::BTreeMap;

use aterm_spec::derive::Model;

pub type State = BTreeMap<&'static str, i64>;

/// How an instance is verified.
pub enum Class {
    /// A safety invariant: `ty` proves it at `Buggy = 0` and finds its counterexample at
    /// `Buggy = 1`; the BMC twin enumerates the bounded space.
    Safety,
    /// Deadlock freedom: `ty` with `CHECK_DEADLOCK` and the BMC no-successor check. `is_final`
    /// marks the legitimate work-complete terminal, which is not a wedge.
    Liveness { is_final: fn(&State) -> bool },
}

/// A verified property instance: the derived model + its proof class.
pub struct Instance {
    pub model: Model,
    pub class: Class,
}

/// The introspection control-plane property suite. A property no generator states is a
/// hand-written model, as reply fidelity is.
pub fn instances() -> Vec<Instance> {
    use aterm_spec::derive::{
        authorize_soundness_model, capability_secrecy_model, dispatch_complete_model,
        forward_handshake_model, no_transitive_authority_model, proxy_registry_model,
        publish_ordering_model, relay_teardown_model, reply_fidelity_model,
        tls_buffered_relay_model,
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
        Instance {
            model: tls_buffered_relay_model(),
            class: Class::Liveness {
                is_final: |s| s.get("service_waiting") == Some(&0),
            },
        },
    ]
}
