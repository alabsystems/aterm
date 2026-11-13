// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A fleet file and a pool file read through the harness's own parsers into rows the
/// importer takes: Claude's realm from the plain file, Codex's from its file's name, a Kimi
/// row's from its region, and a warm list in its order, with what the harness calls the row.
/// NEGATIVE CONTROL: with `realm_of` answering `cli.id()` for Kimi too, the importer's account
/// key would lose the region: `` left: "kimi" right: "kimi:mainland-cn" ``.
#[test]
fn harness_files_read_through_the_harnesss_parsers() {
    let fleet = "[[account]]\nlabel = \"m8-2\"\nemail = \"ann@example.test\"\nmachines = \"m8\"\n";
    let rows = Harness
        .stated(SheetKind::FleetAccounts, "fleet-accounts.toml", fleet)
        .unwrap();
    assert_eq!(
        rows,
        [Stated {
            realm: "claude".to_owned(),
            email: "ann@example.test".to_owned(),
            label: Some("m8-2".to_owned()),
            machines: vec!["m8".to_owned()],
        }]
    );
    let codex = Harness
        .stated(SheetKind::FleetAccounts, "ai-accounts-codex.toml", fleet)
        .unwrap();
    assert_eq!(codex[0].realm, "codex");
    let kimi = "[[account]]\nlabel = \"kimi-m16\"\nemail = \"ann@example.test\"\nmachines = \"m16\"\nregion = \"mainland-cn\"\n";
    let kimi = Harness
        .stated(SheetKind::FleetAccounts, "ai-accounts-kimi.toml", kimi)
        .unwrap();
    assert_eq!(kimi[0].realm, "kimi:mainland-cn");
    let pool = "version = 1\n[[account]]\nprovider = \"claude\"\nemail = \"ann@example.test\"\nwarm = [\"m8\", \"m9\"]\n";
    let warm = Harness
        .stated(SheetKind::AiPool, "ai-pool.toml", pool)
        .unwrap();
    assert_eq!(warm[0].machines, ["m8", "m9"]);
    assert_eq!(warm[0].realm, "claude");
    assert!(
        Harness
            .stated(SheetKind::FleetAccounts, "ai-accounts-other.toml", fleet)
            .is_err()
    );
    for wrong_name in [
        "codex.toml",
        "ai-pool.toml",
        "ai-accounts-codex",
        "dir/fleet-accounts.toml",
        "",
    ] {
        assert!(
            Harness
                .stated(SheetKind::FleetAccounts, wrong_name, fleet)
                .is_err(),
            "{wrong_name} must not fall back to Claude"
        );
    }
    assert!(
        Harness
            .stated(SheetKind::Vault, "fleet-accounts.toml", fleet)
            .is_err()
    );
    assert!(
        Harness
            .stated(SheetKind::MailboxToml, "ai-mailbox.toml", pool)
            .is_err()
    );
    assert!(
        Harness
            .stated(SheetKind::AiPool, "dir/ai-pool.toml", pool)
            .is_err()
    );
}
