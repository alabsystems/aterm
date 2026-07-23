// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Integration-level unit tests for the `aterm-provenance` crate. Exercises
//! every public surface: origin tags, `Provenance<T, O>` wrapper,
//! `DynProvenance<T>`, the 6x6 lattice join, runtime/type-level agreement,
//! dominance derivation, `authorize_*` ceremonies, and the `Top` sentinel.

#![allow(clippy::unwrap_used)] // tests
#![allow(clippy::expect_used)] // tests

use aterm_provenance::{
    Ai, ConfigFile, DynProvenance, Host, JoinWith, NetworkUntrusted, Origin, OriginTag,
    OriginsCompatible, Provenance, Pty, SelfSameOrigin, Subsystem, TOP_TAG_U8, Top, UnliftableTop,
    User, dominates, drop_if_top_ffi, drop_if_top_grid, drop_if_top_memory,
    drop_if_top_notification, drop_if_top_predictor, drop_if_top_voice, drop_on_top_count, join,
    record_drop_on_top, runtime_join,
};
// The `authorize_*` ceremonies and their capability tokens are only reachable
// from outside `aterm-provenance` when the `internal-mint` feature is enabled
// (#8013). The corresponding tests below guard on the same feature.
#[cfg(feature = "internal-mint")]
use aterm_provenance::{
    HostAuthorizationToken, NetworkAuthorizationToken, authorize_network_to_host,
    authorize_pty_to_host, try_authorize_network_to_host_dyn, try_authorize_pty_to_host_dyn,
};

// -- OriginTag ----------------------------------------------------------

#[test]
fn origin_tag_discriminants_are_stable() {
    assert_eq!(OriginTag::Host.as_u8(), 0);
    assert_eq!(OriginTag::ConfigFile.as_u8(), 1);
    assert_eq!(OriginTag::User.as_u8(), 2);
    assert_eq!(OriginTag::Ai.as_u8(), 3);
    assert_eq!(OriginTag::NetworkUntrusted.as_u8(), 4);
    assert_eq!(OriginTag::Pty.as_u8(), 5);
}

#[test]
fn origin_tag_all_has_six_unique_values() {
    let tags = OriginTag::all();
    assert_eq!(tags.len(), 6);
    for (i, &a) in tags.iter().enumerate() {
        for &b in &tags[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

#[test]
fn origin_trait_tags_match_markers() {
    assert_eq!(<Host as Origin>::TAG, OriginTag::Host);
    assert_eq!(<ConfigFile as Origin>::TAG, OriginTag::ConfigFile);
    assert_eq!(<User as Origin>::TAG, OriginTag::User);
    assert_eq!(<Ai as Origin>::TAG, OriginTag::Ai);
    assert_eq!(
        <NetworkUntrusted as Origin>::TAG,
        OriginTag::NetworkUntrusted
    );
    assert_eq!(<Pty as Origin>::TAG, OriginTag::Pty);
}

// -- Provenance wrapper -------------------------------------------------

#[test]
fn provenance_is_repr_transparent() {
    // Acceptance criterion from #8000: `size_of::<Provenance<u8, Pty>>() == 1`.
    assert_eq!(core::mem::size_of::<Provenance<u8, Pty>>(), 1);
    assert_eq!(core::mem::size_of::<Provenance<u8, Host>>(), 1);
    assert_eq!(core::mem::size_of::<Provenance<u32, NetworkUntrusted>>(), 4);
}

#[test]
fn provenance_slice_is_repr_transparent() {
    // Acceptance criterion from #8000:
    // `size_of::<Provenance<&[u8], Host>>() == size_of::<&[u8]>()`.
    assert_eq!(
        core::mem::size_of::<Provenance<&[u8], Host>>(),
        core::mem::size_of::<&[u8]>()
    );
    assert_eq!(
        core::mem::size_of::<Provenance<&str, User>>(),
        core::mem::size_of::<&str>()
    );
}

#[test]
fn provenance_constructors_set_correct_tag() {
    assert_eq!(Provenance::<_, Host>::from_host(7u8).tag(), OriginTag::Host);
    assert_eq!(
        Provenance::<_, ConfigFile>::from_config(7u8).tag(),
        OriginTag::ConfigFile
    );
    assert_eq!(Provenance::<_, User>::from_user(7u8).tag(), OriginTag::User);
    assert_eq!(Provenance::<_, Ai>::from_ai(7u8).tag(), OriginTag::Ai);
    assert_eq!(
        Provenance::<_, NetworkUntrusted>::from_network_untrusted(7u8).tag(),
        OriginTag::NetworkUntrusted
    );
    assert_eq!(Provenance::<_, Pty>::from_pty(7u8).tag(), OriginTag::Pty);
}

#[test]
fn provenance_as_ref_preserves_value() {
    let p = Provenance::<_, User>::from_user(String::from("hello"));
    assert_eq!(p.as_ref(), "hello");
}

#[test]
fn provenance_map_preserves_origin() {
    let p = Provenance::<_, Pty>::from_pty(vec![1u8, 2, 3]);
    let p2: Provenance<usize, Pty> = p.map(|v| v.len());
    assert_eq!(*p2.as_ref(), 3);
    assert_eq!(p2.tag(), OriginTag::Pty);
}

#[test]
fn provenance_into_dyn_round_trips() {
    let p = Provenance::<_, Ai>::from_ai(42u32);
    let d: DynProvenance<u32> = p.into_dyn();
    assert_eq!(d.tag(), OriginTag::Ai);
    // Type-refinement success.
    let back: Provenance<u32, Ai> = d.try_as::<Ai>().expect("tag matches Ai");
    assert_eq!(*back.as_ref(), 42);
}

#[test]
fn provenance_from_t_only_for_host() {
    // `From<T>` is implemented only for the Host origin (§4.1 audit
    // ergonomics — every non-Host tag must be opted into explicitly).
    let _: Provenance<u8, Host> = 5u8.into();
    // Compile-fail coverage for other origins lives in tests/compile_fail/ui.
}

// -- DynProvenance ------------------------------------------------------

#[test]
fn dyn_provenance_try_as_matches_tag() {
    let d = DynProvenance::new(1u8, OriginTag::Pty);
    let lift: Provenance<u8, Pty> = d.try_as::<Pty>().expect("Pty matches");
    assert_eq!(*lift.as_ref(), 1);
}

#[test]
fn dyn_provenance_try_as_rejects_mismatch() {
    let d = DynProvenance::new(1u8, OriginTag::Pty);
    let err = d.try_as::<Host>().expect_err("Pty should not lift to Host");
    assert_eq!(err.tag(), OriginTag::Pty);
}

// -- Lattice: 6x6 exhaustive join ---------------------------------------

/// Returns the expected join value from the §3.1 table. Duplicating the
/// table inside the test is deliberate — the test is an independent
/// witness that build.rs is emitting the correct table.
#[rustfmt::skip]
fn expected_join(a: OriginTag, b: OriginTag) -> OriginTag {
    use OriginTag::{Ai, ConfigFile, Host, NetworkUntrusted, Pty, User};
    const T: [[OriginTag; 6]; 6] = [
        //            Host        Config      User        Ai          NetU        Pty
        /* Host    */ [Host,       ConfigFile, User,       Ai,         NetworkUntrusted, Pty],
        /* Config  */ [ConfigFile, ConfigFile, User,       Ai,         NetworkUntrusted, Pty],
        /* User    */ [User,       User,       User,       Ai,         NetworkUntrusted, Pty],
        /* Ai      */ [Ai,         Ai,         Ai,         Ai,         NetworkUntrusted, Pty],
        /* NetU    */ [NetworkUntrusted, NetworkUntrusted, NetworkUntrusted, NetworkUntrusted, NetworkUntrusted, Pty],
        /* Pty     */ [Pty,        Pty,        Pty,        Pty,        Pty,        Pty],
    ];
    T[a as usize][b as usize]
}

#[test]
fn runtime_join_matches_design_table_all_36_cells() {
    for &a in &OriginTag::all() {
        for &b in &OriginTag::all() {
            assert_eq!(
                runtime_join(a, b),
                expected_join(a, b),
                "join({:?}, {:?}) table drift",
                a,
                b,
            );
        }
    }
}

#[test]
fn runtime_join_is_commutative() {
    for &a in &OriginTag::all() {
        for &b in &OriginTag::all() {
            assert_eq!(
                runtime_join(a, b),
                runtime_join(b, a),
                "join is not commutative at ({:?}, {:?})",
                a,
                b,
            );
        }
    }
}

#[test]
fn runtime_join_is_idempotent() {
    for &a in &OriginTag::all() {
        assert_eq!(runtime_join(a, a), a, "join not idempotent at {:?}", a);
    }
}

#[test]
fn runtime_join_host_is_identity() {
    for &a in &OriginTag::all() {
        assert_eq!(runtime_join(OriginTag::Host, a), a);
        assert_eq!(runtime_join(a, OriginTag::Host), a);
    }
}

#[test]
fn runtime_join_pty_is_absorbing() {
    for &a in &OriginTag::all() {
        assert_eq!(runtime_join(OriginTag::Pty, a), OriginTag::Pty);
        assert_eq!(runtime_join(a, OriginTag::Pty), OriginTag::Pty);
    }
}

#[test]
fn runtime_join_is_associative() {
    // Every triple (a, b, c). 6^3 = 216 checks.
    for &a in &OriginTag::all() {
        for &b in &OriginTag::all() {
            for &c in &OriginTag::all() {
                let left = runtime_join(runtime_join(a, b), c);
                let right = runtime_join(a, runtime_join(b, c));
                assert_eq!(
                    left, right,
                    "join not associative at ({:?}, {:?}, {:?})",
                    a, b, c
                );
            }
        }
    }
}

// -- Public `join(Option, Option)` (§3.4 shape) ------------------------

#[test]
fn public_join_some_some_returns_runtime_join() {
    for &a in &OriginTag::all() {
        for &b in &OriginTag::all() {
            assert_eq!(join(Some(a), Some(b)), Some(runtime_join(a, b)));
        }
    }
}

#[test]
fn public_join_returns_none_on_top() {
    // §3.4: returns None for Top. Top is represented as `None` in the
    // Option-based API.
    assert_eq!(join(None, Some(OriginTag::User)), None);
    assert_eq!(join(Some(OriginTag::User), None), None);
    assert_eq!(join(None, None), None);
}

// -- Dominance ----------------------------------------------------------

#[test]
fn dominance_reflexive() {
    for &a in &OriginTag::all() {
        assert!(dominates(a, a));
    }
}

#[test]
fn dominance_host_dominates_all() {
    for &a in &OriginTag::all() {
        assert!(
            dominates(OriginTag::Host, a),
            "Host should dominate {:?}",
            a
        );
    }
}

#[test]
fn dominance_nothing_above_host_except_host() {
    for &a in &OriginTag::all() {
        if a != OriginTag::Host {
            assert!(
                !dominates(a, OriginTag::Host),
                "{:?} must not dominate Host",
                a
            );
        }
    }
}

#[test]
fn dominance_pty_bottom() {
    for &a in &OriginTag::all() {
        assert!(dominates(a, OriginTag::Pty), "{:?} should dominate Pty", a);
    }
}

#[test]
fn dominance_pty_dominates_only_itself() {
    for &a in &OriginTag::all() {
        if a != OriginTag::Pty {
            assert!(
                !dominates(OriginTag::Pty, a),
                "Pty must not dominate {:?}",
                a
            );
        }
    }
}

#[test]
fn dominance_hasse_chain_config_user_ai() {
    // Host dominates everything.
    assert!(dominates(OriginTag::Host, OriginTag::ConfigFile));
    assert!(dominates(OriginTag::Host, OriginTag::User));
    // Both ConfigFile and User dominate Ai.
    assert!(dominates(OriginTag::ConfigFile, OriginTag::Ai));
    assert!(dominates(OriginTag::User, OriginTag::Ai));
    // Design §3.1 table vs. §3 prose discrepancy (flagged for follow-up):
    //
    //   - Prose in §3 says "ConfigFile ... less than User (config is old)",
    //     implying User dominates ConfigFile.
    //   - Table in §3.1 has join(User, ConfigFile) = User and
    //     join(ConfigFile, User) = User, which under the
    //     widest-origin-wins semantics means ConfigFile >= User
    //     (ConfigFile dominates User, User does NOT dominate ConfigFile).
    //
    // We follow the §3.1 *table* because it is the machine-readable
    // source of truth (build.rs reads it, TLA+ mirrors it). The prose
    // contradiction is tracked for designer review before the
    // framework's semantics are locked in via Phase 1 #8005.
    assert!(dominates(OriginTag::ConfigFile, OriginTag::User));
    assert!(!dominates(OriginTag::User, OriginTag::ConfigFile));
}

// -- Type-level JoinWith (compile-time witness via Origin::TAG) --------

#[test]
fn typelevel_join_host_pty_is_pty() {
    assert_eq!(
        <<Host as JoinWith<Pty>>::Output as Origin>::TAG,
        OriginTag::Pty
    );
}

#[test]
fn typelevel_join_host_host_is_host() {
    assert_eq!(
        <<Host as JoinWith<Host>>::Output as Origin>::TAG,
        OriginTag::Host
    );
}

#[test]
fn typelevel_join_user_ai_is_ai() {
    assert_eq!(
        <<User as JoinWith<Ai>>::Output as Origin>::TAG,
        OriginTag::Ai
    );
}

#[test]
fn typelevel_join_all_36_cells_match_runtime() {
    // Spot-check every compile-time JoinWith output against runtime_join.
    // This proves the build.rs-generated impls agree with the hand-table.
    macro_rules! check {
        ($a:ty, $b:ty) => {
            let runtime = runtime_join(<$a as Origin>::TAG, <$b as Origin>::TAG);
            let type_level = <<$a as JoinWith<$b>>::Output as Origin>::TAG;
            assert_eq!(
                runtime,
                type_level,
                "JoinWith<{}> for {} drifted",
                stringify!($b),
                stringify!($a),
            );
        };
    }
    // 36 checks, enumerated.
    check!(Host, Host);
    check!(Host, ConfigFile);
    check!(Host, User);
    check!(Host, Ai);
    check!(Host, NetworkUntrusted);
    check!(Host, Pty);
    check!(ConfigFile, Host);
    check!(ConfigFile, ConfigFile);
    check!(ConfigFile, User);
    check!(ConfigFile, Ai);
    check!(ConfigFile, NetworkUntrusted);
    check!(ConfigFile, Pty);
    check!(User, Host);
    check!(User, ConfigFile);
    check!(User, User);
    check!(User, Ai);
    check!(User, NetworkUntrusted);
    check!(User, Pty);
    check!(Ai, Host);
    check!(Ai, ConfigFile);
    check!(Ai, User);
    check!(Ai, Ai);
    check!(Ai, NetworkUntrusted);
    check!(Ai, Pty);
    check!(NetworkUntrusted, Host);
    check!(NetworkUntrusted, ConfigFile);
    check!(NetworkUntrusted, User);
    check!(NetworkUntrusted, Ai);
    check!(NetworkUntrusted, NetworkUntrusted);
    check!(NetworkUntrusted, Pty);
    check!(Pty, Host);
    check!(Pty, ConfigFile);
    check!(Pty, User);
    check!(Pty, Ai);
    check!(Pty, NetworkUntrusted);
    check!(Pty, Pty);
}

// -- OriginsCompatible (dominance at type level) -----------------------

/// Utility that statically requires `A: OriginsCompatible<B>`. If it
/// compiles, the lattice relation holds at type level.
fn require_compat<A: OriginsCompatible<B>, B: Origin>() {}

#[test]
fn origins_compatible_host_dominates_pty() {
    require_compat::<Host, Pty>();
    require_compat::<Host, NetworkUntrusted>();
    require_compat::<Host, Ai>();
    require_compat::<Host, User>();
    require_compat::<Host, ConfigFile>();
    require_compat::<Host, Host>();
}

#[test]
fn origins_compatible_reflexive_on_all() {
    require_compat::<ConfigFile, ConfigFile>();
    require_compat::<User, User>();
    require_compat::<Ai, Ai>();
    require_compat::<NetworkUntrusted, NetworkUntrusted>();
    require_compat::<Pty, Pty>();
}

#[test]
fn origins_compatible_ai_dominates_network_and_pty() {
    // Ai sits above NetworkUntrusted and Pty per the operational lattice.
    require_compat::<Ai, NetworkUntrusted>();
    require_compat::<Ai, Pty>();
}

// -- SelfSameOrigin ----------------------------------------------------

fn require_same<A: SelfSameOrigin<B>, B: Origin>() {}

#[test]
fn self_same_origin_on_every_variant() {
    require_same::<Host, Host>();
    require_same::<ConfigFile, ConfigFile>();
    require_same::<User, User>();
    require_same::<Ai, Ai>();
    require_same::<NetworkUntrusted, NetworkUntrusted>();
    require_same::<Pty, Pty>();
}

// -- Authorize ceremonies -----------------------------------------------
//
// The two `authorize_*` tests below call
// `HostAuthorizationToken::__new_for_capability_only` and
// `NetworkAuthorizationToken::__new_for_capability_only`, which are gated
// behind the `internal-mint` feature (#8013). Run with:
//     cargo test -p aterm-provenance --features internal-mint
// CI workflows that exercise the ceremonies pass `--features internal-mint`;
// see `.github/workflows/check-provenance-ceremony.yml`.

#[cfg(feature = "internal-mint")]
#[test]
fn authorize_pty_to_host_lifts_tag() {
    let p = Provenance::<_, Pty>::from_pty(b"ls".to_vec());
    // External tests must use the capability-seal constructor; the
    // bare `new()` is `pub(crate)` (#8001) and the seal constructor is
    // `internal-mint`-gated (#8013).
    let tok = HostAuthorizationToken::__new_for_capability_only();
    let host: Provenance<Vec<u8>, Host> = authorize_pty_to_host(p, tok);
    assert_eq!(host.tag(), OriginTag::Host);
    assert_eq!(host.as_ref(), b"ls");
}

#[cfg(feature = "internal-mint")]
#[test]
fn authorize_network_to_host_lifts_tag() {
    let p = Provenance::<_, NetworkUntrusted>::from_network_untrusted(7u32);
    let tok = NetworkAuthorizationToken::__new_for_capability_only();
    let host: Provenance<u32, Host> = authorize_network_to_host(p, tok);
    assert_eq!(host.tag(), OriginTag::Host);
    assert_eq!(*host.as_ref(), 7);
}

// -- UnliftableTop -----------------------------------------------------

#[test]
fn unliftable_top_has_display() {
    let e = UnliftableTop::Top;
    let s = format!("{}", e);
    assert!(s.contains("Top"));
}

#[test]
fn top_sentinel_is_not_origin() {
    // This function only compiles when `T: Origin`. `Top` does not
    // implement `Origin`, so this function cannot be instantiated with
    // `T = Top`. The compile-fail test in tests/compile_fail/ui/
    // exercises the negative path; here we just construct `Top` as a
    // value to confirm it is a real type.
    let _t: Top = Top;
    fn _accepts_origin<T: Origin>() {}
    _accepts_origin::<Host>();
}

// -- TOP_TAG_U8 --------------------------------------------------------

#[test]
fn top_tag_u8_disjoint_from_origin_tags() {
    // 0xFF must not collide with any OriginTag discriminant.
    for &a in &OriginTag::all() {
        assert_ne!(a.as_u8(), TOP_TAG_U8);
    }
}

// -- Drop-on-Top metrics (#8003) ---------------------------------------

use std::sync::Mutex;

/// Metrics tests mutate process-global counters. Serialize access and
/// reset before each test so per-test deltas are deterministic. The
/// metric module exposes a test-only reset hook via a re-export helper.
static METRICS_TEST_LOCK: Mutex<()> = Mutex::new(());

fn with_reset_metrics<R>(body: impl FnOnce() -> R) -> R {
    let guard = METRICS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Reset is internal and not in the public API; we compute deltas
    // instead, which is safer under any concurrent benchmark harness.
    let before: Vec<_> = Subsystem::all()
        .iter()
        .map(|&s| (s, drop_on_top_count(s)))
        .collect();
    let out = body();
    // Release — prior tests can observe our increments but we always
    // assert on deltas, not absolutes.
    drop(before); // silence unused
    drop(guard);
    out
}

#[test]
fn subsystem_names_follow_design_pattern() {
    // §7.3: metric name pattern is `provenance.drop_on_top.<subsystem>`.
    for &s in &Subsystem::all() {
        let name = s.name();
        assert!(
            name.starts_with("provenance.drop_on_top."),
            "subsystem {s:?} name {name:?} does not follow pattern"
        );
    }
}

#[test]
fn subsystem_all_has_six_unique_names() {
    let names: Vec<_> = Subsystem::all().iter().map(|s| s.name()).collect();
    assert_eq!(names.len(), 6);
    for (i, n) in names.iter().enumerate() {
        for m in &names[i + 1..] {
            assert_ne!(n, m, "duplicate subsystem name {n}");
        }
    }
}

#[test]
fn record_drop_on_top_increments_by_one() {
    with_reset_metrics(|| {
        for &s in &Subsystem::all() {
            let before = drop_on_top_count(s);
            let after = record_drop_on_top(s);
            // `record_drop_on_top` returns post-increment.
            assert_eq!(after, before + 1);
            assert_eq!(drop_on_top_count(s), before + 1);
        }
    });
}

#[test]
fn record_drop_on_top_isolated_per_subsystem() {
    with_reset_metrics(|| {
        let before: Vec<u64> = Subsystem::all()
            .iter()
            .map(|&s| drop_on_top_count(s))
            .collect();
        // Increment only Grid.
        record_drop_on_top(Subsystem::Grid);
        for (i, &s) in Subsystem::all().iter().enumerate() {
            let expected = if s == Subsystem::Grid {
                before[i] + 1
            } else {
                before[i]
            };
            assert_eq!(drop_on_top_count(s), expected, "{s:?} leaked increment");
        }
    });
}

// -- DynProvenance Top / drop_if_top -----------------------------------

#[test]
fn dyn_new_top_is_top() {
    let d = DynProvenance::new_top(5u8, OriginTag::Pty);
    assert!(d.is_top());
    assert_eq!(d.tag_byte(), TOP_TAG_U8);
    // Witness tag is retained for diagnostics.
    assert_eq!(d.tag(), OriginTag::Pty);
}

#[test]
fn dyn_new_concrete_is_not_top() {
    let d = DynProvenance::new(5u8, OriginTag::User);
    assert!(!d.is_top());
    assert_eq!(d.tag_byte(), OriginTag::User.as_u8());
}

#[test]
fn dyn_try_as_on_top_returns_err() {
    let d = DynProvenance::new_top(5u8, OriginTag::Pty);
    // Even though witness tag is Pty, try_as must reject because it is Top.
    assert!(d.try_as::<Pty>().is_err());
}

#[test]
fn drop_if_top_drops_top_and_increments() {
    with_reset_metrics(|| {
        let before = drop_on_top_count(Subsystem::Grid);
        let d = DynProvenance::new_top(vec![1u8, 2, 3], OriginTag::Pty);
        let r = d.drop_if_top(Subsystem::Grid);
        assert!(r.is_none(), "Top must be dropped");
        assert_eq!(drop_on_top_count(Subsystem::Grid), before + 1);
    });
}

#[test]
fn drop_if_top_passes_through_non_top() {
    with_reset_metrics(|| {
        let before = drop_on_top_count(Subsystem::Memory);
        let d = DynProvenance::new(42u32, OriginTag::Ai);
        let r = d.drop_if_top(Subsystem::Memory);
        assert!(r.is_some(), "non-Top must pass through");
        assert_eq!(
            drop_on_top_count(Subsystem::Memory),
            before,
            "non-Top must not increment"
        );
    });
}

#[test]
fn drop_if_top_meters_correct_subsystem() {
    with_reset_metrics(|| {
        let before: Vec<_> = Subsystem::all()
            .iter()
            .map(|&s| (s, drop_on_top_count(s)))
            .collect();
        let d = DynProvenance::new_top(0u8, OriginTag::Pty);
        let _ = d.drop_if_top(Subsystem::Voice);
        for (s, was) in before {
            let now = drop_on_top_count(s);
            let expected = if s == Subsystem::Voice { was + 1 } else { was };
            assert_eq!(now, expected, "{s:?} wrong delta");
        }
    });
}

// -- try_authorize_*_dyn -----------------------------------------------
//
// These tests construct capability tokens via
// `HostAuthorizationToken::__new_for_capability_only` /
// `NetworkAuthorizationToken::__new_for_capability_only`, which are gated
// behind the `internal-mint` feature (#8013). Each test is gated behind
// the same feature so default `cargo test -p aterm-provenance` still
// compiles. Run with `--features internal-mint` to exercise them.

#[cfg(feature = "internal-mint")]
#[test]
fn try_authorize_pty_to_host_dyn_accepts_pty_carrier() {
    with_reset_metrics(|| {
        let d: DynProvenance<&'static [u8]> = DynProvenance::new(b"ls".as_slice(), OriginTag::Pty);
        let tok = HostAuthorizationToken::__new_for_capability_only();
        let before = drop_on_top_count(Subsystem::Ffi);
        let host = try_authorize_pty_to_host_dyn(d, tok, Subsystem::Ffi).expect("Pty lifts");
        assert_eq!(host.tag(), OriginTag::Host);
        assert_eq!(*host.as_ref(), b"ls".as_slice());
        assert_eq!(
            drop_on_top_count(Subsystem::Ffi),
            before,
            "tag-accept must not increment"
        );
    });
}

#[cfg(feature = "internal-mint")]
#[test]
fn try_authorize_pty_to_host_dyn_rejects_top_with_metric() {
    with_reset_metrics(|| {
        let d: DynProvenance<u8> = DynProvenance::new_top(0u8, OriginTag::Pty);
        let tok = HostAuthorizationToken::__new_for_capability_only();
        let before = drop_on_top_count(Subsystem::Ffi);
        let e = try_authorize_pty_to_host_dyn(d, tok, Subsystem::Ffi).expect_err("Top rejected");
        match e {
            UnliftableTop::Top => {}
        }
        assert_eq!(drop_on_top_count(Subsystem::Ffi), before + 1);
    });
}

#[cfg(feature = "internal-mint")]
#[test]
fn try_authorize_pty_to_host_dyn_rejects_mismatch_without_metric() {
    with_reset_metrics(|| {
        // Non-Top, non-Pty carrier. Lifts that do not match expected origin
        // are a fail-closed policy decision; they are NOT a Top drop so the
        // metric must not increment.
        let d: DynProvenance<u8> = DynProvenance::new(0u8, OriginTag::Ai);
        let tok = HostAuthorizationToken::__new_for_capability_only();
        let before = drop_on_top_count(Subsystem::Ffi);
        let e = try_authorize_pty_to_host_dyn(d, tok, Subsystem::Ffi).expect_err("mismatch");
        match e {
            UnliftableTop::Top => {}
        }
        assert_eq!(
            drop_on_top_count(Subsystem::Ffi),
            before,
            "tag-mismatch must NOT increment"
        );
    });
}

#[cfg(feature = "internal-mint")]
#[test]
fn try_authorize_network_to_host_dyn_accepts_network_carrier() {
    with_reset_metrics(|| {
        let d: DynProvenance<u32> = DynProvenance::new(7, OriginTag::NetworkUntrusted);
        let tok = NetworkAuthorizationToken::__new_for_capability_only();
        let host = try_authorize_network_to_host_dyn(d, tok, Subsystem::Notification)
            .expect("network lifts");
        assert_eq!(host.tag(), OriginTag::Host);
        assert_eq!(*host.as_ref(), 7);
    });
}

#[cfg(feature = "internal-mint")]
#[test]
fn try_authorize_network_to_host_dyn_rejects_top_with_metric() {
    with_reset_metrics(|| {
        let d: DynProvenance<u32> = DynProvenance::new_top(9, OriginTag::NetworkUntrusted);
        let tok = NetworkAuthorizationToken::__new_for_capability_only();
        let before = drop_on_top_count(Subsystem::Notification);
        let e = try_authorize_network_to_host_dyn(d, tok, Subsystem::Notification)
            .expect_err("Top rejected");
        match e {
            UnliftableTop::Top => {}
        }
        assert_eq!(drop_on_top_count(Subsystem::Notification), before + 1);
    });
}

// -- Coverage: all 6 subsystems meter -----------------------------------

#[test]
fn every_subsystem_meter_is_independent() {
    with_reset_metrics(|| {
        let snap_before: Vec<_> = Subsystem::all()
            .iter()
            .map(|&s| (s, drop_on_top_count(s)))
            .collect();
        for (i, &s) in Subsystem::all().iter().enumerate() {
            for _ in 0..=i {
                let d: DynProvenance<u8> = DynProvenance::new_top(0, OriginTag::Pty);
                let _ = d.drop_if_top(s);
            }
        }
        for (i, (s, was)) in snap_before.into_iter().enumerate() {
            let expected = was + (i as u64 + 1);
            assert_eq!(drop_on_top_count(s), expected, "{s:?}");
        }
    });
}

// -- Subsystem-bound helpers (§7.2 call-site ergonomics) ----------------

#[test]
fn subsystem_helpers_route_to_correct_counter() {
    with_reset_metrics(|| {
        let before: Vec<_> = Subsystem::all()
            .iter()
            .map(|&s| (s, drop_on_top_count(s)))
            .collect();
        assert!(drop_if_top_grid(DynProvenance::<u8>::new_top(0, OriginTag::Pty)).is_none());
        assert!(drop_if_top_memory(DynProvenance::<u8>::new_top(0, OriginTag::Pty)).is_none());
        assert!(drop_if_top_predictor(DynProvenance::<u8>::new_top(0, OriginTag::Pty)).is_none());
        assert!(drop_if_top_voice(DynProvenance::<u8>::new_top(0, OriginTag::Pty)).is_none());
        assert!(
            drop_if_top_notification(DynProvenance::<u8>::new_top(0, OriginTag::Pty)).is_none()
        );
        assert!(drop_if_top_ffi(DynProvenance::<u8>::new_top(0, OriginTag::Pty)).is_none());
        for (s, was) in before {
            assert_eq!(drop_on_top_count(s), was + 1, "{s:?} helper did not meter");
        }
    });
}

#[test]
fn subsystem_helpers_pass_through_non_top() {
    with_reset_metrics(|| {
        let before_grid = drop_on_top_count(Subsystem::Grid);
        let d = DynProvenance::new(7u8, OriginTag::Pty);
        let r = drop_if_top_grid(d).expect("non-Top must pass through");
        assert_eq!(*r.as_ref(), 7);
        assert_eq!(
            drop_on_top_count(Subsystem::Grid),
            before_grid,
            "non-Top must not meter"
        );
    });
}
