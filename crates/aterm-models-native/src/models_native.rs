// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::*;

/// Lane-exact admission for the fixed control-socket worker pool.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn control_connection_admission_model() -> Model {
    aterm_spec::ty_model! {
        ControlConnectionAdmission {
            const Buggy = 0;
            const LaneCap = 2;
            const MaxArrivals = 4;
            var outstanding = 0;
            var arrivals = 0;
            var accepted = 0;
            var rejected = 0;
            var completed = 0;
            action Admit when (
                arrivals <= MaxArrivals - 1 &&
                outstanding <= if Buggy == 1 { LaneCap } else { LaneCap - 1 }
            ) {
                outstanding = outstanding + 1;
                arrivals = arrivals + 1;
                accepted = accepted + 1;
            }
            action Reject when (
                arrivals <= MaxArrivals - 1 && outstanding == LaneCap
            ) {
                arrivals = arrivals + 1;
                rejected = rejected + 1;
            }
            action Complete when (outstanding > 0 && completed <= accepted - 1) {
                outstanding = if Buggy == 1 { outstanding } else { outstanding - 1 };
                completed = completed + 1;
            }
            invariant LaneBounded: outstanding <= LaneCap;
            bound ArrivalsBounded: arrivals <= MaxArrivals;
            invariant AcceptedWorkAccounted: accepted == outstanding + completed;
        }
    }
}

/// WHERE A CONTROL CONNECTION IS between its requests, and so what a fresh client is refused for (`aterm-gui/src/control_lanes.rs`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn control_lane_tenure_model() -> Model {
    aterm_spec::ty_model! {
        ControlLaneTenure {
            const Buggy = 0;
            const Lanes = 2;
            const Waits = 1;
            const Inline = 1;
            const Cap = 3;
            const Arrivals = 6;
            var working = 0;
            var held = 0;
            var waiting = 0;
            var inline = 0;
            var parked = 0;
            var spared = 0;
            var placing = 0;
            var queued = 0;
            var open = 0;
            var arrivals = 0;
            var refused_idle = 0;
            var busied = 0;
            var bypassed = 0;
            var refused_parked = 0;
            action Admit when (
                arrivals <= Arrivals - 1 &&
                (Buggy == 1 || open <= Cap - 1) &&
                working + held + inline + queued <= if Buggy == 1 { Lanes } else { Lanes - 1 }
            ) {
                working = working + 1;
                open = open + 1;
                arrivals = arrivals + 1;
            }
            action Hand when (
                arrivals <= Arrivals - 1 &&
                Cap <= open && placing <= parked - 1 &&
                working + held + inline + queued <= Lanes - 1
            ) {
                placing = placing + 1;
                arrivals = arrivals + 1;
            }
            action PlaceFreed when (placing > 0 && open <= Cap - 1) {
                placing = placing - 1;
                open = open + 1;
                parked = parked + 1;
                spared = spared + 1;
            }
            action Place when (placing > 0 && Cap <= open && spared <= parked - 1) {
                placing = placing - 1;
                spared = spared + 1;
            }
            action TurnAway when (placing > 0 && Cap <= open && parked <= spared) {
                placing = placing - 1;
            }
            action Refuse when (
                Lanes <= working + held + inline + queued ||
                (Cap <= open && (Buggy == 1 || parked <= placing))
            ) {
                refused_idle = if held > 0 { 1 } else { refused_idle };
                refused_parked = if placing + spared <= parked - 1 &&
                    working + held + inline + queued <= Lanes - 1 {
                    1
                } else {
                    refused_parked
                };
            }
            action Spare when (spared <= parked - 1) {
                spared = spared + 1;
            }
            action Unspare when (spared > 0) {
                spared = spared - 1;
            }
            action Finish when (working > 0) {
                working = working - 1;
                held = if Buggy == 1 { held + 1 } else { held };
                parked = if Buggy == 1 { parked } else { parked + 1 };
            }
            action Defer when (
                working > 0 && (Buggy == 1 || waiting <= Waits - 1)
            ) {
                working = working - 1;
                waiting = waiting + 1;
            }
            action InlineWait when (
                working > 0 && (Buggy == 1 || (Waits <= waiting && inline <= Inline - 1))
            ) {
                working = working - 1;
                inline = inline + 1;
                bypassed = if waiting <= Waits - 1 { 1 } else { bypassed };
            }
            action BusyWait when (
                working > 0 && Inline <= inline && (Buggy == 1 || Waits <= waiting)
            ) {
                working = working - 1;
                parked = parked + 1;
                busied = 1;
                bypassed = if waiting <= Waits - 1 { 1 } else { bypassed };
            }
            action WaitFinish when (waiting > 0) {
                waiting = waiting - 1;
                parked = parked + 1;
            }
            action InlineFinish when (inline > 0) {
                inline = inline - 1;
                parked = parked + 1;
            }
            action Wake when (spared <= parked - 1) {
                parked = parked - 1;
                queued = queued + 1;
            }
            action Pick when (queued > 0 && working + held + inline <= Lanes - 1) {
                queued = queued - 1;
                working = working + 1;
            }
            action Close when (working > 0) {
                working = working - 1;
                open = open - 1;
            }
            action WaitClose when (waiting > 0) {
                waiting = waiting - 1;
                open = open - 1;
            }
            action InlineClose when (inline > 0) {
                inline = inline - 1;
                open = open - 1;
            }
            action ParkedClose when (spared <= parked - 1) {
                parked = parked - 1;
                open = if Buggy == 1 { open } else { open - 1 };
            }
            invariant NoIdleHold: held == 0;
            invariant NoRefusalByIdleDriver: refused_idle == 0;
            invariant NoRefusalBesideParked: refused_parked == 0;
            invariant RequestLanesBounded: working + held + inline <= Lanes;
            invariant WaitLanesBounded: waiting <= Waits;
            invariant RequestLaneLeftForRequests: held + inline <= Lanes - 1;
            invariant WaitLaneTakenFirst: bypassed == 0;
            invariant ParkBounded: parked + queued <= Cap;
            invariant EveryConnectionPlaced:
                working + held + waiting + inline + parked + queued == open;
        }
    }
}

/// Request classification when the focused content may be native rather than a terminal.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_control_routing_model() -> Model {
    aterm_spec::ty_model! {
        NativeControlRouting {
            const Buggy = 0;
            var front_kind = 1;
            var active_terminal = 1;
            var explicit_session_live = 1;
            var owner_app_allowed = 1;
            var bare_session_allowed = 1;
            var explicit_session_allowed = 1;
            var edge_app_allowed = 0;
            var hidden_terminal_fallback = 0;
            var session_without_target = 0;
            action FocusNative {
                front_kind = 2;
                active_terminal = if Buggy == 1 { 1 } else { 0 };
                owner_app_allowed = if Buggy == 1 { 0 } else { 1 };
                bare_session_allowed = if Buggy == 1 { 1 } else { 0 };
                explicit_session_allowed = explicit_session_live;
                edge_app_allowed = 0;
                hidden_terminal_fallback = if Buggy == 1 { 1 } else { 0 };
                session_without_target = if Buggy == 1 { 1 } else { 0 };
            }
            action FocusTerminal {
                front_kind = 1;
                active_terminal = 1;
                owner_app_allowed = 1;
                bare_session_allowed = 1;
                explicit_session_allowed = explicit_session_live;
                edge_app_allowed = if Buggy == 1 { 1 } else { 0 };
                hidden_terminal_fallback = 0;
                session_without_target = 0;
            }
            action RetireExplicitSession {
                explicit_session_live = 0;
                explicit_session_allowed = if Buggy == 1 { active_terminal } else { 0 };
                bare_session_allowed = if Buggy == 1 { 0 } else { bare_session_allowed };
            }
            action RestoreExplicitSession {
                explicit_session_live = 1;
                explicit_session_allowed = if Buggy == 1 { active_terminal } else { 1 };
                bare_session_allowed = if Buggy == 1 { 1 } else { bare_session_allowed };
            }
            bound FrontKindBounded: front_kind > 0 && front_kind <= 2;
            invariant FrontKindMatchesTerminalMirror:
                if front_kind == 1 {
                    active_terminal == 1
                } else {
                    active_terminal == 0
                };
            invariant OwnerAppAlwaysAllowed: owner_app_allowed == 1;
            invariant BareSessionIffFrontTerminal:
                bare_session_allowed == active_terminal;
            invariant ExplicitSessionIffLive:
                explicit_session_allowed == explicit_session_live;
            invariant EdgeAppDenied: edge_app_allowed == 0;
            invariant NoHiddenTerminalFallback: hidden_terminal_fallback == 0;
            invariant NoSessionWithoutTarget: session_without_target == 0;
        }
    }
}

/// Stable tab/view identity and live-reference discipline for the generic native tab core.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_tab_identity_model() -> Model {
    aterm_spec::ty_model! {
        NativeTabIdentity {
            const Buggy = 0;
            const MaxTabs = 3;
            const MaxId = 5;
            var tab_count = 1;
            var tab_one = 1;
            var tab_two = 0;
            var tab_three = 0;
            var tab_view_one = 1;
            var tab_view_two = 0;
            var tab_view_three = 0;
            var live_view_one = 1;
            var live_view_two = 0;
            var live_view_three = 0;
            var active_tab = 1;
            var focused_view = 1;
            var next_tab_id = 2;
            var next_view_id = 2;
            var retired_tab_id = 0;
            var retired_view_id = 0;
            var close_count = 0;
            var removed_count = 0;
            var reused_tab_id = 0;
            var reused_view_id = 0;
            action OpenTab when (
                tab_count <= MaxTabs - 1 &&
                next_tab_id <= if Buggy == 1 && retired_tab_id > 0 {
                    MaxId + 1
                } else { MaxId } &&
                next_view_id <= if Buggy == 1 && retired_view_id > 0 {
                    MaxId + 1
                } else { MaxId }
            ) {
                tab_two = if tab_count == 1 {
                    if Buggy == 1 && retired_tab_id > 0 { retired_tab_id } else { next_tab_id }
                } else { tab_two };
                tab_three = if tab_count == 2 {
                    if Buggy == 1 && retired_tab_id > 0 { retired_tab_id } else { next_tab_id }
                } else { tab_three };
                tab_view_two = if tab_count == 1 {
                    if Buggy == 1 && retired_view_id > 0 { retired_view_id } else { next_view_id }
                } else { tab_view_two };
                tab_view_three = if tab_count == 2 {
                    if Buggy == 1 && retired_view_id > 0 { retired_view_id } else { next_view_id }
                } else { tab_view_three };
                live_view_two = if tab_count == 1 {
                    if Buggy == 4 {
                        0
                    } else {
                        if Buggy == 1 && retired_view_id > 0 {
                            retired_view_id
                        } else {
                            next_view_id
                        }
                    }
                } else { live_view_two };
                live_view_three = if tab_count == 2 {
                    if Buggy == 4 {
                        0
                    } else {
                        if Buggy == 1 && retired_view_id > 0 {
                            retired_view_id
                        } else {
                            next_view_id
                        }
                    }
                } else { live_view_three };
                tab_count = tab_count + 1;
                active_tab = if Buggy == 1 && retired_tab_id > 0 {
                    retired_tab_id
                } else { next_tab_id };
                focused_view = if Buggy == 1 && retired_view_id > 0 {
                    retired_view_id
                } else { next_view_id };
                next_tab_id = if Buggy == 1 && retired_tab_id > 0 {
                    next_tab_id
                } else { next_tab_id + 1 };
                next_view_id = if Buggy == 1 && retired_view_id > 0 {
                    next_view_id
                } else {
                    if Buggy == 6 && next_view_id <= MaxId - 1 {
                        next_view_id + 2
                    } else {
                        next_view_id + 1
                    }
                };
                reused_tab_id = if Buggy == 1 && retired_tab_id > 0 {
                    1
                } else { reused_tab_id };
                reused_view_id = if Buggy == 1 && retired_view_id > 0 {
                    1
                } else { reused_view_id };
            }
            action SelectFirst {
                active_tab = tab_one;
                focused_view = tab_view_one;
            }
            action SelectSecond when (tab_count > 1) {
                active_tab = tab_two;
                focused_view = tab_view_two;
            }
            action SelectThird when (tab_count == 3) {
                active_tab = tab_three;
                focused_view = tab_view_three;
            }
            action ReorderFirstSecond when (tab_count > 1) {
                tab_one = tab_two;
                tab_two = tab_one;
                tab_view_one = tab_view_two;
                tab_view_two = tab_view_one;
            }
            action ReorderSecondThird when (tab_count == 3) {
                tab_two = tab_three;
                tab_three = tab_two;
                tab_view_two = tab_view_three;
                tab_view_three = tab_view_two;
            }
            action CloseFirst when (tab_count > 1) {
                tab_count = tab_count - 1;
                tab_one = tab_two;
                tab_two = if tab_count == 3 { tab_three } else { 0 };
                tab_three = 0;
                tab_view_one = tab_view_two;
                tab_view_two = if tab_count == 3 { tab_view_three } else { 0 };
                tab_view_three = 0;
                live_view_one = if live_view_one == tab_view_one {
                    live_view_two
                } else {
                    live_view_one
                };
                live_view_two = if live_view_one == tab_view_one {
                    live_view_three
                } else {
                    if live_view_two == tab_view_one { live_view_three } else { live_view_two }
                };
                live_view_three = 0;
                active_tab = if active_tab == tab_one {
                    if Buggy == 1 { active_tab } else { tab_two }
                } else {
                    active_tab
                };
                focused_view = if active_tab == tab_one {
                    if Buggy == 1 { focused_view } else { tab_view_two }
                } else {
                    focused_view
                };
                retired_tab_id = tab_one;
                retired_view_id = tab_view_one;
                close_count = close_count + 1;
                removed_count = removed_count + 1;
            }
            action CloseSecond when (tab_count > 1) {
                tab_count = tab_count - 1;
                tab_two = if tab_count == 3 { if Buggy == 2 { 0 } else { tab_three } } else { 0 };
                tab_three = if Buggy == 2 { tab_three } else { 0 };
                tab_view_two = if tab_count == 3 {
                    if Buggy == 2 { 0 } else { tab_view_three }
                } else { 0 };
                tab_view_three = if Buggy == 2 { tab_view_three } else { 0 };
                live_view_one = if live_view_one == tab_view_two {
                    live_view_two
                } else {
                    live_view_one
                };
                live_view_two = if live_view_one == tab_view_two {
                    live_view_three
                } else {
                    if live_view_two == tab_view_two { live_view_three } else { live_view_two }
                };
                live_view_three = 0;
                active_tab = if active_tab == tab_two {
                    if Buggy == 1 {
                        active_tab
                    } else {
                        if tab_count == 3 { tab_three } else { tab_one }
                    }
                } else {
                    active_tab
                };
                focused_view = if active_tab == tab_two {
                    if Buggy == 1 {
                        focused_view
                    } else {
                        if tab_count == 3 { tab_view_three } else { tab_view_one }
                    }
                } else {
                    focused_view
                };
                retired_tab_id = tab_two;
                retired_view_id = tab_view_two;
                close_count = close_count + 1;
                removed_count = removed_count + 1;
            }
            action CloseThird when (tab_count == 3) {
                tab_count = tab_count - 1;
                tab_three = 0;
                tab_view_three = 0;
                live_view_one = if live_view_one == tab_view_three {
                    live_view_two
                } else {
                    live_view_one
                };
                live_view_two = if live_view_one == tab_view_three {
                    live_view_three
                } else {
                    if live_view_two == tab_view_three { live_view_three } else { live_view_two }
                };
                live_view_three = if Buggy == 3 { live_view_three } else { 0 };
                active_tab = if active_tab == tab_three {
                    if Buggy == 1 { active_tab } else { tab_two }
                } else {
                    active_tab
                };
                focused_view = if active_tab == tab_three {
                    if Buggy == 1 { focused_view } else { tab_view_two }
                } else {
                    focused_view
                };
                retired_tab_id = tab_three;
                retired_view_id = tab_view_three;
                close_count = close_count + 1;
                removed_count = removed_count + 1;
            }
            bound TabCountBounded: tab_count > 0 && tab_count <= MaxTabs;
            invariant TabSlotsCompact:
                if tab_count == 1 {
                    tab_one > 0 && tab_two == 0 && tab_three == 0
                } else {
                    if tab_count == 2 {
                        tab_one > 0 && tab_two > 0 && tab_three == 0
                    } else {
                        tab_one > 0 && tab_two > 0 && tab_three > 0
                    }
                };
            invariant TabViewSlotsCompact:
                if tab_count == 1 {
                    tab_view_one > 0 && tab_view_two == 0 && tab_view_three == 0
                } else {
                    if tab_count == 2 {
                        tab_view_one > 0 && tab_view_two > 0 && tab_view_three == 0
                    } else {
                        tab_view_one > 0 && tab_view_two > 0 && tab_view_three > 0
                    }
                };
            invariant LiveViewSlotsCompact:
                if tab_count == 1 {
                    live_view_one > 0 && live_view_two == 0 && live_view_three == 0
                } else {
                    if tab_count == 2 {
                        live_view_one > 0 && live_view_two > 0 && live_view_three == 0
                    } else {
                        live_view_one > 0 && live_view_two > 0 && live_view_three > 0
                    }
                };
            invariant TabIdsUnique:
                if tab_one == tab_two {
                    tab_two == 0
                } else {
                    if tab_one == tab_three {
                        tab_three == 0
                    } else {
                        if tab_two == tab_three { tab_three == 0 } else { tab_count > 0 }
                    }
                };
            invariant LiveViewIdsUnique:
                if live_view_one == live_view_two {
                    live_view_two == 0
                } else {
                    if live_view_one == live_view_three {
                        live_view_three == 0
                    } else {
                        if live_view_two == live_view_three {
                            live_view_three == 0
                        } else {
                            tab_count > 0
                        }
                    }
                };
            invariant ActiveReferencesLiveTab:
                if active_tab == tab_one {
                    tab_one > 0
                } else {
                    if active_tab == tab_two {
                        tab_two > 0
                    } else {
                        active_tab == tab_three && tab_three > 0
                    }
                };
            invariant FocusMatchesActiveTab:
                if active_tab == tab_one {
                    focused_view == tab_view_one
                } else {
                    if active_tab == tab_two {
                        focused_view == tab_view_two
                    } else {
                        active_tab == tab_three && focused_view == tab_view_three
                    }
                };
            invariant FirstTabViewLive:
                if tab_view_one == live_view_one {
                    tab_view_one > 0
                } else {
                    if tab_view_one == live_view_two {
                        tab_view_one > 0
                    } else {
                        tab_view_one == live_view_three && tab_view_one > 0
                    }
                };
            invariant SecondTabViewLive:
                if tab_count == 1 {
                    tab_view_two == 0
                } else {
                    if tab_view_two == live_view_one {
                        tab_view_two > 0
                    } else {
                        if tab_view_two == live_view_two {
                            tab_view_two > 0
                        } else {
                            tab_view_two == live_view_three && tab_view_two > 0
                        }
                    }
                };
            invariant ThirdTabViewLive:
                if tab_count <= 2 {
                    tab_view_three == 0
                } else {
                    if tab_view_three == live_view_one {
                        tab_view_three > 0
                    } else {
                        if tab_view_three == live_view_two {
                            tab_view_three > 0
                        } else {
                            tab_view_three == live_view_three && tab_view_three > 0
                        }
                    }
                };
            invariant TabIdsNeverReused: reused_tab_id == 0;
            invariant ViewIdsNeverReused: reused_view_id == 0;
            invariant AllocatorsAdvanceTogether: next_tab_id == next_view_id;
            bound TabAllocatorBounded: next_tab_id <= MaxId + 1;
            bound ViewAllocatorBounded: next_view_id <= MaxId + 1;
        }
    }
}

/// Bounded descriptor ledger behind native "Reopen Closed Tab".
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_reopen_ledger_model() -> Model {
    aterm_spec::ty_model! {
        NativeReopenLedger {
            const Buggy = 0;
            const Cap = 3;
            const MaxId = 8;
            const MaxLive = 4;
            const MaxFailures = 2;
            var ledger = 0;
            var native_live = 1;
            var opened_id = 2;
            var retired_id = 0;
            var next_id = 3;
            var failures = 0;
            var reused_retired = 0;
            var lost_on_failure = 0;
            action OpenAnother when (
                native_live <= MaxLive - 1 && next_id <= MaxId
            ) {
                native_live = native_live + 1;
                opened_id = next_id;
                next_id = next_id + 1;
            }
            action Close when (native_live > 0) {
                ledger = if ledger <= Cap - 1 {
                    ledger + 1
                } else {
                    if Buggy == 1 { Cap + 1 } else { Cap }
                };
                native_live = native_live - 1;
                retired_id = opened_id;
            }
            action Reopen when (
                native_live <= MaxLive - 1 && ledger > 0 && next_id <= MaxId
            ) {
                ledger = ledger - 1;
                native_live = native_live + 1;
                opened_id = if Buggy == 1 { retired_id } else { next_id };
                next_id = if Buggy == 1 { next_id } else { next_id + 1 };
                reused_retired = if Buggy == 1 { 1 } else { reused_retired };
            }
            action FailReopen when (
                ledger > 0 && failures <= MaxFailures - 1
            ) {
                ledger = if Buggy == 1 { ledger - 1 } else { ledger };
                failures = failures + 1;
                lost_on_failure = if Buggy == 1 { 1 } else { lost_on_failure };
            }
            invariant LedgerBounded: ledger <= Cap;
            bound NativeLiveBounded: native_live <= MaxLive;
            invariant FreshReopenIdentity: reused_retired == 0;
            invariant FailedReopenRetainsDescriptor: lost_on_failure == 0;
            bound NextIdentityBounded: next_id <= MaxId + 1;
            bound FailureCountBounded: failures <= MaxFailures;
        }
    }
}

/// Independent bounded recovery ledgers for a non-last split leaf and a whole tab.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn closed_recovery_ledgers_model() -> Model {
    aterm_spec::ty_model! {
        ClosedRecoveryLedgers {
            const Buggy = 0;
            const ViewCap = 2;
            const TabCap = 3;
            const MaxLeaves = 3;
            const MaxFailures = 2;
            var view_ledger = 0;
            var tab_ledger = 0;
            var live_tabs = 1;
            var live_leaves = 2;
            var failures = 0;
            var double_recorded = 0;
            var lost_on_failure = 0;
            action CloseView when (live_tabs == 1 && live_leaves > 1) {
                view_ledger = if view_ledger <= ViewCap - 1 {
                    view_ledger + 1
                } else {
                    if Buggy == 1 { ViewCap + 1 } else { ViewCap }
                };
                tab_ledger = if Buggy == 1 {
                    if tab_ledger <= TabCap - 1 { tab_ledger + 1 } else { TabCap + 1 }
                } else {
                    tab_ledger
                };
                live_leaves = live_leaves - 1;
                double_recorded = if Buggy == 1 { 1 } else { double_recorded };
            }
            action CloseTab when (live_tabs == 1) {
                tab_ledger = if tab_ledger <= TabCap - 1 {
                    tab_ledger + 1
                } else {
                    if Buggy == 1 { TabCap + 1 } else { TabCap }
                };
                view_ledger = if Buggy == 1 {
                    if view_ledger <= ViewCap - 1 { view_ledger + 1 } else { ViewCap + 1 }
                } else {
                    view_ledger
                };
                live_tabs = 0;
                live_leaves = 0;
                double_recorded = if Buggy == 1 { 1 } else { double_recorded };
            }
            action OpenTab when (live_tabs == 0) {
                live_tabs = 1;
                live_leaves = 2;
            }
            action ReopenView when (
                view_ledger > 0 && live_tabs == 1 && live_leaves <= MaxLeaves - 1
            ) {
                view_ledger = view_ledger - 1;
                live_leaves = live_leaves + 1;
            }
            action ReopenTab when (tab_ledger > 0 && live_tabs == 0) {
                tab_ledger = tab_ledger - 1;
                live_tabs = 1;
                live_leaves = 1;
            }
            action FailView when (
                view_ledger > 0 && failures <= MaxFailures - 1
            ) {
                view_ledger = if Buggy == 1 { view_ledger - 1 } else { view_ledger };
                failures = failures + 1;
                lost_on_failure = if Buggy == 1 { 1 } else { lost_on_failure };
            }
            action FailTab when (
                tab_ledger > 0 && failures <= MaxFailures - 1
            ) {
                tab_ledger = if Buggy == 1 { tab_ledger - 1 } else { tab_ledger };
                failures = failures + 1;
                lost_on_failure = if Buggy == 1 { 1 } else { lost_on_failure };
            }
            invariant ViewLedgerBounded: view_ledger <= ViewCap;
            invariant TabLedgerBounded: tab_ledger <= TabCap;
            bound LiveLeavesBounded: live_leaves <= MaxLeaves;
            invariant OnlyOneRecordPerClose: double_recorded == 0;
            invariant FailedReopenRetainsRecord: lost_on_failure == 0;
            bound FailureCountBounded: failures <= MaxFailures;
        }
    }
}

/// Bounded, per-view Markdown reading history.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_markdown_history_model() -> Model {
    aterm_spec::ty_model! {
        NativeMarkdownHistory {
            const Buggy = 0;
            const Cap = 3;
            const MaxVisits = 6;
            var len = 0;
            var cursor = 0;
            var visits = 0;
            var last_visit_was_branch = 0;
            var expected_len = 0;
            action Visit when (visits <= MaxVisits - 1) {
                expected_len = if cursor == 0 {
                    1
                } else if cursor <= Cap - 1 {
                    cursor + 1
                } else {
                    Cap
                };
                last_visit_was_branch = if len > cursor { 1 } else { 0 };
                len = if Buggy == 1 {
                    len + 1
                } else if cursor == 0 {
                    1
                } else if cursor <= Cap - 1 {
                    cursor + 1
                } else {
                    Cap
                };
                cursor = if Buggy == 1 {
                    len + 1
                } else if cursor == 0 {
                    1
                } else if cursor <= Cap - 1 {
                    cursor + 1
                } else {
                    Cap
                };
                visits = visits + 1;
            }
            action Back when (cursor > 1 || (Buggy == 1 && cursor == 1)) {
                cursor = cursor - 1;
                last_visit_was_branch = 0;
            }
            action Forward when (len > cursor || (Buggy == 1 && len == cursor && len > 0)) {
                cursor = cursor + 1;
                last_visit_was_branch = 0;
            }
            action Duplicate when (cursor > 0) {
                last_visit_was_branch = 0;
            }
            invariant HistoryBounded: len <= Cap;
            invariant CursorWithinHistory: cursor <= len;
            invariant EmptyIffNoCursor:
                if len == 0 { cursor == 0 } else { cursor > 0 };
            invariant ForwardBranchTruncated:
                if last_visit_was_branch == 1 {
                    len == expected_len
                } else {
                    len <= Cap
                };
            bound VisitsBounded: visits <= MaxVisits;
        }
    }
}

/// Exact intra-block Markdown navigation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_markdown_viewport_model() -> Model {
    aterm_spec::ty_model! {
        NativeMarkdownViewport {
            const Buggy = 0;
            const BlockRows = 4;
            const MaxSteps = 6;
            var actual_row = 0;
            var expected_row = 0;
            var steps = 0;
            action Step when (steps <= MaxSteps - 1) {
                actual_row = if Buggy == 1 {
                    actual_row + BlockRows
                } else {
                    actual_row + 1
                };
                expected_row = expected_row + 1;
                steps = steps + 1;
            }
            invariant ExactIntraBlockProgress: actual_row == expected_row;
            bound StepsBounded: steps <= MaxSteps;
        }
    }
}

/// Renderer-sized editor caret reveal and stable scrolling.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_editor_viewport_model() -> Model {
    aterm_spec::ty_model! {
        NativeEditorViewport {
            const Buggy = 0;
            const CaretLine = 20;
            const CompactLines = 8;
            const DesktopGuess = 36;
            const ShortDocumentLines = 3;
            const TallViewportLines = 40;
            const ScrollDocumentLines = 13;
            const ScrollViewportLines = 4;
            var anchor_line = 0;
            var visible_lines = 36;
            var short_anchor_line = 2;
            var short_visible_lines = 4;
            var resized = 0;
            var scroll_anchor_line = 0;
            var scroll_phase = 0;
            action Resize when (resized == 0) {
                visible_lines = CompactLines;
                anchor_line = if Buggy == 1 {
                    0
                } else {
                    CaretLine - CompactLines + 3
                };
                short_visible_lines = TallViewportLines;
                short_anchor_line = if Buggy == 1 { 2 } else { 0 };
                resized = 1;
            }
            action Overscroll when (scroll_phase == 0) {
                scroll_anchor_line = if Buggy == 1 {
                    ScrollDocumentLines - 1
                } else {
                    ScrollDocumentLines - ScrollViewportLines
                };
                scroll_phase = 1;
            }
            action ReverseScroll when (scroll_phase == 1) {
                scroll_anchor_line = scroll_anchor_line - 1;
                scroll_phase = 2;
            }
            invariant CaretVisibleAfterResize:
                if resized == 1 {
                    anchor_line <= CaretLine &&
                    CaretLine <= anchor_line + visible_lines - 1
                } else {
                    visible_lines == DesktopGuess
                };
            invariant ShortDocumentFullyVisible:
                if resized == 1 {
                    ShortDocumentLines <= short_visible_lines &&
                    short_anchor_line == 0
                } else {
                    short_visible_lines == 4 && short_anchor_line == 2
                };
            invariant StoredScrollAnchorPresentable:
                scroll_anchor_line <= ScrollDocumentLines - ScrollViewportLines;
            invariant FirstReverseStepMoves:
                if scroll_phase == 2 {
                    scroll_anchor_line == ScrollDocumentLines - ScrollViewportLines - 1
                } else {
                    scroll_anchor_line <= ScrollDocumentLines - 1
                };
            bound ScrollPhaseBounded: scroll_phase <= 2;
        }
    }
}

/// Bounded M-x completion lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_editor_command_palette_model() -> Model {
    aterm_spec::ty_model! {
        NativeEditorCommandPalette {
            const Buggy = 0;
            var mode = 0;
            var query_phase = 0;
            var results = 0;
            var selected = 0;
            var query_changed = 0;
            var submitted = 0;
            var exact_selected_dispatch = 0;
            action Open when (mode == 0) {
                mode = 1;
                query_phase = 0;
                results = 4;
                selected = 0;
                query_changed = 0;
                submitted = 0;
                exact_selected_dispatch = 0;
            }
            action TypeBroad when (mode == 1 && query_phase == 0) {
                query_phase = 1;
                results = 2;
                selected = 0;
                query_changed = 1;
            }
            action MoveNext when (
                mode == 1 && query_phase == 1 && selected <= results - 2
            ) {
                selected = selected + 1;
                query_changed = 0;
            }
            action MovePrevious when (
                mode == 1 && query_phase == 1 && selected > 0
            ) {
                selected = selected - 1;
                query_changed = 0;
            }
            action Refine when (mode == 1 && query_phase == 1) {
                query_phase = 2;
                results = 1;
                selected = if Buggy == 1 { selected } else { 0 };
                query_changed = 1;
            }
            action TabComplete when (mode == 1 && results > 0) {
                query_phase = 2;
                results = 1;
                selected = 0;
                query_changed = 1;
            }
            action Submit when (mode == 1 && results > 0 && selected == 0) {
                mode = 0;
                query_phase = 0;
                results = 0;
                selected = 0;
                query_changed = 0;
                submitted = 1;
                exact_selected_dispatch = if Buggy == 1 { 0 } else { 1 };
            }
            action Abort when (mode == 1) {
                mode = 0;
                query_phase = 0;
                results = 0;
                selected = 0;
                query_changed = 0;
                submitted = 0;
                exact_selected_dispatch = 0;
            }
            invariant SelectionWithinResults:
                if mode == 1 {
                    results > 0 && selected <= results - 1
                } else {
                    results == 0 && selected == 0
                };
            invariant QueryChangeResetsSelection:
                if query_changed == 1 { selected == 0 } else { selected <= results };
            invariant SubmitIsExactSelected:
                submitted == exact_selected_dispatch;
            bound ResultsBounded: results <= 4;
            bound PhaseBounded: query_phase <= 2;
        }
    }
}

/// Bounded Settings ▸ Manual completion lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn manual_config_completion_model() -> Model {
    aterm_spec::ty_model! {
        ManualConfigCompletion {
            const Buggy = 0;
            const Results = 8;
            const Capacity = 3;
            const MaxContext = 3;
            var context = 1;
            var assist_visible = 1;
            var interacting = 0;
            var selected = 0;
            var window_start = 0;
            var dismissed_context = 0;
            var document_edits = 0;
            var ordinary_enter = 0;
            var caret = 0;
            var ordinary_down = 0;
            var expected_accept = 0;
            var accepted = 0;

            action OrdinaryEnter when (
                assist_visible == 1 && interacting == 0 && context <= MaxContext - 1
            ) {
                context = context + 1;
                document_edits = if Buggy == 1 { document_edits } else { document_edits + 1 };
                ordinary_enter = ordinary_enter + 1;
                selected = 0;
                window_start = 0;
            }
            action OrdinaryDown when (
                assist_visible == 1 && interacting == 0 && context <= MaxContext - 1
            ) {
                context = context + 1;
                caret = if Buggy == 4 { caret } else { caret + 1 };
                ordinary_down = ordinary_down + 1;
                selected = 0;
                window_start = 0;
            }
            action EnterSelection when (assist_visible == 1 && interacting == 0) {
                interacting = 1;
            }
            action TabEnterSelection when (assist_visible == 1 && interacting == 0) {
                interacting = 1;
            }
            action MoveNext when (
                interacting == 1 &&
                    (selected <= Results - 2 || (Buggy == 2 && selected <= Results - 1))
            ) {
                selected = selected + 1;
                window_start = if Buggy == 1 {
                    window_start
                } else {
                    if selected + 1 <= Capacity - 1 {
                        0
                    } else {
                        if selected + 1 <= Capacity + Capacity - 1 {
                            Capacity
                        } else {
                            if Buggy == 3 { Capacity + Capacity + Capacity } else { 6 }
                        }
                    }
                };
            }
            action MovePrevious when (interacting == 1 && selected > 0) {
                selected = selected - 1;
                window_start = if selected - 1 <= Capacity - 1 {
                    0
                } else {
                    if selected - 1 <= Capacity + Capacity - 1 { Capacity } else { 6 }
                };
            }
            action AcceptSelected when (interacting == 1) {
                expected_accept = selected + 1;
                accepted = if Buggy == 1 { 1 } else { selected + 1 };
                interacting = 0;
                assist_visible = 0;
            }
            action Dismiss when (assist_visible == 1) {
                assist_visible = 0;
                interacting = 0;
                dismissed_context = context;
            }
            action ChangeDismissedContext when (
                assist_visible == 0 && dismissed_context == context && context <= MaxContext - 1
            ) {
                context = if Buggy == 5 { context } else { context + 1 };
                assist_visible = 1;
                selected = 0;
                window_start = 0;
            }
            action Settled when (accepted > 0) {
                accepted = accepted;
            }

            invariant SelectionWithinResults: selected <= Results - 1;
            invariant SelectedCandidateVisible:
                if assist_visible == 1 {
                    window_start <= selected && selected <= window_start + Capacity - 1
                } else {
                    selected <= Results - 1
                };
            invariant WindowWithinResults: window_start <= Results - 1;
            invariant OrdinaryEnterEditsDocument: ordinary_enter == document_edits;
            invariant OrdinaryDownMovesCaret: ordinary_down == caret;
            invariant AcceptsExactSelected:
                if accepted > 0 { accepted == expected_accept } else { expected_accept == 0 };
            invariant ExactContextDismissal:
                if dismissed_context == context && dismissed_context > 0 {
                    assist_visible == 0
                } else {
                    assist_visible <= 1
                };
            bound StateIsBounded:
                context <= MaxContext && assist_visible <= 1 && interacting <= 1 &&
                document_edits <= MaxContext && ordinary_enter <= MaxContext &&
                caret <= MaxContext && ordinary_down <= MaxContext;
            invariant AcceptedWithinResults: accepted <= Results;
        }
    }
}

/// Bounded Manual problem-navigation lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn manual_config_problem_navigation_model() -> Model {
    aterm_spec::ty_model! {
        ManualConfigProblemNavigation {
            const Buggy = 0;
            const MaxProblems = 3;
            var problems = 0;
            var selected = 0;
            var target = 0;
            var caret_target = 0;
            var revealed = 0;
            var semantic_full = 0;
            var jumps = 0;

            action LoadOne when (problems == 0) {
                problems = 1;
                selected = 0;
            }
            action LoadThree when (problems == 0) {
                problems = 3;
                selected = 0;
            }
            action JumpNext when (problems > 0 && jumps <= 3) {
                selected = if problems == 1 {
                    if Buggy == 2 { 1 } else { 0 }
                } else {
                    if selected <= problems - 2 { selected + 1 } else { 0 }
                };
                target = if problems == 1 {
                    1
                } else {
                    if selected <= problems - 2 { selected + 2 } else { 1 }
                };
                caret_target = if Buggy == 1 {
                    caret_target
                } else {
                    if problems == 1 {
                        1
                    } else {
                        if selected <= problems - 2 { selected + 2 } else { 1 }
                    }
                };
                revealed = if Buggy == 1 { 0 } else { 1 };
                semantic_full = if Buggy == 1 { 0 } else { 1 };
                jumps = jumps + 1;
            }
            action JumpPrevious when (problems > 0 && jumps <= 3) {
                selected = if problems == 1 {
                    0
                } else {
                    if selected > 0 { selected - 1 } else { problems - 1 }
                };
                target = if problems == 1 {
                    1
                } else {
                    if selected > 0 { selected } else { problems }
                };
                caret_target = if Buggy == 1 {
                    caret_target
                } else {
                    if problems == 1 {
                        1
                    } else {
                        if selected > 0 { selected } else { problems }
                    }
                };
                revealed = if Buggy == 1 { 0 } else { 1 };
                semantic_full = if Buggy == 1 { 0 } else { 1 };
                jumps = jumps + 1;
            }
            action Settled when (jumps > 0) {
                jumps = jumps;
            }

            invariant SelectionWithinProblems:
                if problems > 0 { selected <= problems - 1 } else { selected == 0 };
            invariant JumpMovesToExactProblem:
                if jumps > 0 { target == selected + 1 && caret_target == target } else {
                    target == 0 && caret_target == 0
                };
            invariant JumpRevealsProblem:
                if jumps > 0 { revealed == 1 } else { revealed == 0 };
            invariant FullProblemIsSemantic:
                if jumps > 0 { semantic_full == 1 } else { semantic_full == 0 };
            bound StateIsBounded:
                problems <= MaxProblems && selected <= MaxProblems - 1 &&
                target <= MaxProblems && caret_target <= MaxProblems &&
                revealed <= 1 && semantic_full <= 1 && jumps <= 4;
        }
    }
}

/// Bounded Settings → Manual handoff.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn manual_config_handoff_model() -> Model {
    aterm_spec::ty_model! {
        ManualConfigHandoff {
            const Buggy = 0;
            const MaxRequests = 2;
            var requests = 0;
            var request_kind = 0;
            var outcome = 0;
            var selected_exact = 0;
            var added_exact = 0;
            var completion_ready = 0;
            var canonical_path_authority = 1;
            var editor_instances = 0;
            var focused = 0;

            action RevealAuthoredKey when (requests <= MaxRequests - 1) {
                requests = requests + 1;
                request_kind = 1;
                outcome = 1;
                selected_exact = if Buggy == 1 { 0 } else { 1 };
                added_exact = 0;
                completion_ready = 0;
                canonical_path_authority = if Buggy == 1 {
                    0
                } else { canonical_path_authority };
                editor_instances = if editor_instances == 0 {
                    1
                } else {
                    if Buggy == 1 { editor_instances + 1 } else { editor_instances }
                };
                focused = 1;
            }
            action AddAbsentKey when (requests <= MaxRequests - 1) {
                requests = requests + 1;
                request_kind = 2;
                outcome = 2;
                selected_exact = 0;
                added_exact = if Buggy == 1 { 0 } else { 1 };
                completion_ready = if Buggy == 1 { 0 } else { 1 };
                canonical_path_authority = if Buggy == 1 {
                    0
                } else { canonical_path_authority };
                editor_instances = if editor_instances == 0 {
                    1
                } else {
                    if Buggy == 1 { editor_instances + 1 } else { editor_instances }
                };
                focused = 1;
            }

            invariant HostOwnsCanonicalPath: canonical_path_authority == 1;
            invariant OneManualEditor: editor_instances <= 1;
            invariant EveryRequestFocused:
                if requests > 0 { focused == 1 && editor_instances == 1 } else {
                    focused == 0 && editor_instances == 0
                };
            invariant AuthoredTargetSelected:
                if request_kind == 1 {
                    outcome == 1 && selected_exact == 1 && added_exact == 0 &&
                    completion_ready == 0
                } else { selected_exact == 0 };
            invariant AbsentTargetAdded:
                if request_kind == 2 {
                    outcome == 2 && selected_exact == 0 && added_exact == 1 &&
                    completion_ready == 1
                } else { added_exact == 0 };
            invariant StateIsBounded:
                requests <= MaxRequests && request_kind <= 2 && outcome <= 2 &&
                selected_exact <= 1 && added_exact <= 1 && completion_ready <= 1 &&
                canonical_path_authority <= 1 && editor_instances <= 1 && focused <= 1;
        }
    }
}

/// Process-global Settings Packages worker lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_packages_worker_model() -> Model {
    aterm_spec::ty_model! {
        NativePackagesWorker {
            const Buggy = 0;
            const MaxSequence = 6;
            var sequence = 0;
            var inflight = 0;
            var operation = 0;
            var observed = 0;
            var last_operation = 0;
            var last_result = 0;
            var presented_result = 0;
            var expected_result = 0;

            action BeginRefresh when (inflight == 0 && sequence <= MaxSequence - 1) {
                sequence = sequence + 1;
                inflight = 1;
                operation = 1;
                observed = observed;
                last_operation = last_operation;
                last_result = last_result;
                presented_result = presented_result;
                expected_result = expected_result;
            }
            action BeginCheck when (inflight == 0 && sequence <= MaxSequence - 1) {
                sequence = sequence + 1;
                inflight = 1;
                operation = 2;
                observed = observed;
                last_operation = 0;
                last_result = 0;
                presented_result = 0;
                expected_result = 0;
            }
            action BeginInstall when (inflight == 0 && sequence <= MaxSequence - 1) {
                sequence = sequence + 1;
                inflight = 1;
                operation = 3;
                observed = observed;
                last_operation = 0;
                last_result = 0;
                presented_result = 0;
                expected_result = 0;
            }
            action FinishRefresh when (inflight == 1 && operation == 1) {
                sequence = sequence;
                inflight = 0;
                operation = 0;
                observed = 1;
                last_operation = if Buggy == 1 { 0 } else { last_operation };
                last_result = if Buggy == 1 { 0 } else { last_result };
                presented_result = if Buggy == 1 { 0 } else { presented_result };
                expected_result = expected_result;
            }
            action FinishCheckSuccess when (inflight == 1 && operation == 2) {
                sequence = sequence;
                inflight = 0;
                operation = 0;
                observed = 1;
                last_operation = 2;
                last_result = 1;
                presented_result = 1;
                expected_result = 1;
            }
            action FinishCheckFailure when (inflight == 1 && operation == 2) {
                sequence = sequence;
                inflight = 0;
                operation = 0;
                observed = 1;
                last_operation = 2;
                last_result = 2;
                presented_result = if Buggy == 1 { 1 } else { 2 };
                expected_result = 2;
            }
            action FinishInstallSuccess when (inflight == 1 && operation == 3) {
                sequence = sequence;
                inflight = 0;
                operation = 0;
                observed = 1;
                last_operation = 3;
                last_result = 1;
                presented_result = 1;
                expected_result = 1;
            }
            action FinishInstallFailure when (inflight == 1 && operation == 3) {
                sequence = sequence;
                inflight = 0;
                operation = 0;
                observed = 1;
                last_operation = 3;
                last_result = 2;
                presented_result = if Buggy == 1 { 1 } else { 2 };
                expected_result = 2;
            }
            action Abort when (inflight == 1) {
                sequence = sequence;
                inflight = 0;
                operation = if Buggy == 1 && operation > 1 { operation } else { 0 };
                observed = observed;
                last_operation = last_operation;
                last_result = last_result;
                presented_result = presented_result;
                expected_result = expected_result;
            }

            invariant SingleFlightHasOneKind:
                if inflight == 1 {
                    operation > 0 && operation <= 3
                } else { operation == 0 };
            invariant FinalResultIsPresented: presented_result == last_result;
            invariant RefreshKeepsVerbResult: last_result == expected_result;
            bound StateIsBounded:
                sequence <= MaxSequence && inflight <= 1 && operation <= 3 &&
                observed <= 1 && last_operation <= 3 && last_result <= 2 &&
                presented_result <= 2;
        }
    }
}

/// Recovery pagination and capability work are one bounded view lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_recovery_interaction_model() -> Model {
    aterm_spec::ty_model! {
        NativeRecoveryInteraction {
            const Buggy = 0;
            const MaxPage = 3;
            const MaxStarts = 2;
            const MaxCompletions = 3;
            var page = 0;
            var pending = 0;
            var inflight = 0;
            var starts = 0;
            var completions = 0;
            var last_completion_was_stale = 0;
            action NextPage when (page <= MaxPage - 1) {
                page = if Buggy == 1 { page + 2 } else { page + 1 };
                last_completion_was_stale = 0;
            }
            action PreviousPage when (page > 0) {
                page = page - 1;
                last_completion_was_stale = 0;
            }
            action BeginRetry when (
                starts <= MaxStarts - 1 && (pending == 0 || Buggy == 1)
            ) {
                pending = 1;
                inflight = inflight + 1;
                starts = starts + 1;
                last_completion_was_stale = 0;
            }
            action BeginCopy when (
                starts <= MaxStarts - 1 && (pending == 0 || Buggy == 1)
            ) {
                pending = 2;
                inflight = inflight + 1;
                starts = starts + 1;
                last_completion_was_stale = 0;
            }
            action MatchingComplete when (
                pending > 0 && completions <= MaxCompletions - 1
            ) {
                pending = 0;
                inflight = 0;
                completions = completions + 1;
                last_completion_was_stale = 0;
            }
            action StaleComplete when (
                pending > 0 && completions <= MaxCompletions - 1
            ) {
                pending = if Buggy == 1 { 0 } else { pending };
                inflight = if Buggy == 1 { 0 } else { inflight };
                completions = completions + 1;
                last_completion_was_stale = 1;
            }
            invariant PageBounded: page <= MaxPage;
            invariant SingleCapabilityFlight: inflight <= 1;
            invariant PendingMatchesFlight:
                if pending == 0 { inflight == 0 } else { inflight == 1 };
            invariant StaleCannotClear:
                if last_completion_was_stale == 1 { pending > 0 } else { inflight <= 1 };
            bound StartsBounded: starts <= MaxStarts;
            bound CompletionsBounded: completions <= MaxCompletions;
        }
    }
}

/// Bounded editor mark/minibuffer lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_editor_modal_model() -> Model {
    aterm_spec::ty_model! {
        NativeEditorModal {
            const Buggy = 0;
            const Cap = 3;
            var mode = 0;
            var query = 0;
            var caret = 0;
            var anchor = 0;
            var mark_origin = 0;
            var mark_active = 0;
            var search_origin = 0;
            var document_edits = 0;
            var authorized_edits = 0;
            var last_exit = 0;
            var prompt_base = 0;
            action SetMark when (mode == 0) {
                anchor = caret;
                mark_origin = caret;
                mark_active = 1;
            }
            action Move when (mode == 0 && caret <= Cap - 1) {
                caret = caret + 1;
                anchor = if mark_active == 1 {
                    if Buggy == 1 { caret + 1 } else { anchor }
                } else {
                    caret + 1
                };
            }
            action KillRegion when (
                mode == 0 && mark_active == 1 &&
                caret > anchor &&
                document_edits <= Cap - 1
            ) {
                document_edits = document_edits + 1;
                authorized_edits = authorized_edits + 1;
                caret = anchor;
                mark_active = 0;
            }
            action OpenCommand when (mode == 0) {
                mode = 1;
                query = 0;
                last_exit = 0;
            }
            action OpenSearch when (mode == 0) {
                mode = 2;
                query = 0;
                search_origin = caret;
                mark_active = 0;
                last_exit = 0;
            }
            action OpenBuffer when (mode == 0) {
                mode = 3;
                query = 0;
                last_exit = 0;
            }
            action OpenGoto when (mode == 0) {
                mode = 4;
                query = 0;
                search_origin = caret;
                mark_active = 0;
                last_exit = 0;
            }
            action OpenPrompt when (mode == 0) {
                mode = 5;
                query = 0;
                prompt_base = document_edits;
                last_exit = 0;
            }
            action MinibufferType when (
                mode > 0 && query <= Cap - 1 && document_edits <= Cap - 1
            ) {
                query = query + 1;
                caret = if mode == 2 {
                    if search_origin + query <= Cap - 1 {
                        search_origin + query + 1
                    } else if Buggy == 1 {
                        Cap + 1
                    } else {
                        Cap
                    }
                } else {
                    caret
                };
                document_edits = if Buggy == 1 {
                    document_edits + 1
                } else if Buggy == 2 && mode == 5 {
                    document_edits + 1
                } else {
                    document_edits
                };
                authorized_edits = if Buggy == 2 && mode == 5 {
                    authorized_edits + 1
                } else {
                    authorized_edits
                };
            }
            action MinibufferBackspace when (mode > 0 && query > 0) {
                query = query - 1;
                caret = if mode == 2 {
                    if query == 1 { search_origin } else { caret - 1 }
                } else {
                    caret
                };
            }
            action Submit when (mode > 0 && mode <= 3) {
                mode = 0;
                query = 0;
                last_exit = 0;
            }
            action SubmitGoto when (mode == 4 && query > 0) {
                mode = 0;
                caret = query;
                anchor = query;
                query = 0;
                last_exit = 0;
            }
            action AbortSearch when (mode == 2) {
                mode = 0;
                query = 0;
                caret = search_origin;
                anchor = search_origin;
                mark_active = 0;
                last_exit = 1;
            }
            action AbortCommand when (mode == 1) {
                mode = 0;
                query = 0;
                mark_active = 0;
                last_exit = 1;
            }
            action AbortBuffer when (mode == 3) {
                mode = 0;
                query = 0;
                mark_active = 0;
                last_exit = 1;
            }
            action AbortGoto when (mode == 4) {
                mode = 0;
                query = 0;
                caret = search_origin;
                anchor = search_origin;
                mark_active = 0;
                last_exit = 1;
            }
            action SubmitPrompt when (mode == 5) {
                mode = 0;
                query = 0;
                last_exit = 0;
            }
            action AbortPrompt when (mode == 5) {
                mode = 0;
                query = 0;
                mark_active = 0;
                last_exit = 1;
            }
            bound ModeBounded: mode <= 5;
            bound QueryBounded: query <= Cap;
            invariant CaretBounded: caret <= Cap;
            invariant AnchorBounded: anchor <= Cap;
            invariant MarkPinned:
                if mark_active == 1 { anchor == mark_origin } else { anchor <= Cap };
            invariant MinibufferCannotEditDocument: document_edits == authorized_edits;
            invariant PromptNeverEditsDocument:
                if mode == 5 { document_edits == prompt_base } else { prompt_base <= Cap };
            bound DocumentEditsBounded: document_edits <= Cap;
            bound ExitKindBounded: last_exit <= 1;
        }
    }
}

/// Native Settings activation is process-singleton while its implicit view is window-local.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_settings_singleton_model() -> Model {
    aterm_spec::ty_model! {
        NativeSettingsSingleton {
            const Buggy = 0;
            const MaxOpens = 4;
            var opens = 0;
            var settings_instances = 0;
            var window_one_implicit = 0;
            var window_two_implicit = 0;
            var requesting_window = 0;
            var focused_window = 0;
            action OpenOne when (opens <= MaxOpens - 1) {
                opens = opens + 1;
                settings_instances = if settings_instances == 0 {
                    1
                } else {
                    if Buggy == 1 { settings_instances + 1 } else { settings_instances }
                };
                window_one_implicit = if window_one_implicit == 0 {
                    1
                } else {
                    if Buggy == 1 { window_one_implicit + 1 } else { window_one_implicit }
                };
                requesting_window = 1;
                focused_window = 1;
            }
            action OpenTwo when (opens <= MaxOpens - 1) {
                opens = opens + 1;
                settings_instances = if settings_instances == 0 {
                    1
                } else {
                    if Buggy == 1 { settings_instances + 1 } else { settings_instances }
                };
                window_two_implicit = if window_two_implicit == 0 {
                    1
                } else {
                    if Buggy == 1 { window_two_implicit + 1 } else { window_two_implicit }
                };
                requesting_window = 2;
                focused_window = if Buggy == 1 && window_one_implicit > 0 { 1 } else { 2 };
            }
            invariant SingletonInstance: settings_instances <= 1;
            invariant OneImplicitViewWindowOne: window_one_implicit <= 1;
            invariant OneImplicitViewWindowTwo: window_two_implicit <= 1;
            invariant RequestingWindowFocused:
                if requesting_window == 0 {
                    focused_window == 0
                } else {
                    focused_window == requesting_window
                };
            bound OpensBounded: opens <= MaxOpens;
        }
    }
}

/// Native Settings keeps text drafts across navigation/publication, blocks every close scope while a draft exists, and keeps explicit recovery visible.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_settings_draft_close_model() -> Model {
    aterm_spec::ty_model! {
        NativeSettingsDraftClose {
            const Buggy = 0;
            const MaxPreservations = 2;
            var draft = 0;
            var discard_armed = 0;
            var close_result = 0;
            var recovery_visible = 0;
            var preservations = 0;
            action Edit when (draft == 0) {
                draft = 1;
                discard_armed = 0;
                close_result = 0;
                recovery_visible = 1;
                preservations = 0;
            }
            action PreserveDraft when (
                draft == 1 && preservations <= MaxPreservations - 1
            ) {
                draft = draft;
                discard_armed = 0;
                close_result = 0;
                recovery_visible = 1;
                preservations = preservations + 1;
            }
            action AttemptDirtyClose when (draft == 1) {
                draft = draft;
                close_result = if Buggy == 1 { 2 } else { 1 };
                recovery_visible = if Buggy == 1 { 0 } else { 1 };
            }
            action ArmDiscard when (draft == 1 && discard_armed == 0) {
                draft = if Buggy == 1 { 0 } else { draft };
                discard_armed = 1;
                close_result = 0;
                recovery_visible = if Buggy == 1 { 0 } else { 1 };
            }
            action CancelDiscard when (draft == 1 && discard_armed == 1) {
                discard_armed = 0;
                close_result = 0;
                recovery_visible = 1;
            }
            action ConfirmDiscard when (draft == 1 && discard_armed == 1) {
                draft = 0;
                discard_armed = 0;
                close_result = 0;
                recovery_visible = 0;
            }
            action SaveDraft when (draft == 1) {
                draft = 0;
                discard_armed = 0;
                close_result = 0;
                recovery_visible = 0;
            }
            action AttemptCleanClose when (draft == 0) {
                close_result = 2;
                recovery_visible = 0;
            }
            invariant DirtyNeverReady:
                if draft == 1 { close_result <= 1 } else { close_result <= 2 };
            invariant DirtyRecoveryVisible:
                if draft == 1 { recovery_visible == 1 } else { recovery_visible == 0 };
            invariant ConfirmationOwnsDraft:
                if discard_armed == 1 { draft == 1 } else { draft <= 1 };
            bound FlagsBounded:
                draft <= 1 && discard_armed <= 1 && recovery_visible <= 1;
            bound ResultBounded: close_result <= 2;
            bound PreservationBounded: preservations <= MaxPreservations;
        }
    }
}

/// Versioned preference patches use touched-key expectations rather than blind whole-file overwrite.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_config_transaction_model() -> Model {
    aterm_spec::ty_model! {
        NativeConfigTransaction {
            const Buggy = 0;
            const MaxRevision = 7;
            var revision = 0;
            var key_a = 1;
            var key_b = 1;
            var patch_active = 0;
            var expected_a = 0;
            var undo_ready = 0;
            var undo_before_a = 0;
            var undo_expected_a = 0;
            var accepted = 0;
            var external_edits = 0;
            var stale_overwrite = 0;
            var partial_reset = 0;
            action BeginPatchA when (patch_active == 0) {
                patch_active = 1;
                expected_a = key_a;
            }
            action ExternalAFromOne when (
                key_a == 1 && revision <= MaxRevision - 1
            ) {
                key_a = 2;
                revision = revision + 1;
                external_edits = external_edits + 1;
            }
            action ExternalAFromZero when (
                key_a == 0 && revision <= MaxRevision - 1
            ) {
                key_a = 2;
                revision = revision + 1;
                external_edits = external_edits + 1;
            }
            action ExternalB when (
                key_b == 1 && revision <= MaxRevision - 1
            ) {
                key_b = 2;
                revision = revision + 1;
                external_edits = external_edits + 1;
            }
            action CommitPatchA when (
                patch_active == 1 && expected_a <= key_a &&
                key_a <= if Buggy == 1 { 2 } else { expected_a } &&
                revision <= MaxRevision - 1 && accepted <= MaxRevision - 1
            ) {
                key_a = 0;
                patch_active = 0;
                revision = if Buggy == 1 { revision } else { revision + 1 };
                undo_ready = if Buggy == 1 && key_a > expected_a {
                    undo_ready
                } else { 1 };
                undo_before_a = if Buggy == 1 && key_a > expected_a {
                    undo_before_a
                } else { expected_a };
                undo_expected_a = if Buggy == 1 && key_a > expected_a {
                    undo_expected_a
                } else { 0 };
                accepted = accepted + 1;
                stale_overwrite = if Buggy == 1 && key_a > expected_a {
                    1
                } else { stale_overwrite };
            }
            action RejectPatchConflict when (
                patch_active == 1 && key_a > expected_a
            ) {
                patch_active = 0;
            }
            action UndoPatchA when (
                undo_ready == 1 && undo_expected_a <= key_a &&
                key_a <= if Buggy == 1 { 2 } else { undo_expected_a } &&
                revision <= MaxRevision - 1 && accepted <= MaxRevision - 1
            ) {
                key_a = undo_before_a;
                undo_ready = 0;
                revision = if Buggy == 1 { revision } else { revision + 1 };
                accepted = accepted + 1;
                stale_overwrite = if Buggy == 1 && key_a > undo_expected_a {
                    1
                } else { stale_overwrite };
            }
            action RejectUndoConflict when (
                undo_ready == 1 && key_a > undo_expected_a
            ) {
                undo_ready = 0;
            }
            action ResetAll when (
                patch_active == 0 && revision <= MaxRevision - 1 &&
                accepted <= MaxRevision - 1
            ) {
                key_a = 0;
                key_b = if Buggy == 1 { key_b } else { 0 };
                revision = if Buggy == 1 { revision } else { revision + 1 };
                undo_ready = if Buggy == 1 { undo_ready } else { 0 };
                accepted = accepted + 1;
                partial_reset = if Buggy == 1 { 1 } else { partial_reset };
            }
            invariant NoBlindOverwrite: stale_overwrite == 0;
            invariant AtomicResetVisibility: partial_reset == 0;
            invariant RevisionCountsWrites: accepted + external_edits == revision;
            bound KeysBounded: key_a <= 2 && key_b <= 2;
            bound RevisionBounded: revision <= MaxRevision;
        }
    }
}

/// Exact config observations cross a serialized worker/event-loop handoff.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_config_observation_handoff_model() -> Model {
    aterm_spec::ty_model! {
        NativeConfigObservationHandoff {
            const Buggy = 0;
            var phase = 0;
            var pending = 0;
            var sampled = 0;
            var gate = 0;
            var queued = 0;
            var admitted = 0;
            var reconciliation_failed = 0;
            var dropped_candidate = 0;
            var blind_write = 0;
            var stale_admission = 0;

            action QueueWrite when (queued == 0) {
                queued = 1;
            }
            action BeginWrite when (
                phase == 0 && gate == 0 && pending == 0
            ) {
                phase = 1;
            }
            action StartQueuedWrite when (
                phase == 0 && queued == 1 && gate == 0 && pending == 0
            ) {
                phase = 1;
                queued = 0;
            }
            action ObserveFirst when (pending == 0) {
                pending = 1;
                gate = 1;
            }
            action ObserveNewer when (pending == 1) {
                pending = 2;
                gate = 1;
            }
            action FinishWrite when (phase == 1) {
                phase = 0;
                gate = if pending > 0 { 1 } else { gate };
            }
            action StartReconcile when (
                phase == 0 && gate == 1 && pending > 0
            ) {
                phase = 2;
                sampled = pending;
                reconciliation_failed = 0;
            }
            action FailReconcile when (phase == 2) {
                phase = 0;
                pending = if Buggy == 1 { 0 } else { pending };
                sampled = 0;
                reconciliation_failed = 1;
                queued = 0;
                dropped_candidate = if Buggy == 1 { 1 } else { dropped_candidate };
                queued = 0;
            }
            action RetryReconcile when (
                phase == 0 && gate == 1 && pending > 0 &&
                reconciliation_failed == 1
            ) {
                phase = 2;
                sampled = pending;
                reconciliation_failed = 0;
            }
            action ResampleNewer when (
                phase == 2 && pending > sampled
            ) {
                sampled = pending;
            }
            action AdmitExact when (
                phase == 2 && pending == sampled
            ) {
                phase = 0;
                admitted = pending;
                pending = 0;
                sampled = 0;
                gate = 0;
                reconciliation_failed = 0;
            }
            action StartBlindWrite when (
                Buggy == 1 && phase == 0 && queued == 1 &&
                (gate == 1 || pending > 0)
            ) {
                phase = 1;
                queued = 0;
                blind_write = 1;
            }
            action AdmitStaleSample when (
                Buggy == 1 && phase == 2 && pending > sampled
            ) {
                phase = 0;
                admitted = sampled;
                pending = 0;
                sampled = 0;
                gate = 0;
                stale_admission = 1;
            }

            invariant DeferredGenerationNeverLost: dropped_candidate == 0;
            invariant UnknownAuthorityFencesWrites: blind_write == 0;
            invariant LatestExactGenerationWins: stale_admission == 0;
            bound Bounds:
                phase <= 2 && pending <= 2 && sampled <= 2 && admitted <= 2 &&
                gate <= 1 && queued <= 1 && reconciliation_failed <= 1 &&
                dropped_candidate <= 1 && blind_write <= 1 && stale_admission <= 1;
        }
    }
}

/// Process-global Serious Mode commands keep semantic desired values while another config write is in flight.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn serious_mode_intent_queue_model() -> Model {
    aterm_spec::ty_model! {
        SeriousModeIntentQueue {
            const Buggy = 0;
            const MaxIssued = 3;
            var live = 0;
            var service = 0;
            var projection = 0;
            var inflight = 0;
            var current_desired = 0;
            var queue_count = 0;
            var q1 = 0;
            var q2 = 0;
            var q1_expected = 0;
            var q2_expected = 0;
            var issued = 0;
            var completed = 0;
            var conflict = 0;
            var last_desired = 0;
            var intent_kind = 0;
            action StartToggle when (
                inflight == 0 && issued <= MaxIssued - 1
            ) {
                current_desired = if projection == 0 { 1 } else { 0 };
                service = if projection == 0 { 1 } else { 0 };
                projection = if projection == 0 { 1 } else { 0 };
                inflight = 1;
                issued = issued + 1;
                last_desired = if projection == 0 { 1 } else { 0 };
                intent_kind = 1;
            }
            action QueueToggle when (
                inflight == 1 && queue_count <= 1 && issued <= MaxIssued - 1
            ) {
                q1 = if queue_count == 0 {
                    if projection == 0 { 1 } else { 0 }
                } else { q1 };
                q2 = if queue_count == 1 {
                    if projection == 0 { 1 } else { 0 }
                } else { q2 };
                q1_expected = if queue_count == 0 { service } else { q1_expected };
                q2_expected = if queue_count == 1 { service } else { q2_expected };
                queue_count = queue_count + 1;
                projection = if projection == 0 { 1 } else { 0 };
                issued = issued + 1;
                last_desired = if projection == 0 { 1 } else { 0 };
                intent_kind = 1;
            }
            action StartSetOn when (
                inflight == 0 && issued <= MaxIssued - 1 && projection == 0
            ) {
                current_desired = 1;
                service = 1;
                projection = 1;
                inflight = 1;
                issued = issued + 1;
                last_desired = 1;
                intent_kind = 2;
            }
            action StartSetOff when (
                inflight == 0 && issued <= MaxIssued - 1 && projection == 1
            ) {
                current_desired = 0;
                service = 0;
                projection = 0;
                inflight = 1;
                issued = issued + 1;
                last_desired = 0;
                intent_kind = 2;
            }
            action QueueSetOn when (
                inflight == 1 && queue_count <= 1 && issued <= MaxIssued - 1
            ) {
                q1 = if queue_count == 0 { 1 } else { q1 };
                q2 = if queue_count == 1 { 1 } else { q2 };
                q1_expected = if queue_count == 0 { service } else { q1_expected };
                q2_expected = if queue_count == 1 { service } else { q2_expected };
                queue_count = queue_count + 1;
                projection = 1;
                issued = issued + 1;
                last_desired = 1;
                intent_kind = 2;
            }
            action QueueSetOff when (
                inflight == 1 && queue_count <= 1 && issued <= MaxIssued - 1
            ) {
                q1 = if queue_count == 0 { 0 } else { q1 };
                q2 = if queue_count == 1 { 0 } else { q2 };
                q1_expected = if queue_count == 0 { service } else { q1_expected };
                q2_expected = if queue_count == 1 { service } else { q2_expected };
                queue_count = queue_count + 1;
                projection = 0;
                issued = issued + 1;
                last_desired = 0;
                intent_kind = 2;
            }
            action Complete when (inflight == 1) {
                live = current_desired;
                service = if queue_count == 0 {
                    service
                } else {
                    if Buggy == 1 && (q1_expected > service || service > q1_expected) {
                        service
                    } else { q1 }
                };
                current_desired = if queue_count == 0 { current_desired } else { q1 };
                inflight = if queue_count == 0 {
                    0
                } else {
                    if Buggy == 1 && (q1_expected > service || service > q1_expected) {
                        0
                    } else { 1 }
                };
                q1 = if queue_count <= 1 { 0 } else { q2 };
                q2 = 0;
                q1_expected = if queue_count <= 1 { 0 } else { q2_expected };
                q2_expected = 0;
                queue_count = if queue_count == 0 { 0 } else { queue_count - 1 };
                projection = if queue_count == 0 {
                    current_desired
                } else {
                    if Buggy == 1 { q1 } else { projection }
                };
                completed = completed + 1;
                conflict = if (
                    queue_count > 0 && Buggy == 1 &&
                    (q1_expected > service || service > q1_expected)
                ) { 1 } else { conflict };
            }
            invariant NoSerializedConflict: conflict == 0;
            invariant IdleIsAuthoritative:
                if inflight == 0 { live == service && live == projection } else { live <= 1 };
            invariant ProjectionTracksLatestIntent: projection == last_desired;
            bound QueueBounded: queue_count <= 2;
            bound IssuedBounded: issued <= MaxIssued;
            bound ValuesBoolean:
                live <= 1 && service <= 1 && projection <= 1 && current_desired <= 1 &&
                q1 <= 1 && q2 <= 1 && q1_expected <= 1 && q2_expected <= 1 &&
                last_desired <= 1 && intent_kind <= 2;
        }
    }
}

/// Filesystem commit authority shared by Manual and structured Settings.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn config_file_commit_cas_model() -> Model {
    aterm_spec::ty_model! {
        ConfigFileCommitCas {
            const Buggy = 0;
            var disk = 0;
            var service = 0;
            var target = 0;
            var link = 0;
            var manual_phase = 0;
            var settings_phase = 0;
            var manual_base = 0;
            var settings_base = 0;
            var manual_target = 0;
            var settings_target = 0;
            var manual_link = 0;
            var settings_link = 0;
            var manual_symlink = 0;
            var settings_symlink = 0;
            var lock_owner = 0;
            var manual_committed = 0;
            var settings_committed = 0;
            var double_winner = 0;
            var stale_publication = 0;
            var split_target_commit = 0;
            var symlink_publication = 0;
            var manual_unsynchronized = 0;
            var manual_indeterminate = 0;
            var settings_indeterminate = 0;
            var blind_retry = 0;

            action BeginManual when (manual_phase == 0) {
                manual_phase = 1;
                manual_base = disk;
                manual_target = target;
                manual_link = link;
            }
            action BeginSettings when (settings_phase == 0) {
                settings_phase = 1;
                settings_base = disk;
                settings_target = target;
                settings_link = link;
            }
            action BeginManualSymlink when (manual_phase == 0) {
                manual_phase = 1;
                manual_base = disk;
                manual_target = target;
                manual_link = link;
                manual_symlink = 1;
            }
            action BeginSettingsSymlink when (settings_phase == 0) {
                settings_phase = 1;
                settings_base = disk;
                settings_target = target;
                settings_link = link;
                settings_symlink = 1;
            }
            action Retarget when (target == 0) {
                target = 1;
                link = 1;
            }
            action Relink when (link == 0) {
                link = 1;
            }
            action LockManual when (manual_phase == 1 && lock_owner == 0) {
                manual_phase = 2;
                lock_owner = 1;
            }
            action LockSettings when (settings_phase == 1 && lock_owner == 0) {
                settings_phase = 2;
                lock_owner = 2;
            }
            action ResolveManual when (manual_phase == 2 && lock_owner == 1) {
                disk = if (
                    manual_base == disk && manual_target == target &&
                    manual_link == link
                ) { 1 } else { if Buggy == 1 { 1 } else { disk } };
                service = if (
                    manual_base == disk && manual_target == target &&
                    manual_link == link
                ) { if Buggy == 1 { service } else { 1 } } else { service };
                manual_committed = if (
                    (manual_base == disk && manual_target == target &&
                     manual_link == link) || Buggy == 1
                ) { 1 } else { manual_committed };
                double_winner = if (
                    Buggy == 1 && settings_committed == 1 &&
                    manual_base == settings_base
                ) { 1 } else { double_winner };
                stale_publication = if (
                    Buggy == 1 && disk > manual_base
                ) { 1 } else { stale_publication };
                split_target_commit = if (
                    Buggy == 1 && target > manual_target
                ) { 1 } else { split_target_commit };
                symlink_publication = if (
                    Buggy == 1 && link > manual_link
                ) { 1 } else { symlink_publication };
                manual_unsynchronized = if (
                    Buggy == 1 && manual_base == disk && manual_target == target &&
                    manual_link == link
                ) { 1 } else { manual_unsynchronized };
                manual_phase = if (
                    (manual_base == disk && manual_target == target &&
                     manual_link == link) || Buggy == 1
                ) { 3 } else { 4 };
                lock_owner = 0;
            }
            action ResolveSettings when (
                settings_phase == 2 && lock_owner == 2
            ) {
                disk = if (
                    settings_base == disk && settings_target == target &&
                    settings_link == link
                ) { 2 } else { if Buggy == 1 { 2 } else { disk } };
                service = if (
                    (settings_base == disk && settings_target == target &&
                     settings_link == link) || Buggy == 1
                ) { 2 } else { service };
                settings_committed = if (
                    (settings_base == disk && settings_target == target &&
                     settings_link == link) || Buggy == 1
                ) { 1 } else { settings_committed };
                double_winner = if (
                    Buggy == 1 && manual_committed == 1 &&
                    settings_base == manual_base
                ) { 1 } else { double_winner };
                stale_publication = if (
                    Buggy == 1 && disk > settings_base
                ) { 1 } else { stale_publication };
                split_target_commit = if (
                    Buggy == 1 && target > settings_target
                ) { 1 } else { split_target_commit };
                symlink_publication = if (
                    Buggy == 1 && link > settings_link
                ) { 1 } else { symlink_publication };
                settings_phase = if (
                    (settings_base == disk && settings_target == target &&
                     settings_link == link) || Buggy == 1
                ) { 3 } else { 4 };
                lock_owner = 0;
            }
            action ResolveManualIndeterminate when (
                manual_phase == 2 && lock_owner == 1 &&
                manual_base == disk && manual_target == target &&
                manual_link == link
            ) {
                disk = 1;
                manual_phase = 5;
                manual_indeterminate = 1;
                manual_committed = if Buggy == 1 { 1 } else { manual_committed };
                lock_owner = 0;
            }
            action ResolveSettingsIndeterminate when (
                settings_phase == 2 && lock_owner == 2 &&
                settings_base == disk && settings_target == target &&
                settings_link == link
            ) {
                disk = 2;
                settings_phase = 5;
                settings_indeterminate = 1;
                lock_owner = 0;
            }
            action ReconcileManual when (manual_phase == 5) {
                service = disk;
                manual_phase = 4;
                manual_indeterminate = 0;
            }
            action ReconcileSettings when (settings_phase == 5) {
                service = disk;
                settings_phase = 4;
                settings_indeterminate = 0;
            }
            action RetryIndeterminate when (
                manual_phase == 5 || settings_phase == 5
            ) {
                blind_retry = if Buggy == 1 { 1 } else { blind_retry };
            }

            invariant SameBaselineHasOneWinner: double_winner == 0;
            invariant NoStalePublication: stale_publication == 0;
            invariant NoSplitTargetCommit: split_target_commit == 0;
            invariant NoChangedLinkPublication: symlink_publication == 0;
            invariant ManualDurableSynchronizesImmediately:
                manual_unsynchronized == 0 &&
                if manual_phase == 3 && settings_phase == 5 {
                    service <= 2
                } else {
                    if manual_phase == 3 { service == disk } else { service <= 2 }
                };
            invariant IndeterminateDoesNotClaimDurability:
                if manual_phase == 5 {
                    manual_committed == 0 && manual_indeterminate == 1
                } else {
                    if settings_phase == 5 {
                        settings_committed == 0 && settings_indeterminate == 1
                    } else { manual_indeterminate + settings_indeterminate == 0 }
                };
            invariant ReconcileBeforeRetry: blind_retry == 0;
            bound Bounded:
                lock_owner <= 2 &&
                disk <= 2 && service <= 2 && target <= 1 && link <= 1 &&
                manual_phase <= 5 && settings_phase <= 5 &&
                manual_link <= 1 && settings_link <= 1 &&
                manual_symlink <= 1 && settings_symlink <= 1 &&
                manual_committed <= 1 && settings_committed <= 1 &&
                symlink_publication <= 1 && manual_indeterminate <= 1 &&
                settings_indeterminate <= 1 && blind_retry <= 1;
        }
    }
}

/// A composite accessibility tree publishes independent route ownership for two visible native views.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn composite_accessibility_route_model() -> Model {
    aterm_spec::ty_model! {
        CompositeAccessibilityRoute {
            const Buggy = 0;
            var owner_one_generation = 1;
            var owner_two_generation = 1;
            var published_one_generation = 0;
            var published_two_generation = 0;
            var focus_owner = 2;
            var pending = 0;
            var target_owner = 0;
            var target_generation = 0;
            var dispatched_owner = 0;
            var dispatched_generation = 0;
            var cross_dispatch = 0;
            var stale_dispatch = 0;
            action Publish when (
                published_one_generation == 0 && published_two_generation == 0
            ) {
                published_one_generation = owner_one_generation;
                published_two_generation = owner_two_generation;
            }
            action AdvanceOne when (owner_one_generation == 1) {
                owner_one_generation = 2;
            }
            action AdvanceTwo when (owner_two_generation == 1) {
                owner_two_generation = 2;
            }
            action RequestOne when (
                published_one_generation > 0 && pending == 0 && dispatched_owner == 0
            ) {
                pending = 1;
                target_owner = 1;
                target_generation = published_one_generation;
            }
            action RequestTwo when (
                published_two_generation > 0 && pending == 0 && dispatched_owner == 0
            ) {
                pending = 1;
                target_owner = 2;
                target_generation = published_two_generation;
            }
            action Route when (
                pending == 1 && (
                    target_generation == if target_owner == 1 {
                        owner_one_generation
                    } else {
                        owner_two_generation
                    } || Buggy == 1
                )
            ) {
                pending = 0;
                dispatched_owner = if Buggy == 1 { focus_owner } else { target_owner };
                dispatched_generation = target_generation;
                focus_owner = if Buggy == 1 { focus_owner } else { target_owner };
                cross_dispatch = if Buggy == 1 {
                    if focus_owner == target_owner { 0 } else { 1 }
                } else { 0 };
                stale_dispatch = if target_owner == 1 {
                    if target_generation == owner_one_generation { 0 } else { 1 }
                } else {
                    if target_generation == owner_two_generation { 0 } else { 1 }
                };
            }
            action RejectStale when (
                pending == 1 && (
                    target_owner == 1 && owner_one_generation > target_generation ||
                    target_owner == 2 && owner_two_generation > target_generation
                )
            ) {
                pending = 0;
            }
            invariant NoCrossViewDispatch: cross_dispatch == 0;
            invariant NoStaleGenerationDispatch: stale_dispatch == 0;
            bound GenerationsBounded:
                owner_one_generation <= 2 && owner_two_generation <= 2 &&
                published_one_generation <= 2 && published_two_generation <= 2;
            bound OwnerDomain:
                focus_owner <= 2 && target_owner <= 2 && dispatched_owner <= 2;
        }
    }
}

/// Two controllers share one document sequence.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_document_publication_model() -> Model {
    aterm_spec::ty_model! {
        NativeDocumentPublication {
            const Buggy = 0;
            const MaxSeq = 4;
            var edit_seq = 0;
            var snapshot_seq = 0;
            var editor_seen = 0;
            var markdown_seen = 0;
            var anchor_seq = 0;
            var txn_active = 0;
            var txn_base = 0;
            var stale_write = 0;
            var partial_publish = 0;
            action BeginTxn when (txn_active == 0) {
                txn_active = 1;
                txn_base = edit_seq;
            }
            action OtherCommit when (edit_seq <= MaxSeq - 1) {
                edit_seq = edit_seq + 1;
                snapshot_seq = if Buggy == 1 { snapshot_seq } else { snapshot_seq + 1 };
                editor_seen = if Buggy == 1 { editor_seen } else { editor_seen + 1 };
                markdown_seen = markdown_seen + 1;
                anchor_seq = if Buggy == 1 { anchor_seq } else { anchor_seq + 1 };
                partial_publish = if Buggy == 1 { 1 } else { partial_publish };
            }
            action CommitClean when (
                txn_active == 1 && txn_base <= edit_seq &&
                edit_seq <= if Buggy == 1 { MaxSeq - 1 } else { txn_base } &&
                edit_seq <= MaxSeq - 1
            ) {
                edit_seq = edit_seq + 1;
                snapshot_seq = if Buggy == 1 && txn_base == edit_seq {
                    snapshot_seq
                } else {
                    snapshot_seq + 1
                };
                editor_seen = editor_seen + 1;
                markdown_seen = if Buggy == 1 && txn_base == edit_seq {
                    markdown_seen
                } else {
                    markdown_seen + 1
                };
                anchor_seq = anchor_seq + 1;
                txn_active = 0;
                stale_write = if Buggy == 1 && edit_seq > txn_base {
                    1
                } else { stale_write };
                partial_publish = if Buggy == 1 && txn_base == edit_seq {
                    1
                } else { partial_publish };
            }
            action RejectStale when (
                txn_active == 1 && edit_seq > txn_base
            ) {
                txn_active = 0;
            }
            invariant SnapshotCurrent: snapshot_seq == edit_seq;
            invariant EditorCurrent: editor_seen == edit_seq;
            invariant MarkdownCurrent: markdown_seen == edit_seq;
            invariant AnchorsTransformed: anchor_seq == edit_seq;
            invariant StaleTxnIsNoOp: stale_write == 0;
            invariant PublishIsAtomic: partial_publish == 0;
            bound SequenceBounded: edit_seq <= MaxSeq;
        }
    }
}

/// File-watch observations are a five-way decision with strict precedence: an in-flight save defers every changed observation; otherwise byte-equivalent generations rebind only the baseline; otherwise dirty local bytes surface a conflict and clean bytes reload atomically.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_file_watch_model() -> Model {
    aterm_spec::ty_model! {
        NativeFileWatch {
            const Buggy = 0;
            var changed = 0;
            var equivalent = 0;
            var dirty = 0;
            var saving = 0;
            var verdict = 0;
            action ObserveChange when (changed == 0 && verdict == 0) {
                changed = 1;
            }
            action MarkDirty when (dirty == 0 && verdict == 0) {
                dirty = 1;
            }
            action MarkEquivalent when (equivalent == 0 && verdict == 0) {
                equivalent = 1;
            }
            action BeginSave when (saving == 0 && verdict == 0) {
                saving = 1;
            }
            action Resolve when (verdict == 0) {
                verdict = if changed == 0 {
                    1
                } else if Buggy == 1 && dirty == 1 {
                    3
                } else if saving == 1 {
                    4
                } else if equivalent == 1 {
                    5
                } else if dirty == 1 {
                    3
                } else {
                    2
                };
            }
            invariant PriorityIsDeterministic:
                if verdict > 0 {
                    verdict == if changed == 0 {
                        1
                    } else if saving == 1 {
                        4
                    } else if equivalent == 1 {
                        5
                    } else if dirty == 1 {
                        3
                    } else {
                        2
                    }
                } else {
                    verdict == 0
                };
            bound InputsBounded: changed <= 1 && equivalent <= 1 && dirty <= 1 && saving <= 1;
            bound VerdictBounded: verdict <= 5;
        }
    }
}

/// Failure/recovery protocol shared by the live config and theme watchers.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn watcher_failure_recovery_model() -> Model {
    aterm_spec::ty_model! {
        WatcherFailureRecovery {
            const Buggy = 0;
            const MaxEpoch = 2;
            var failed = 0;
            var status_failed = 0;
            var failure_epochs = 0;
            var failure_wakes = 0;
            var recovery_wakes = 0;
            var catalog = 0;
            var latched_catalog = 0;
            var latest_candidate = 0;
            var pending_candidate = 0;
            var recovered_candidate = 0;
            action ObserveFailure when (
                failed == 0 && failure_epochs <= MaxEpoch - 1
            ) {
                failed = 1;
                status_failed = 1;
                failure_epochs = failure_epochs + 1;
                failure_wakes = failure_wakes + 1;
                latched_catalog = catalog;
                latest_candidate = 0;
                pending_candidate = 0;
                recovered_candidate = 0;
            }
            action RepeatFailure when (failed == 1) {
                status_failed = if Buggy == 1 { 0 } else { status_failed };
                failure_wakes = if Buggy == 1 {
                    failure_wakes + 1
                } else {
                    failure_wakes
                };
                catalog = if Buggy == 1 {
                    if catalog == MaxEpoch { 0 } else { catalog + 1 }
                } else {
                    catalog
                };
            }
            action ObserveCandidateOne when (failed == 1) {
                latest_candidate = 1;
                pending_candidate = 1;
            }
            action ObserveCandidateTwo when (
                failed == 1 && latest_candidate == 1
            ) {
                latest_candidate = 2;
                pending_candidate = 2;
            }
            action AdmitCandidateOne when (
                failed == 1 && pending_candidate > 0
            ) {
                failed = if pending_candidate == 1 || Buggy == 1 { 0 } else { failed };
                status_failed = if pending_candidate == 1 || Buggy == 1 {
                    0
                } else {
                    status_failed
                };
                recovery_wakes = if pending_candidate == 1 || Buggy == 1 {
                    recovery_wakes + 1
                } else {
                    recovery_wakes
                };
                recovered_candidate = if pending_candidate == 1 || Buggy == 1 {
                    1
                } else {
                    recovered_candidate
                };
                pending_candidate = if pending_candidate == 1 || Buggy == 1 {
                    0
                } else {
                    pending_candidate
                };
            }
            action AdmitCandidateTwo when (
                failed == 1 && pending_candidate == 2
            ) {
                failed = 0;
                status_failed = 0;
                recovery_wakes = recovery_wakes + 1;
                recovered_candidate = 2;
                pending_candidate = 0;
            }
            action Recover when (
                (failed == 1 || (Buggy == 2 && recovery_wakes == 0)) && latest_candidate == 0
            ) {
                failed = 0;
                status_failed = 0;
                recovery_wakes = recovery_wakes + 1;
            }
            action HealthyCatalogEdge when (failed == 0) {
                catalog = if catalog == MaxEpoch { 0 } else { catalog + 1 };
            }
            invariant FailureStatusExact: status_failed == failed;
            invariant FailureWakeDeduped: failure_wakes == failure_epochs;
            invariant RecoveryWakeBounded: recovery_wakes <= failure_wakes;
            invariant FailedPollRetainsCatalog:
                if failed == 1 { catalog == latched_catalog } else { catalog <= MaxEpoch };
            invariant ConfigRecoveryAdmitsLatest:
                recovered_candidate == 0 || recovered_candidate == latest_candidate;
            bound CandidateGenerationBounded:
                latest_candidate <= 2 && pending_candidate <= 2 && recovered_candidate <= 2;
            bound EpochsBounded: failure_epochs <= MaxEpoch;
        }
    }
}

/// Crash-journal serialization has one in-flight generation, coalesces edits to the latest desired head, accepts durability only for the exact fsync proof, and rebases/prunes only after an atomic file-save proof.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_draft_journal_model() -> Model {
    aterm_spec::ty_model! {
        NativeDraftJournal {
            const Buggy = 0;
            const MaxSeq = 4;
            const MaxGeneration = 4;
            var edit_seq = 0;
            var desired_seq = 0;
            var durable_seq = 0;
            var inflight = 0;
            var target_seq = 0;
            var generation = 0;
            var file_durable_seq = 0;
            var baseline_seq = 0;
            var checkpoint_ready = 0;
            var stale_rejected = 0;
            var stale_accepted = 0;
            var unsafe_prune = 0;
            var journal_disk_generation = 0;
            var plan_disk_generation = 0;
            var disk_conflict_rejected = 0;
            var wrong_image_accepted = 0;
            action Edit when (edit_seq <= MaxSeq - 1) {
                edit_seq = edit_seq + 1;
                desired_seq = if Buggy == 2 && inflight > 0 {
                    desired_seq
                } else {
                    desired_seq + 1
                };
            }
            action BeginJournal when (
                inflight == 0 && desired_seq > durable_seq &&
                generation <= MaxGeneration - 1
            ) {
                inflight = 1;
                target_seq = if Buggy == 3 { desired_seq + 1 } else { desired_seq };
                generation = generation + 1;
                plan_disk_generation = journal_disk_generation;
            }
            action AcceptJournal when (
                inflight == 1 && journal_disk_generation <= MaxGeneration - 1 &&
                (plan_disk_generation == journal_disk_generation || Buggy == 1)
            ) {
                durable_seq = target_seq;
                inflight = 0;
                wrong_image_accepted = if (
                    Buggy == 1 && journal_disk_generation > plan_disk_generation
                ) { 1 } else { wrong_image_accepted };
                journal_disk_generation = journal_disk_generation + 1;
            }
            action ProveFileSave when (desired_seq > file_durable_seq) {
                file_durable_seq = desired_seq;
                checkpoint_ready = 1;
            }
            action BeginCheckpoint when (
                inflight == 0 &&
                checkpoint_ready + (if Buggy == 1 && desired_seq > file_durable_seq {
                    1
                } else { 0 }) > 0 &&
                generation <= MaxGeneration - 1
            ) {
                inflight = 2;
                target_seq = desired_seq;
                baseline_seq = if Buggy == 1 && checkpoint_ready == 0 {
                    desired_seq
                } else {
                    file_durable_seq
                };
                checkpoint_ready = 0;
                generation = generation + 1;
                plan_disk_generation = journal_disk_generation;
                unsafe_prune = if Buggy == 1 && checkpoint_ready == 0 {
                    1
                } else {
                    unsafe_prune
                };
            }
            action AcceptCheckpoint when (
                inflight == 2 && journal_disk_generation <= MaxGeneration - 1 &&
                (plan_disk_generation == journal_disk_generation || Buggy == 1)
            ) {
                durable_seq = target_seq;
                inflight = 0;
                wrong_image_accepted = if (
                    Buggy == 1 && journal_disk_generation > plan_disk_generation
                ) { 1 } else { wrong_image_accepted };
                journal_disk_generation = journal_disk_generation + 1;
            }
            action ExternalJournalCommit when (
                inflight > 0 && journal_disk_generation <= MaxGeneration - 1
            ) {
                journal_disk_generation = journal_disk_generation + 1;
            }
            action RejectJournalDiskConflict when (
                inflight > 0 && journal_disk_generation > plan_disk_generation
            ) {
                inflight = 0;
                disk_conflict_rejected = 1;
            }
            action RejectStaleProof when (
                inflight > 0 && generation > 1 &&
                stale_rejected <= MaxSeq - 1
            ) {
                durable_seq = if (
                    Buggy == 1 && durable_seq == desired_seq &&
                    desired_seq <= MaxSeq - 1
                ) {
                    durable_seq + 1
                } else {
                    durable_seq
                };
                stale_rejected = stale_rejected + 1;
                stale_accepted = if (
                    Buggy == 1 && durable_seq == desired_seq &&
                    desired_seq <= MaxSeq - 1
                ) { 1 } else { stale_accepted };
            }
            invariant DesiredIsLatest: desired_seq == edit_seq;
            invariant NeverAckFuture: durable_seq <= desired_seq;
            invariant PendingTargetWasPublished: target_seq <= desired_seq;
            invariant PruneOnlyAfterFileDurable: baseline_seq <= file_durable_seq;
            invariant StaleProofIsNoOp: stale_accepted == 0;
            invariant NoUnsafePrune: unsafe_prune == 0;
            invariant JournalImageCas: wrong_image_accepted == 0;
            bound SequenceBounded: edit_seq <= MaxSeq;
            bound GenerationBounded:
                generation <= MaxGeneration &&
                journal_disk_generation <= MaxGeneration &&
                plan_disk_generation <= MaxGeneration;
        }
    }
}

/// Durable restore-manifest publication and single-use consumption.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn restore_manifest_single_use_model() -> Model {
    aterm_spec::ty_model! {
        RestoreManifestSingleUse {
            const Buggy = 0;
            var lock_owner = 0;
            var visible = 1;
            var claim_owner = 0;
            var claim_synced = 0;
            var returned = 0;
            var unique_temporary = 0;
            var unsafe_return = 0;
            var fixed_alias_corrupted = 0;
            var historical_a = 0;
            var historical_b = 0;

            action LockTakeA when (lock_owner == 0) {
                lock_owner = 1;
            }
            action LockTakeB when (lock_owner == 0) {
                lock_owner = 2;
            }
            action ClaimA when (lock_owner == 1 && visible == 1) {
                visible = 0;
                claim_owner = 1;
            }
            action ClaimB when (lock_owner == 2 && visible == 1) {
                visible = 0;
                claim_owner = 2;
            }
            action SyncClaim when (claim_owner > 0 && claim_synced == 0) {
                claim_synced = 1;
            }
            action ReturnA when (
                lock_owner == 1 && claim_owner == 1 && claim_synced == 1
            ) {
                returned = returned + 1;
                claim_owner = 0;
                claim_synced = 0;
                lock_owner = 0;
            }
            action ReturnB when (
                lock_owner == 2 && claim_owner == 2 && claim_synced == 1
            ) {
                returned = returned + 1;
                claim_owner = 0;
                claim_synced = 0;
                lock_owner = 0;
            }
            action ReturnUnsynced when (
                Buggy == 1 && claim_owner > 0 && lock_owner == claim_owner &&
                claim_synced == 0
            ) {
                returned = returned + 1;
                unsafe_return = 1;
                claim_owner = 0;
                lock_owner = 0;
            }
            action ObserveAbsentA when (
                lock_owner == 1 && visible == 0 && claim_owner == 0
            ) {
                lock_owner = 0;
            }
            action ObserveAbsentB when (
                lock_owner == 2 && visible == 0 && claim_owner == 0
            ) {
                lock_owner = 0;
            }
            action LockWriter when (lock_owner == 0 && returned == 0) {
                lock_owner = 3;
            }
            action CreateUniqueTemporary when (
                lock_owner == 3 && unique_temporary == 0
            ) {
                unique_temporary = 1;
            }
            action PublishManifest when (
                lock_owner == 3 && unique_temporary == 1
            ) {
                visible = 1;
                unique_temporary = 0;
                lock_owner = 0;
            }
            action ReuseFixedTemporary when (Buggy == 1 && lock_owner == 3) {
                fixed_alias_corrupted = 1;
            }
            action HistoricalTakeA when (
                Buggy == 1 &&
                ((historical_a == 0 && visible == 1) || historical_a == 1)
            ) {
                historical_a = historical_a + 1;
                visible = if historical_a == 1 { 0 } else { visible };
                returned = if historical_a == 1 { returned + 1 } else { returned };
            }
            action HistoricalTakeB when (
                Buggy == 1 &&
                ((historical_b == 0 && visible == 1) || historical_b == 1)
            ) {
                historical_b = historical_b + 1;
                visible = if historical_b == 1 { 0 } else { visible };
                returned = if historical_b == 1 { returned + 1 } else { returned };
            }

            invariant AtMostOneConsumer: returned <= 1;
            invariant ReturnOnlyAfterDurableClaim: unsafe_return == 0;
            invariant ClaimRemovesVisibleName:
                if claim_owner > 0 || historical_a == 1 || historical_b == 1 {
                    visible == 0
                } else {
                    visible <= 1
                };
            invariant UniqueTemporaryNeverAliases: fixed_alias_corrupted == 0;
            bound OwnerBounded: lock_owner <= 3 && claim_owner <= 2;
            bound FlagsBounded:
                visible <= 1 && claim_synced <= 1 &&
                unique_temporary <= 1 && unsafe_return <= 1 &&
                fixed_alias_corrupted <= 1;
        }
    }
}

/// Atomic close planning freezes the final document sequence, waits for a durable acknowledgement and every leaf's readiness, and only then detaches the tree.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_close_plan_model() -> Model {
    aterm_spec::ty_model! {
        NativeClosePlan {
            const Buggy = 0;
            const MaxSeq = 3;
            var phase = 0;
            var edit_seq = 0;
            var requested_seq = 0;
            var checkpoint_seq = 0;
            var markdown_views = 1;
            var editor_views = 1;
            var document_ready = 0;
            var other_leaf_ready = 0;
            var any_leaf_detached = 0;
            action Edit when (
                (phase == 0 || (Buggy == 1 && phase == 1)) && edit_seq <= MaxSeq - 1
            ) {
                edit_seq = edit_seq + 1;
            }
            action CloseMarkdownNonFinal when (
                phase == 0 && markdown_views > 0 && editor_views > 0
            ) {
                markdown_views = markdown_views - 1;
            }
            action CloseEditorNonFinal when (
                phase == 0 && markdown_views > 0 && editor_views > 0
            ) {
                editor_views = editor_views - 1;
            }
            action BeginFinalClose when (
                phase == 0 && markdown_views + editor_views == 1
            ) {
                phase = 1;
                requested_seq = edit_seq;
                document_ready = if checkpoint_seq == edit_seq { 1 } else { 0 };
                any_leaf_detached = if Buggy == 1 { 1 } else { any_leaf_detached };
            }
            action ReadyOtherLeaf when (phase == 1) {
                other_leaf_ready = 1;
            }
            action AckCheckpoint when (
                phase == 1 && document_ready == 0
            ) {
                checkpoint_seq = if Buggy == 1 { checkpoint_seq } else { requested_seq };
                document_ready = 1;
            }
            action FailCheckpoint when (
                phase == 1 && document_ready == 0
            ) {
                phase = 2;
            }
            action RetryCheckpoint when (phase == 2) {
                phase = 1;
                document_ready = 0;
            }
            action CommitClose when (
                phase == 1 && document_ready == 1 && other_leaf_ready == 1
            ) {
                phase = 3;
                markdown_views = 0;
                editor_views = 0;
                any_leaf_detached = 1;
            }
            invariant NoSilentLoss:
                if phase <= 2 {
                    edit_seq <= MaxSeq
                } else {
                    requested_seq <= checkpoint_seq && requested_seq == edit_seq
                };
            invariant AtomicTreeClose:
                if any_leaf_detached == 0 {
                    any_leaf_detached == 0
                } else {
                    document_ready == 1 && other_leaf_ready == 1
                };
            invariant FrozenFinalSequence:
                if phase == 0 { edit_seq <= MaxSeq } else { requested_seq == edit_seq };
            bound SequenceBounded: edit_seq <= MaxSeq;
        }
    }
}

/// A document owns the latest explicit Save/close durability intent while an older atomic file generation is in flight.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_save_intent_latch_model() -> Model {
    aterm_spec::ty_model! {
        NativeSaveIntentLatch {
            const Buggy = 0;
            const MaxSeq = 3;
            var head = 0;
            var durable = 0;
            var inflight = 0;
            var target = 0;
            var requested = 0;
            var latched = 0;
            var close_waiting = 0;
            var close_seq = 0;
            var closed = 0;
            var settled = 1;
            action Edit when (closed == 0 && close_waiting == 0 && head <= MaxSeq - 1) {
                head = head + 1;
                settled = 0;
            }
            action BeginSave when (inflight == 0 && durable <= head - 1) {
                inflight = 1;
                target = head;
                requested = head;
                latched = 0;
                settled = 0;
            }
            action RequestSave when (inflight == 1) {
                requested = head;
                latched = 1;
                settled = 0;
            }
            action BeginCloseIdle when (
                close_waiting == 0 && inflight == 0 && durable <= head - 1
            ) {
                close_waiting = 1;
                close_seq = head;
                requested = head;
                latched = 0;
                inflight = 1;
                target = head;
                settled = 0;
            }
            action BeginCloseInflight when (
                close_waiting == 0 && inflight == 1
            ) {
                close_waiting = 1;
                close_seq = head;
                requested = head;
                latched = 1;
                settled = 0;
            }
            action CompleteAndPump when (
                inflight == 1 && requested > target
            ) {
                durable = target;
                target = if Buggy == 1 { target } else { requested };
                inflight = if Buggy == 1 { 0 } else { 1 };
                latched = 0;
                settled = if Buggy == 1 { 1 } else { 0 };
            }
            action CompleteChain when (
                inflight == 1 && requested > target
            ) {
                durable = if Buggy == 1 { target } else { requested };
                target = if Buggy == 1 { target } else { requested };
                inflight = 0;
                latched = 0;
                settled = 1;
            }
            action CompleteFinal when (
                inflight == 1 && requested <= target
            ) {
                durable = target;
                inflight = 0;
                latched = 0;
                settled = 1;
            }
            action CommitClose when (
                close_waiting == 1 &&
                (Buggy == 1 || (inflight == 0 && close_seq <= durable))
            ) {
                close_waiting = 0;
                closed = 1;
            }
            invariant SettledCoversLatestRequest:
                if settled == 1 { requested <= durable } else { durable <= head };
            invariant WaitingCloseHasCompletionPump:
                if close_waiting == 1 && inflight == 0 { close_seq <= durable }
                else { close_seq <= head };
            invariant ClosedSequenceIsDurable:
                if closed == 1 { close_seq <= durable } else { head <= MaxSeq };
            bound SequenceBounded: head <= MaxSeq;
        }
    }
}

/// Async work is routed by owner identity and generation, not current focus or operation number.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_async_delivery_model() -> Model {
    aterm_spec::ty_model! {
        NativeAsyncDelivery {
            const Buggy = 0;
            const MaxGeneration = 3;
            var owner = 0;
            var sink = 0;
            var token_generation = 0;
            var pending = 0;
            var view_generation = 1;
            var reduced = 0;
            var wrong_delivery = 0;
            var service_dropped_with_view = 0;
            action IssueView when (pending == 0) {
                owner = 1;
                sink = 1;
                token_generation = view_generation;
                pending = 1;
                reduced = 0;
            }
            action IssueService when (pending == 0) {
                owner = 4;
                sink = 4;
                token_generation = 1;
                pending = 1;
                reduced = 0;
            }
            action NavigateView when (view_generation <= MaxGeneration - 1) {
                view_generation = view_generation + 1;
            }
            action NavigateCancelsService when (
                Buggy == 1 && view_generation <= MaxGeneration - 1 &&
                pending == 1 && owner == 4
            ) {
                view_generation = view_generation + 1;
                pending = 0;
                service_dropped_with_view = 1;
            }
            action CompleteView when (
                pending == 1 && owner == 1 && sink == 1 &&
                token_generation == view_generation
            ) {
                pending = 0;
                reduced = reduced + 1;
            }
            action CompleteService when (
                pending == 1 && owner == 4 && sink == 4
            ) {
                pending = 0;
                reduced = reduced + 1;
            }
            action DropStaleView when (
                pending == 1 && owner == 1 &&
                view_generation > token_generation
            ) {
                pending = 0;
            }
            action DeliverStaleView when (
                Buggy == 1 && pending == 1 && owner == 1 &&
                view_generation > token_generation
            ) {
                pending = 0;
                reduced = reduced + 1;
                wrong_delivery = 1;
            }
            action RedeliverView when (
                Buggy == 1 && pending == 0 && owner == 1 && reduced == 1
            ) {
                reduced = reduced + 1;
            }
            invariant IdentityAndGenerationChecked: wrong_delivery == 0;
            invariant ServiceOutlivesRequester: service_dropped_with_view == 0;
            invariant ReducedAtMostOnce: reduced <= 1;
            bound GenerationsBounded: view_generation <= MaxGeneration;
        }
    }
}
