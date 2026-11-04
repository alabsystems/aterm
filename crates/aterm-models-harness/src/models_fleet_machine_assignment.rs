// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// How the harness maps accounts to machines under revision 2 of the lab model
/// (`docs/DESIGN-alab-lab-model-2026-11-01.md` §0: "there are accounts, there are machines, and
/// there are emails"; a machine prefers accounts, and nothing else maps them). One law family to a
/// walk: the first step chooses the area (`area`), and only that area's actions run, since the
/// areas share no state and a product of them would prove nothing more.
///
/// 1. **A fleet file's machine list** (`accounts_sync::fleet_assignment`, the reader of the
///    one-release `fleet-accounts.toml` and per-CLI files). A row's `machines` is admitted whole or
///    refused (`case` 1 to 3), never repaired into another list; an admitted list keeps its
///    normalized order and distinct members (`exact`). A stored `class` word is no input (`case`
///    4): §0 removed the class, so the row is admitted with the order its list derives (one Mac
///    first, several after, as every legacy reader infers it) whatever word, valid, unknown or
///    mistyped, an older writer left.
/// 2. **A preference orders and never limits** (`alab_roster::{fleet_for, asked_row, retain}` over
///    `accounts_sync::build_machine_roster`). One account and this Mac: `here` this Mac prefers
///    it, `other` another Mac does, `row` it has a row here, `cls` that row's order (1 preferred
///    here, 2 pool). A sync keeps every row whoever prefers the account, adds only what this Mac
///    prefers, and orders a row by this Mac's preference alone, never by how many Macs prefer it
///    and never by the order a withdrawn preference left (`Unprefer`); an account asked for by
///    name takes the same order. `dropped`, `unasked` and `misordered`
///    record the opposite.
/// 3. **Labels name no machine** (`alab_roster::{fleet_for, renames}`,
///    `accounts_sync::build_machine_roster`'s default login). One account's row on this Mac:
///    `lbl` 0 none, 1 a machine-shaped label (its legacy label, `m1-2`), 2 its lab label, 3 a
///    label the owner gave; `dflt` the account is this Mac's default login (its row dirless, under
///    whatever label it already has); `settled` this Mac's registry inventory equalled a peer's.
///    A sync names a new row by the lab and never renames one; a migration renames only from the
///    legacy label, only once settled, and every legacy row once settled, the default login's
///    too. `minted`, `resynced`, `misnamed`, `early` and `stale` record the opposite.
/// 4. **An alias resolves to one effective mailbox** (`mailbox_routes::resolve_index` with
///    `alab_roster::{mailbox_of, mail_targets}`). One login address: `lm` the lab decides the
///    mailbox its codes arrive in and a route has that login, `own` a route names the address,
///    `link` it is an alias (`alias_of`) of an address a route names, `dom` a `*@domain` route
///    matches. `res` is the route the login reads its codes from (0 not resolved since the last
///    change, 1 the lab's mailbox, 2 its own, 3 its link's, 4 the domain's, 5 none): the lab's
///    mailbox first, then the address's own route, then its link's, then a pattern.
///
/// Mutants: 1 to 3 a malformed machine list is admitted, 4 a stored class word is read and an
/// unknown one refuses the row, 5 a scalar list is refused, 6 to 8 an admitted list is not the
/// normalized one (a scalar's, a distinct list's order, a duplicate kept), 9 a distinct list is
/// refused, 10 a stored class word overrides the derived order, 11 a row several Macs prefer reads
/// as pool, 12 a sync drops a row another Mac prefers, 13 a sync adds an account another Mac
/// prefers, 14 a sync keeps the stored order of a row this Mac no longer prefers, 21 the default
/// login is named after the machine, 22 a migration renames an owner's
/// label, 23 a migration renames before the registry is settled, 24 a sync renames a row to its
/// lab label, 25 a sync renames the default login's legacy row, 26 a settled migration skips the
/// default login's legacy row, 31 the address's own route beats the lab's mailbox, 32 the alias link beats the
/// address's own route, 33 a domain route beats the alias link.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fleet_machine_assignment_model() -> Model {
    aterm_spec::ty_model! {
        FleetMachineAssignment {
            const Buggy = 0;
            var area = 0;
            var case = 0; var accepted = 0; var exact = 1;
            var here = 0; var other = 0; var row = 0; var cls = 0;
            var dropped = 0; var unasked = 0; var misordered = 0;
            var lbl = 0; var dflt = 0; var settled = 0;
            var minted = 0; var resynced = 0; var misnamed = 0; var early = 0; var stale = 0;
            var lm = 0; var own = 0; var link = 0; var dom = 0; var res = 0;

            action ChooseFile when (area == 0) { area = 1; }
            action ChooseOrder when (area == 0) { area = 2; }
            action ChooseLabels when (area == 0) { area = 3; }
            action ChooseMail when (area == 0) { area = 4; }

            // 1. A fleet file row's machine list.
            action Missing when (area == 1 && case == 0) {
                case = 1; accepted = if Buggy == 1 {1} else {0};
            }
            action InvalidMachine when (area == 1 && case == 0) {
                case = 2; accepted = if Buggy == 2 {1} else {0};
            }
            action WrongType when (area == 1 && case == 0) {
                case = 3; accepted = if Buggy == 3 {1} else {0};
            }
            action StoredClass when (area == 1 && case == 0) {
                case = 4; accepted = if Buggy == 4 {0} else {1};
                exact = if Buggy == 10 {0} else {1};
            }
            action Scalar when (area == 1 && case == 0) {
                case = 5; accepted = if Buggy == 5 {0} else {1};
                exact = if Buggy == 6 {0} else {1};
            }
            action Distinct when (area == 1 && case == 0) {
                case = 6; accepted = if Buggy == 9 {0} else {1};
                exact = if Buggy == 7 {0} else {1};
            }
            action Duplicate when (area == 1 && case == 0) {
                case = 7; accepted = 1; exact = if Buggy == 8 {0} else {1};
            }

            // 2. The order a preference gives, and no limit: the owner's verbs and a person's
            // request, then a sync. Every assignment reads the state before the step.
            action PreferHere when (area == 2 && here == 0) { here = 1; }
            action PreferElsewhere when (area == 2 && other == 0) { other = 1; }
            action Unprefer when (area == 2 && here == 1) { here = 0; }
            action Ask when (area == 2 && row == 0) {
                row = 1;
                cls = if Buggy == 11 {if here == 1 && other == 0 {1} else {2}}
                    else {if here == 1 {1} else {2}};
                misordered = if Buggy == 11 && here == 1 && other == 1 {1} else {misordered};
            }
            action Sync when (area == 2) {
                row = if Buggy == 12 {if here == 1 || (row == 1 && other == 0) {1} else {0}}
                    else {if Buggy == 13 {if row == 1 || here == 1 || other == 1 {1} else {0}}
                    else {if row == 1 || here == 1 {1} else {0}}};
                cls = if Buggy == 14 && row == 1 && here == 0 {cls}
                    else {if Buggy == 12 && here == 0 && other == 1 {0}
                    else {if Buggy == 13 && row == 0 && here == 0 && other == 1 {2}
                    else {if row == 0 && here == 0 {0}
                    else {if Buggy == 11 {if here == 1 && other == 0 {1} else {2}}
                    else {if here == 1 {1} else {2}}}}}};
                dropped = if Buggy == 12 && row == 1 && here == 0 && other == 1 {1} else {dropped};
                unasked = if Buggy == 13 && row == 0 && here == 0 && other == 1 {1} else {unasked};
                misordered = if (Buggy == 11 && here == 1 && other == 1)
                    || (Buggy == 14 && row == 1 && here == 0 && cls == 1) {1} else {misordered};
            }

            // 3. Labels: rows a Mac already had, its default login and the inventory round, then
            // a sync or a migration.
            action RowLegacy when (area == 3 && lbl == 0) { lbl = 1; }
            action RowOwner when (area == 3 && lbl == 0) { lbl = 3; }
            action DefaultLogin when (area == 3 && dflt == 0) { dflt = 1; }
            action Settle when (area == 3 && settled == 0) { settled = 1; }
            action SyncLabels when (area == 3) {
                lbl = if lbl == 0 {if dflt == 1 {if Buggy == 21 {1} else {2}} else {0}}
                    else {if (Buggy == 24 || (Buggy == 25 && dflt == 1)) && lbl == 1 {2} else {lbl}};
                minted = if Buggy == 21 && lbl == 0 && dflt == 1 {1} else {minted};
                resynced = if (Buggy == 24 || (Buggy == 25 && dflt == 1)) && lbl == 1 {1}
                    else {resynced};
            }
            action Migrate when (area == 3) {
                lbl = if Buggy == 26 && dflt == 1 && lbl == 1 {lbl}
                    else {if (lbl == 1 && (settled == 1 || Buggy == 23))
                    || (Buggy == 22 && lbl == 3 && settled == 1) {2} else {lbl}};
                stale = if Buggy == 26 && dflt == 1 && lbl == 1 && settled == 1 {1} else {stale};
                misnamed = if Buggy == 22 && lbl == 3 && settled == 1 {1} else {misnamed};
                early = if Buggy == 23 && lbl == 1 && settled == 0 {1} else {early};
            }

            // 4. Mail: the lab's rows and this Mac's mailbox routes, then the resolution a
            // login's code fetch makes.
            action LabMailbox when (area == 4 && lm == 0) { lm = 1; res = 0; }
            action OwnRoute when (area == 4 && own == 0) { own = 1; res = 0; }
            action AliasLink when (area == 4 && link == 0) { link = 1; res = 0; }
            action DomainRoute when (area == 4 && dom == 0) { dom = 1; res = 0; }
            action Resolve when (area == 4 && res == 0) {
                res = if Buggy == 31 && own == 1 {2}
                    else {if lm == 1 {1}
                    else {if Buggy == 32 && link == 1 {3}
                    else {if own == 1 {2}
                    else {if Buggy == 33 && dom == 1 {4}
                    else {if link == 1 {3}
                    else {if dom == 1 {4} else {5}}}}}}};
            }

            invariant ValidMachines: case == 0 || case > 3 || accepted == 0;
            invariant ValidAssignment: 4 > case || accepted == 1;
            invariant ExactAssignment: accepted == 0 || exact == 1;
            invariant KeptWherePreferredElsewhere: dropped == 0;
            invariant NothingUnasked: unasked == 0;
            invariant OrderIsThisMacsPreference: misordered == 0;
            invariant NoMachineLabelMinted: minted == 0;
            invariant SyncNeverRenames: resynced == 0;
            invariant RenameOnlyFromLegacy: misnamed == 0;
            invariant RenameOnlyOnceSettled: early == 0;
            invariant SettledMigrationRenamesLegacy: stale == 0;
            invariant LabMailboxFirst: res == 0 || lm == 0 || res == 1;
            invariant OwnRouteBeforeLink: res == 0 || lm == 1 || own == 0 || res == 2;
            invariant LinkBeforePattern:
                res == 0 || lm == 1 || own == 1 || link == 0 || res == 3;
        }
    }
}
