// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness accounts observed` (`docs/DESIGN-account-discovery-2026-11-09.md`, step 1):
//! each login place's file beside the row the roster believes is there.

use super::*;

const UNICORN: &str = "unicorn.m15.2@proton.example";
const BRONTO: &str = "brontosaurus.m22@proton.example";

fn home() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("aterm-observed-report-")
        .tempdir()
        .expect("temp dir")
}

fn profile(email: &str) -> String {
    format!(
        r#"{{"oauthAccount":{{"emailAddress":"{email}","accountUuid":"11111111-2222-3333-4444-555555555555"}}}}"#
    )
}

/// THE LIVE BUG, as the verb shows it: the default row `m22` expects brontosaurus, and the
/// default home's profile names unicorn: the row is marked as contradicted, the file's address is
/// shown, and the verb parses with `--json` alone of its flags.
/// NEGATIVE CONTROL: with `agrees` taken from the row's label rather than its address, the place
/// agrees: `` left: Some(true) right: Some(false) ``.
#[test]
fn the_default_place_names_the_observed_login_beside_the_belief() {
    let home = home();
    std::fs::write(home.path().join(".claude.json"), profile(UNICORN)).unwrap();
    let roster = Roster::parse_for(
        Cli::Claude,
        &format!("[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n"),
    )
    .unwrap();
    let mut state = accounts::State::default();
    accounts::note_session(&mut state, "s-41a2", "m22", None, 1_790_000_000);
    let rows = places_seen(&roster, &state, home.path(), 1_790_000_000);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row.place, "~/.claude.json");
    assert_eq!(row.observed.as_deref(), Some(UNICORN));
    assert_eq!(row.believed.as_deref(), Some("m22"));
    assert_eq!(row.agrees, Some(false));
    assert_eq!(row.tabs, vec!["s-41a2".to_owned()]);
    assert_eq!(row.src, "profile");
    // With no default row, the default home is still a place, believed by none.
    let empty = places_seen(&Roster::default(), &state, home.path(), 1_790_000_000);
    assert_eq!(empty[0].believed, None);
    assert_eq!(empty[0].observed.as_deref(), Some(UNICORN));
    assert!(matches!(
        parsed("accounts observed --json"),
        Ok(Cmd::Accounts(AccountsCmd::Observed { json: true }))
    ));
    assert!(parsed("accounts observed --cli codex").is_err());
}
