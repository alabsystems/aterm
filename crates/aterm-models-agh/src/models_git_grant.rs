// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `AghGitGrant` (docs/DESIGN-agh-2026-10-19.md §6.9, §17.2): what agh's git credential
//! helper answers. It answers only `get`, only for `https` on github.com with a path, never
//! when its stdin is a terminal; a dev-tier owner gets only a dev-tier credential (its read
//! chain, or the writer a one-run push grant names); a release-tier owner gets
//! a credential only under a live grant for that slug, asked by a process in the grant's
//! tree, and a grant never outlives the child it was written for (a spawner killed before it
//! consumed the grant leaves a stale file, which expires).

use aterm_spec::derive::Model;

/// The git-grant model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn agh_git_grant_model() -> Model {
    aterm_spec::ty_model! {
        AghGitGrant {
            const Buggy = 0;
            var tier = 0;   // the slug's owner: 0 dev, 1 release
            var path = 1;   // 1: the request carries path= (useHttpPath)
            var host = 1;   // 1: protocol=https and host=github.com
            var tty = 0;    // 1: the helper's stdin is a terminal
            var op = 0;     // 0 get, 1 store, 2 erase
            var slugm = 1;  // 1: the grant's slug is the request's
            var asker = 0;  // 1: the asking process is outside the grant's tree
            var grant = 0;  // 0 none, 1 live, 2 stale (its spawner was killed)
            var child = 0;  // 0 none, 1 running, 2 exited
            var phase = 0;  // 0 choosing, 1 answered with a credential, 2 quit
            var used = 0;   // 0 none, 1 the dev profile, 2 the release profile
            action NextTier when (phase == 0) { tier = 1 - tier; }
            action TogglePath when (phase == 0) { path = 1 - path; }
            action ToggleHost when (phase == 0) { host = 1 - host; }
            action ToggleTty when (phase == 0) { tty = 1 - tty; }
            action NextOp when (phase == 0) { op = if op == 2 { 0 } else { op + 1 }; }
            action ToggleSlug when (phase == 0) { slugm = 1 - slugm; }
            action ToggleAsker when (phase == 0) { asker = 1 - asker; }
            action Spawn when (phase == 0 && grant == 0 && child == 0) { grant = 1; child = 1; }
            action ChildExits when (phase == 0 && child == 1) { child = 2; }
            action Consume when (phase == 0 && child == 2 && grant == 1) { grant = 0; child = 0; }
            action SpawnerKilled when (phase == 0 && child == 2 && grant == 1) { grant = 2; }
            action Expire when (phase == 0 && grant == 2) { grant = 0; child = 0; }
            action Answer when (phase == 0) {
                phase = if path == 1 && host == 1 && tty == 0 && op == 0
                        && (tier == 0 || (grant == 1 && child == 1 && asker == 0 && slugm == 1)) { 1 } else { 2 };
                used = if path == 1 && host == 1 && tty == 0 && op == 0 && tier == 1
                        && grant == 1 && child == 1 && asker == 0 && slugm == 1 { 2 }
                    else if path == 1 && host == 1 && tty == 0 && op == 0 && tier == 0 { 1 }
                    else { 0 };
            }
            action Reset when (phase > 0) { phase = 0; used = 0; }
            action BuggyPathless when (Buggy == 1 && phase == 0 && path == 0 && tier == 0) { phase = 1; used = 1; }
            action BuggyForeignHost when (Buggy == 1 && phase == 0 && host == 0 && path == 1 && tier == 0) { phase = 1; used = 1; }
            action BuggyTtyAnswers when (Buggy == 1 && phase == 0 && tty == 1 && tier == 0) { phase = 1; used = 1; }
            action BuggyEraseAnswers when (Buggy == 1 && phase == 0 && op > 0 && tier == 0) { phase = 1; used = 1; }
            action BuggyReleaseByRead when (Buggy == 1 && phase == 0 && path == 1 && host == 1 && tty == 0 && op == 0
                    && tier == 1 && grant == 0) { phase = 1; used = 1; }
            action BuggyStaleGrant when (Buggy == 1 && phase == 0 && path == 1 && host == 1 && tty == 0 && op == 0
                    && tier == 1 && grant > 0 && child == 2) { phase = 1; used = 2; }
            action BuggyOtherPid when (Buggy == 1 && phase == 0 && path == 1 && host == 1 && tty == 0 && op == 0
                    && tier == 1 && grant == 1 && child == 1 && asker == 1) { phase = 1; used = 2; }
            action BuggyWrongSlug when (Buggy == 1 && phase == 0 && path == 1 && host == 1 && tty == 0 && op == 0
                    && tier == 1 && grant == 1 && child == 1 && slugm == 0) { phase = 1; used = 2; }
            action BuggyReleaseForDev when (Buggy == 1 && phase == 0 && path == 1 && host == 1 && tty == 0 && op == 0
                    && tier == 0 && grant == 1 && child == 1) { phase = 1; used = 2; }
            invariant PathlessGetsNothing: path > 0 || used <= 0;
            invariant GithubOnly: host > 0 || used <= 0;
            invariant NoTerminalDoor: tty <= 0 || used <= 0;
            invariant StoreEraseInert: op <= 0 || used <= 0;
            invariant ReleaseOwnerOnlyByGrant: tier <= 0 || used <= 0 || used == 2;
            invariant ReleaseStaysHome: used <= 1 || tier == 1;
            // GrantLive's `child == 1` is held by Consume (the runner removes the grant once its
            // child exits) and by Expire, never by a check the helper makes.
            invariant GrantLive: used <= 1 || (grant == 1 && child == 1);
            invariant GrantPidBound: used <= 1 || asker <= 0;
            invariant GrantSlugBound: used <= 1 || slugm > 0;
            bound TierRange: tier <= 1;   bound PathRange: path <= 1;     bound HostRange: host <= 1;
            bound TtyRange: tty <= 1;     bound OpRange: op <= 2;         bound SlugRange: slugm <= 1;
            bound AskerRange: asker <= 1; bound GrantRange: grant <= 2;   bound ChildRange: child <= 2;
            bound PhaseRange: phase <= 2; bound UsedRange: used <= 2;
        }
    }
}
