// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn peer_cycle_is_exclusive_with_startup_suppression_and_delayed_observation() {
    let parse =
        |words: &[&str]| options(&words.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
    assert_eq!(parse(&[]).unwrap(), (false, Mode::Normal));
    assert_eq!(
        parse(&["--log-backed", "--local-follower"]).unwrap(),
        (true, Mode::LocalFollower)
    );
    assert_eq!(
        parse(&["--peer-cycle"]).unwrap(),
        (false, Mode::PeerCycle { details: false })
    );
    assert_eq!(
        parse(&["--log-backed", "--peer-cycle"]).unwrap(),
        (true, Mode::PeerCycle { details: false })
    );
    assert_eq!(
        parse(&["--peer-cycle", "--peer-details"]).unwrap(),
        (false, Mode::PeerCycle { details: true })
    );
    assert_eq!(
        parse(&["--log-backed", "--peer-cycle", "--peer-details"]).unwrap(),
        (true, Mode::PeerCycle { details: true })
    );
    assert_eq!(
        parse(&["--log-backed", "--observe", "60"]).unwrap(),
        (true, Mode::Observe(60))
    );
    for words in [
        vec!["--suppressed", "--peer-cycle"],
        vec!["--local-follower", "--peer-cycle"],
        vec!["--local-follower", "--suppressed"],
        vec!["--local-follower", "--observe", "1"],
        vec!["--peer-cycle", "--suppressed"],
        vec!["--observe", "1", "--peer-cycle"],
        vec!["--peer-cycle", "--observe", "1"],
        vec!["--peer-cycle", "--peer-cycle"],
        vec!["--peer-cycle", "--log-backed"],
        vec!["--peer-details"],
        vec!["--peer-details", "--peer-cycle"],
        vec!["--suppressed", "--peer-details"],
        vec!["--observe", "1", "--peer-details"],
        vec!["--peer-cycle", "--peer-details", "--peer-details"],
        vec!["--observe", "0"],
        vec!["--observe", "61"],
    ] {
        assert!(parse(&words).is_err(), "incompatible options: {words:?}");
    }
}

#[test]
fn an_ordinary_host_environment_keeps_its_private_marker_and_guarded_attachment() {
    let mut world = World::new(CONFIG_OFF).unwrap();
    world.keep();
    let env = environment(&world, 49152);
    assert!(
        env.iter()
            .any(|(key, value)| key == "ATERM_STATE_HOME" && Path::new(value) == world.state())
    );
    assert!(
        env.iter()
            .any(|(key, value)| key == "HOME" && Path::new(value) == world.home)
    );
    assert!(
        env.iter()
            .any(|(key, value)| key == "XDG_RUNTIME_DIR" && Path::new(value) == world.run)
    );
    assert!(
        env.iter()
            .any(|(key, value)| key == aterm_net::loopback::SEAM && value == "1")
    );
    assert!(
        env.iter()
            .any(|(key, value)| key == aterm_db::runtime::LOOPBACK_PORT_ENV && value == "49152")
    );
}
