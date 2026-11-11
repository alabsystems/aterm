// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn execution_and_observation_share_the_option_value_boundaries() {
    let args = ["socket", "--secret-file", "--durability", "relaxed"];
    let parsed = BrokerArgs::parse(&args)
        .ok()
        .expect("accepted broker syntax");
    assert_eq!(parsed.secret_file.copied(), Some("--durability"));
    assert_eq!(parsed.words, vec![&"socket", &"relaxed"]);
    assert_eq!(
        parsed.serving_durability(),
        Some(crate::durability::default_for(true))
    );
    let at = |path: &str| aterm_uds::holders::SocketPath::of(std::path::Path::new(path));
    assert!(parsed.serves(&at("socket")));
    assert!(!parsed.serves(&at("relaxed")));
    let opaque = [
        "--secret-file",
        "--help",
        "--durability",
        "--repair",
        "sock",
    ];
    let parsed = BrokerArgs::parse(&opaque)
        .ok()
        .expect("values are opaque words");
    assert_eq!(parsed.secret_file.copied(), Some("--help"));
    assert_eq!(parsed.durability.copied(), Some("--repair"));
    assert_eq!(parsed.words, vec![&"sock"]);
    assert!(!parsed.repair);
    assert!(matches!(
        BrokerArgs::parse(&["--tcp", "127.0.0.1:7", "log"]),
        Err(ParseError::Usage(_))
    ));
    assert!(matches!(
        BrokerArgs::parse(&["sock", "--secret-file"]),
        Err(ParseError::Usage(_))
    ));
    assert!(matches!(
        BrokerArgs::parse(&["sock", "--secret-file", "one", "--secret-file", "two"]),
        Err(ParseError::Usage(_))
    ));
    assert!(matches!(
        BrokerArgs::parse(&["sock", "--help"]),
        Err(ParseError::Help)
    ));
}

#[test]
fn a_lifeline_is_a_valued_flag_and_the_serving_reads_hold() {
    let argv = [
        "aterm",
        "link",
        "broker",
        "/r/bus.sock",
        "/r/bus.log",
        "--secret-file",
        "/r/mint.secret",
        aterm_uds::lifeline::FLAG,
        "0",
    ];
    let parsed = BrokerArgs::from_invocation(&argv).expect("a serving invocation");
    assert_eq!(parsed.lifeline_fd.copied(), Some("0"));
    assert_eq!(parsed.lifeline_at, Some(4), "its place among the words");
    assert_eq!(parsed.words, vec![&"/r/bus.sock", &"/r/bus.log"]);
    assert!(
        parsed.serves(&aterm_uds::holders::SocketPath::of(std::path::Path::new(
            "/r/bus.sock"
        )))
    );
    assert_eq!(
        parsed.serving_durability(),
        Some(crate::durability::default_for(true))
    );
    assert_eq!(
        parsed.log_path(),
        Some(std::path::PathBuf::from("/r/bus.log"))
    );
    let words = ["sock", "--lifeline-fd", "--durability"];
    let opaque = BrokerArgs::parse(&words)
        .ok()
        .expect("its value is an opaque word");
    assert_eq!(opaque.lifeline_fd.copied(), Some("--durability"));
    assert!(opaque.durability.is_none());
    assert!(matches!(
        BrokerArgs::parse(&["sock", "--lifeline-fd", "0", "--lifeline-fd", "3"]),
        Err(ParseError::Usage(_))
    ));
    let cut = &argv[..argv.len() - 1];
    assert!(BrokerArgs::from_invocation(cut).is_none(), "no value");
}

#[test]
fn the_log_is_the_one_the_broker_verb_serves() {
    let log = |args: &[&str]| BrokerArgs::parse(args).ok().and_then(|a| a.log_path());
    let p = std::path::PathBuf::from;
    assert_eq!(log(&["/r/bus.sock", "/r/bus.log"]), Some(p("/r/bus.log")));
    assert_eq!(log(&["/r/bus.sock"]), Some(p("/r/bus.sock.log")));
    assert_eq!(
        log(&[
            "/r/bus.sock",
            "--secret-file",
            "/r/mint.secret",
            "/r/bus.log",
            "--durability",
            "relaxed"
        ]),
        Some(p("/r/bus.log"))
    );
    assert_eq!(log(&["bus.sock", "bus.log"]), None, "relative");
    assert_eq!(
        log(&["--repair", "/r/bus.log"]),
        None,
        "a repair serves nothing"
    );
    assert_eq!(
        log(&["/r/a", "/r/b", "/r/c"]),
        None,
        "a surplus word is refused"
    );
    assert_eq!(
        log(&["/r/bus.sock", "--durability", "fast"]),
        None,
        "a tier the verb refuses"
    );
}
