// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `AghRouteWall`: identity routing rules by owner tier, access class, and redirect hop.

use aterm_spec::derive::*;

/// Route-wall model over the canonical config (tier wall, named destination, and redirect hop).
/// `Vault = 1` is the same wall over the rows vault adoption derives with no agh.toml (design
/// §6.10). They are the canonical rows with each absent token's rows left out, so two
/// decisions differ: the dev owner's read with no dev token has no row and goes anonymous, and
/// the release owner's plain read with no dev token is the release token's.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn agh_route_wall_model() -> Model {
    aterm_spec::ty_model! {
        AghRouteWall {
            const Buggy = 0;
            const Vault = 0;  // 1: the rows vault adoption derives (design §6.10)
            var tier = 0;     // destination owner: 0 dev, 1 release, 2 external (default row)
            var op = 0;       // 0 Read, 1 WriterRead, 2 Write
            var named = 0;    // 1: the slug was typed literally on this command line
            var dev = 1;      // the dev profile is present
            var rel = 0;      // the release profile is present
            var amb = 1;      // GH_TOKEN/GITHUB_TOKEN is set in the environment
            var phase = 0;    // 0 choosing, 1 sent with an identity, 2 sent anonymously, 3 refused
            var used = 0;     // 0 none, 1 dev, 2 release, 3 ambient
            var hop = 0;      // 1: the answer redirected and agh made the one hop
            var hopauth = 0;  // 1: the hop carried Authorization

            action NextTier when (phase == 0) { tier = if tier == 2 { 0 } else { tier + 1 }; }
            action NextOp when (phase == 0) { op = if op == 2 { 0 } else { op + 1 }; }
            action ToggleNamed when (phase == 0) { named = 1 - named; }
            action ToggleDev when (phase == 0) { dev = 1 - dev; }
            action ToggleRel when (phase == 0) { rel = 1 - rel; }
            action ToggleAmbient when (phase == 0) { amb = 1 - amb; }
            action Reset when (phase > 0) { phase = 0; used = 0; hop = 0; hopauth = 0; }

            action Decide when (phase == 0) {
                phase = if Vault == 0 && ((dev == 1 && (tier == 0 || op == 0))
                        || (tier == 1 && rel == 1 && (op == 1 || (op == 2 && named == 1)))) { 1 }
                    else if Vault == 0 && op == 0 && tier > 0 && dev == 0 { 2 }
                    else if Vault == 1 && ((dev == 1 && (tier == 0 || op == 0))
                        || (tier == 1 && rel == 1
                            && (op == 1 || (op == 2 && named == 1) || (op == 0 && dev == 0)))) { 1 }
                    else if Vault == 1 && op == 0 { 2 }
                    else { 3 };
                used = if dev == 1 && (tier == 0 || op == 0) { 1 }
                    else if tier == 1 && rel == 1 && (op == 1 || (op == 2 && named == 1)) { 2 }
                    else if Vault == 1 && tier == 1 && rel == 1 && op == 0 && dev == 0 { 2 }
                    else { 0 };
            }
            action Hop when (phase > 0 && phase <= 2 && hop == 0 && op <= 1) { hop = 1; hopauth = 0; }

            action BuggyDecideFallback when (Buggy == 1 && phase == 0 && tier == 1 && op == 1 && rel == 0 && dev == 1) { phase = 1; used = 1; }
            action BuggyDecideAlias when (Buggy == 1 && phase == 0 && tier == 1 && op == 2 && rel == 1 && named == 0) { phase = 1; used = 2; }
            action BuggyDecideReleaseAbroad when (Buggy == 1 && phase == 0 && tier == 2 && rel == 1) { phase = 1; used = 2; }
            action BuggyDecideAmbient when (Buggy == 1 && phase == 0 && amb == 1 && dev == 0) { phase = 1; used = 3; }
            action BuggyAnonymousCarries when (Buggy == 1 && phase == 0 && op == 0 && tier > 0 && dev == 0 && amb == 1) { phase = 2; used = 3; }
            action BuggyDecideAnonWrite when (Buggy == 1 && phase == 0 && op == 2 && rel == 0 && dev == 0) { phase = 2; used = 0; }
            action BuggyRefuseButSend when (Buggy == 1 && phase == 0 && tier == 0 && dev == 0) { phase = 3; used = 1; }
            action BuggyHopKeepsAuth when (Buggy == 1 && phase == 1 && hop == 0 && op <= 1) { hop = 1; hopauth = 1; }
            action BuggyHopOnWrite when (Buggy == 1 && phase == 1 && hop == 0 && op == 2) { hop = 1; hopauth = 0; }

            invariant ReleaseTokenStaysHome: used <= 1 || used > 2 || tier == 1;
            invariant TierWall: used <= 0 || used > 1 || tier <= 0 || tier > 1 || op <= 0;
            invariant NamedDestination: phase <= 0 || phase > 1 || tier <= 0 || tier > 1 || op <= 1 || named > 0;
            invariant NoAmbient: used <= 2;
            invariant NoAnonymousWriterOrWrite: phase <= 1 || phase > 2 || op <= 0;
            invariant RefusalSendsNothing: phase <= 2 || used <= 0;
            invariant AnonymousCarriesNothing: phase <= 1 || phase > 2 || used <= 0;
            invariant SendCarriesIdentity: phase <= 0 || phase > 1 || (used > 0 && used <= 2);
            invariant ExternalWriteOff: phase <= 0 || phase > 1 || tier <= 1 || op <= 0;
            invariant HopDropsAuth: hop <= 0 || hopauth <= 0;
            invariant NoWriteHop: hop <= 0 || op <= 1;

            bound TierRange: tier <= 2;  bound OpRange: op <= 2;   bound NamedRange: named <= 1;
            bound DevRange: dev <= 1;    bound RelRange: rel <= 1; bound AmbRange: amb <= 1;
            bound PhaseRange: phase <= 3; bound UsedRange: used <= 3;
            bound HopRange: hop <= 1;    bound HopAuthRange: hopauth <= 1;
        }
    }
}
