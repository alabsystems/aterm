// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const STORE: &str = "0F9E2C1A-7B3D-8A4E-9C5F-0123456789AB";

fn first_line(store: Option<&str>, hosts: &[&str]) -> String {
    wire::open_line(&wire::Open {
        title: "aterm \u{b7} signing in Claude someone@example.com".into(),
        store: store.map(str::to_owned),
        hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
        loopback: Some(4321),
    })
}

/// THE HOST TAKES NO ARGUMENT: what it opens is its opener's first line, so the account's
/// address is in no process listing. A word after the verb, no first line, a store that is no
/// UUID and a host that is no host name each open nothing.
#[test]
fn the_host_opens_what_its_first_line_says_and_takes_no_argument() {
    let line = first_line(Some(STORE), &["claude.ai", "claude.com"]);
    let open = opening(&[], Some(&line)).expect("the first line says what to open");
    assert_eq!(
        open.title,
        "aterm \u{b7} signing in Claude someone@example.com"
    );
    assert_eq!(open.store.as_deref(), Some(STORE));
    assert_eq!(open.hosts, ["claude.ai", "claude.com"]);
    assert_eq!(open.loopback, Some(4321));
    assert!(opening(&[], Some(&first_line(None, &[]))).is_ok());

    for word in ["--title", "https://claude.ai/login", "someone@example.com"] {
        assert!(
            opening(&[OsString::from(word)], Some(&line)).is_err(),
            "{word}"
        );
    }
    assert_eq!(opening(&[], None), Err("the opener sent nothing to open"));
    assert!(opening(&[], Some("")).is_err());
    assert_eq!(
        opening(&[], Some(&first_line(Some("not-a-uuid"), &["claude.ai"]))),
        Err("the store is not a UUID")
    );
    for host in [
        "",
        "claude.ai/login",
        "https://claude.ai",
        "a b",
        "claude.ai:443",
    ] {
        assert_eq!(
            opening(&[], Some(&first_line(Some(STORE), &[host]))),
            Err("a host its page may go to is not a host name"),
            "{host:?}"
        );
    }
}

/// A WHOLE PAGE GOES ONLY WHERE THE WINDOW'S PAGES MAY: `https` on a named host, `http` on
/// this Mac's loopback address at exactly the waiting sign-in's port, or the empty page.
/// CONTROLS: another host (a subdomain or a longer name included), plain `http` on a named
/// host, another local port, a loopback address with no port named for it, and any other
/// scheme are each refused.
#[test]
fn a_page_is_sent_only_to_the_named_hosts_and_the_listeners_port() {
    let hosts = vec!["claude.ai".to_owned(), "platform.claude.com".to_owned()];
    let to = |scheme: &str, host: &str, port: Option<u16>, loopback: Option<u16>| {
        admits(scheme, host, port, &hosts, loopback)
    };
    assert!(to("https", "claude.ai", None, Some(4321)));
    assert!(to("HTTPS", "Claude.AI", Some(443), None));
    assert!(to("https", "platform.claude.com", None, None));
    assert!(to("http", "localhost", Some(4321), Some(4321)));
    assert!(to("http", "127.0.0.1", Some(4321), Some(4321)));
    assert!(to("about", "", None, None));

    assert!(!to("https", "accounts.google.com", None, Some(4321)));
    assert!(!to("https", "evil.claude.ai", None, None));
    assert!(!to("https", "claude.ai.example.com", None, None));
    assert!(!to("https", "claude.com", None, None));
    assert!(!to("https", "claude.ai", Some(8443), None));
    assert!(!to("http", "claude.ai", None, Some(4321)));
    assert!(!to("http", "localhost", Some(9999), Some(4321)));
    assert!(!to("http", "localhost", None, Some(4321)));
    assert!(!to("http", "localhost", Some(4321), None));
    assert!(!to("http", "192.168.1.10", Some(4321), Some(4321)));
    assert!(!to("https", "localhost", Some(4321), Some(4321)));
    for scheme in ["file", "data", "javascript", "blob", "ftp", ""] {
        assert!(!to(scheme, "claude.ai", None, Some(4321)), "{scheme}");
    }
}

/// A UUID IS 8-4-4-4-12 HEX DIGITS, in either case, and nothing else.
#[test]
fn a_uuid_is_its_five_hex_groups() {
    assert!(is_uuid("0f9e2c1a-7b3d-8a4e-9c5f-0123456789ab"));
    assert!(is_uuid("0F9E2C1A-7B3D-8A4E-9C5F-0123456789AB"));
    assert!(!is_uuid("0f9e2c1a7b3d8a4e9c5f0123456789ab"));
    assert!(!is_uuid("0f9e2c1a-7b3d-8a4e-9c5f-0123456789a"));
    assert!(!is_uuid("0f9e2c1a-7b3d-8a4e-9c5f-0123456789ag"));
    assert!(!is_uuid("0f9e2c1a-7b3d-8a4e-9c5f_0123456789ab"));
}
