// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn enrollment_options_cannot_accept_credentials_or_unbounded_attempts() {
    let args = |values: &[&str]| values.iter().map(|v| (*v).into()).collect::<Vec<String>>();
    let parsed = parse(&args(&["Me@gmail.com", "--timeout", "45"]))
        .unwrap_or_else(|_| panic!("valid enrollment options"));
    assert_eq!(parsed.account, "me@gmail.com");
    assert_eq!(parsed.within, Duration::from_secs(45));
    for values in [
        vec!["a@b", "--password", "secret"],
        vec!["a@b", "--timeout", "301"],
        vec!["a@b", "--timeout", "0"],
        vec!["a@b", "--timeout", "+45"],
        vec!["a@b", "--timeout", "045"],
        vec!["a@b", "--client-file", "/tmp/google-client.json"],
        vec!["a@b", "--timeout", "1", "--timeout", "2"],
        vec!["a@b@c"],
    ] {
        assert!(parse(&args(&values)).is_err());
    }
}

/// THE GOOGLE CLIENT REGISTRATION IS THE VAULT'S ENTRY ALONE: where this Mac's store holds none,
/// enrollment files `entry:google-mail-client.json` and stops naming it, though a file of that
/// name lies in `~/.secrets`. NEGATIVE CONTROL: once the store holds the entry, it is read (and
/// here refused as no registration).
#[cfg(unix)]
#[test]
fn a_client_registration_the_vault_lacks_is_filed_and_one_it_holds_is_read() {
    use aterm_secrets::need::{self, Purpose, Want};
    let home = aterm_tempfile::tempdir().expect("home");
    let entry = aterm_secrets::entry_class::GOOGLE_MAIL_CLIENT;
    std::fs::create_dir_all(home.path().join(".secrets")).expect("secrets");
    std::fs::write(home.path().join(".secrets").join(entry), "{}").expect("client");
    let missing = read_client(home.path())
        .err()
        .expect("no registry names it")
        .message;
    assert!(
        missing.contains("not in this Mac's vault")
            && missing.contains("entry:google-mail-client.json"),
        "{missing}"
    );
    let paths = aterm_secrets::client::Paths::for_home(home.path());
    let filed = need::peek(&paths).expect("ledger");
    let want = Want::Entry(entry.to_owned());
    let filed = filed.iter().find(|need| need.want == want).expect("filed");
    assert_eq!(filed.purpose, Purpose::HarnessSignIn);
    crate::vault_entry::put(home.path(), entry, "{}");
    let read = read_client(home.path())
        .err()
        .expect("an empty registration")
        .message;
    assert!(!read.contains("not in this Mac's vault"), "{read}");
}
