// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_url_is_logged_by_its_scheme_and_host_never_its_query() {
    assert_eq!(
        logged("https://auth.openai.com/oauth/authorize?state=s3cret&code_challenge=x").as_deref(),
        Some("https://auth.openai.com")
    );
    assert_eq!(
        logged("x-lab-control://selftest").as_deref(),
        Some("x-lab-control://selftest")
    );
    assert_eq!(
        logged("/Applications/Safari.app").as_deref(),
        Some("<a path or app>")
    );
    assert_eq!(logged("-a"), None);
}

/// A URL's shape names its place, its query's keys, and only `scope`'s and `expires_in`'s
/// values, decoded; no other value (a state, a challenge) is in it.
#[test]
fn a_url_shape_holds_its_keys_and_only_the_scope_and_lifetime() {
    let url = "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c&\
               response_type=code&scope=user%3Ainference&code_challenge=SECRETCH&state=SECRETST";
    let shape = url_shape(url).expect("a URL");
    assert_eq!(
        shape,
        "https://claude.com/cai/oauth/authorize keys=code,client_id,response_type,scope,\
         code_challenge,state scope=user:inference"
    );
    assert!(!shape.contains("SECRET"), "{shape}");
    assert_eq!(
        url_shape("https://a.test/p?scope=a+b%3Ac&expires_in=31536000").as_deref(),
        Some("https://a.test/p keys=scope,expires_in scope=a b:c expires_in=31536000")
    );
    assert_eq!(url_decoded("100%"), "100%", "an escape cut short is kept");
    assert_eq!(url_shape("/Applications/Safari.app"), None);
}

/// The recorder's verdict needs both halves: an empty `open.log` and the
/// control logged. NEGATIVE CONTROLS: an agent's open in the log fails it,
/// and so does a control that was never logged (the empty log of a
/// recorder that logs nothing), or one that logged another URL.
#[test]
fn nothing_opened_needs_an_empty_log_and_the_control_logged() {
    let ctl = "18:48:24 open x-lab-control://selftest\n";
    assert!(nothing_opened("", ctl));
    assert!(nothing_opened("\n", ctl));
    assert!(!nothing_opened("18:48:30 open https://claude.ai\n", ctl));
    assert!(!nothing_opened("", ""));
    assert!(!nothing_opened("", "18:48:24 open x-lab-control://other\n"));
    let w = Witness {
        opened: String::new(),
        control: ctl.into(),
    };
    assert!(w.ok());
    let opened = Witness {
        opened: "18:48:30 open <a path or app>\n".into(),
        ..w
    };
    assert!(!opened.ok());
}
