// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Structural account replacement preserves every non-account part of the recipe.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_account_successor_recipe_model() -> Model {
    aterm_spec::ty_model! {
        NativeAccountSuccessorRecipe {
            const Buggy = 0;
            var context = 1; var native = 1; var command = 1; var stored = 1;
            var namespace = 1; var routing = 1; var fresh = 1; var canonical = 1;
            var checked = 0; var accepted = 0;
            action ChangeContext when (checked == 0) { context = 0; }
            action ChangeNative when (checked == 0) { native = 0; }
            action ChangeCommand when (checked == 0) { command = 0; }
            action ExternalAccount when (checked == 0) { stored = 0; }
            action ChangeNamespace when (checked == 0) { namespace = 0; }
            action MismatchRouting when (checked == 0) { routing = 0; }
            action ReuseOperation when (checked == 0) { fresh = 0; }
            action InvalidEncoding when (checked == 0) { canonical = 0; }
            action Check when (checked == 0) {
                checked = 1;
                accepted = if (context == 1 || Buggy == 1)
                    && (native == 1 || Buggy == 2) && (command == 1 || Buggy == 3)
                    && (stored == 1 || Buggy == 4) && (namespace == 1 || Buggy == 5)
                    && (routing == 1 || Buggy == 6) && (fresh == 1 || Buggy == 7)
                    && (canonical == 1 || Buggy == 8) { 1 } else { 0 };
            }
            invariant ContextPreserved: accepted == 0 || context == 1;
            invariant NativeIdentityPreserved: accepted == 0 || native == 1;
            invariant CommandPreserved: accepted == 0 || command == 1;
            invariant StoredOnly: accepted == 0 || stored == 1;
            invariant NamespacePreserved: accepted == 0 || namespace == 1;
            invariant RoutingMatches: accepted == 0 || routing == 1;
            invariant FreshOperation: accepted == 0 || fresh == 1;
            invariant CanonicalInput: accepted == 0 || canonical == 1;
            bound Finite: 0 <= context && context <= 1 && 0 <= native && native <= 1
                && 0 <= command && command <= 1 && 0 <= stored && stored <= 1
                && 0 <= namespace && namespace <= 1 && 0 <= routing && routing <= 1
                && 0 <= fresh && fresh <= 1 && 0 <= canonical && canonical <= 1
                && 0 <= checked && checked <= 1 && 0 <= accepted && accepted <= 1;
        }
    }
}
