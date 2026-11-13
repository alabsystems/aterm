// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_machine_whose_host_name_is_under_a_words_floor_still_scrubs() {
    let top = aterm_tempfile::Builder::new()
        .prefix("lab-scrub-")
        .tempdir()
        .expect("scratch");
    let home = std::fs::canonicalize(top.path()).expect("physical");
    let p = crate::paths::Paths::resolve_with(Some(home.clone()), Some(home.into()));
    let (scrub, _) = machine_scrub_of(&p, &["m8".to_string()]).expect("scrub");
    let user = &p.user;
    assert_eq!(
        scrub.text(&format!("{user}@m8 ~ % ssh m8.local m80")),
        format!("{}@{h} ~ % ssh {h}.local m80", token::USER, h = token::HOST)
    );
}

#[test]
fn under_a_scratch_home_the_account_home_is_scrubbed_too() {
    let top = aterm_tempfile::Builder::new()
        .prefix("lab-scrub-")
        .tempdir()
        .expect("scratch");
    let real = std::fs::canonicalize(top.path()).expect("physical");
    let (account, scratch) = (real.join("account"), real.join("scratch"));
    for d in [&account, &scratch] {
        std::fs::create_dir_all(d).expect("home");
    }
    let text = format!(
        "{}/Library/Application Support/aterm/pkg/bin/claude HOME={}/x",
        account.display(),
        scratch.display()
    );
    let p = crate::paths::Paths::resolve_with(Some(account.clone()), Some(scratch.clone().into()));
    let hosts = ["lab-scrub-host".to_string()];
    let (scrub, home) = machine_scrub_of(&p, &hosts).expect("scrub");
    assert_eq!(home, account);
    assert_eq!(
        scrub.text(&text),
        format!(
            "{h}/Library/Application Support/aterm/pkg/bin/claude HOME={h}/x",
            h = token::HOME
        )
    );
    let own = crate::paths::Paths::resolve_with(Some(account.clone()), Some(account.into()));
    let (scrub, _) = machine_scrub_of(&own, &hosts).expect("scrub");
    let left = scrub.text(&text);
    assert!(
        left.starts_with(&format!("{}/Library", token::HOME)),
        "{left}"
    );
    assert!(
        !left.ends_with(&format!("HOME={}/x", token::HOME)),
        "{left}"
    );
}
