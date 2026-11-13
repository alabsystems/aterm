// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn enrollment_options_cannot_accept_credentials_or_unbounded_attempts() {
    let args = |values: &[&str]| values.iter().map(|v| (*v).into()).collect::<Vec<String>>();
    let home = Path::new("/home/example");
    let parsed = parse(home, &args(&["Me@gmail.com", "--timeout", "45"]))
        .unwrap_or_else(|_| panic!("valid enrollment options"));
    assert_eq!(parsed.account, "me@gmail.com");
    assert_eq!(parsed.within, Duration::from_secs(45));
    assert_eq!(
        parsed.client_file,
        home.join(".config/aterm/mail/google-client.json")
    );
    for values in [
        vec!["a@b", "--password", "secret"],
        vec!["a@b", "--timeout", "301"],
        vec!["a@b", "--timeout", "0"],
        vec!["a@b", "--timeout", "+45"],
        vec!["a@b", "--timeout", "045"],
        vec!["a@b", "--client-file"],
        vec!["a@b", "--client-file", "bad\0path"],
        vec!["a@b", "--timeout", "1", "--timeout", "2"],
        vec!["a@b@c"],
    ] {
        assert!(parse(home, &args(&values)).is_err());
    }
}
