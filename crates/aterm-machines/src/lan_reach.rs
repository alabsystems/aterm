// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What of aterm's reaches the LAN through the root LAN broker on macOS
//! (`docs/DESIGN-lan-broker-2026-11-05.md`), and whether the broker answers: a vault at a URL on
//! the LAN is reached through it, so a step that would reach one waits while it does not; a
//! vault at a path, or at a URL off the LAN, waits for nothing.

use std::time::Duration;

/// Why the LAN broker, through which aterm reaches the LAN on macOS, does not answer: its
/// hello's error in its own words (not installed, another protocol, refused), from
/// `aterm_uds::lan::broker::hello`. `None` where it answers, and off macOS, where the LAN is
/// reached in-process.
#[must_use]
pub fn broker_unready() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    aterm_uds::lan::broker::hello()
        .err()
        .map(|why| why.to_string())
}

/// Whether `url`'s host is on the LAN, so aterm reaches it through the LAN broker on macOS: a
/// local name (`alab-secrets.local`, a single label) or a local-network address
/// ([`aterm_uds::lan::is_local_name`], [`aterm_uds::lan::is_local_network`]). A source that is no
/// URL (a vault at a path) is not.
#[must_use]
pub fn on_the_lan(url: &str) -> bool {
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or(bracketed),
        None => authority
            .rsplit_once(':')
            .map_or(authority, |(host, _)| host),
    };
    match host.parse::<std::net::IpAddr>() {
        Ok(addr) => aterm_uds::lan::is_local_network(&addr),
        Err(_) => aterm_uds::lan::is_local_name(host),
    }
}

/// How often [`watch_arrivals`] asks while the broker does not answer: a connect to a socket
/// that is not there fails at once, so this costs nothing, and a step waiting for the broker
/// goes on within it of the elevation that installs it.
pub const ABSENT_POLL: Duration = Duration::from_secs(2);

/// How often [`watch_arrivals`] asks while the broker answers, to hear it go (a repair, an
/// upgrade that restarts it) and so its next arrival.
pub const PRESENT_POLL: Duration = Duration::from_secs(30);

/// Call `arrived` each time the LAN broker starts answering after it did not, for as long as
/// this process lives: a step deferred until the broker answers (the vault's look, a reach of a
/// vault on the LAN) is woken by its arrival, never by a person. Asks every [`ABSENT_POLL`]
/// while it is absent and every [`PRESENT_POLL`] while it answers. Off macOS, where nothing
/// waits for a broker, it returns at once.
pub fn watch_arrivals(arrived: impl FnMut()) {
    if !cfg!(target_os = "macos") {
        return;
    }
    arrivals(
        || broker_unready().is_none(),
        std::thread::sleep,
        arrived,
        None,
    );
}

/// [`watch_arrivals`]' loop: `answers` asked, then `sleep` for the poll its answer calls for,
/// `arrived` called on each absent-to-answering turn (the first answer included only after an
/// absence), for `rounds` asks (`None`: forever).
fn arrivals(
    mut answers: impl FnMut() -> bool,
    mut sleep: impl FnMut(Duration),
    mut arrived: impl FnMut(),
    rounds: Option<usize>,
) {
    let mut was = true;
    let mut asked = 0usize;
    while rounds.is_none_or(|rounds| asked < rounds) {
        asked += 1;
        let now = answers();
        if now && !was {
            arrived();
        }
        was = now;
        sleep(if now { PRESENT_POLL } else { ABSENT_POLL });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a vault on the LAN (a `.local` or single-label name, a link-local address or one on
    /// an attached subnet) is reached through the broker; a public URL and a path are not.
    /// NEGATIVE CONTROL: an `on_the_lan` true of every URL reads the public vault as on the LAN.
    #[test]
    fn only_a_url_on_the_lan_is_on_the_lan() {
        for (source, lan) in [
            ("http://alab-secrets.local:7484/v1/alab", true),
            ("http://m11:7484/v1/alab", true),
            ("http://169.254.10.20:7484/v1/alab", true),
            ("http://[fe80::1]:7484/v1/alab", true),
            ("https://user@m11.local/v1/alab", true),
            ("https://vault.example/v1/alab", false),
            ("https://8.8.8.8/v1/alab", false),
            ("/Volumes/Keys/team.vault", false),
            ("~/team.vault", false),
        ] {
            assert_eq!(on_the_lan(source), lan, "{source}");
        }
    }

    /// Each turn from absent to answering wakes once; a broker that answered from the start, or
    /// keeps answering, wakes nothing; the poll follows the answer.
    /// NEGATIVE CONTROL: waking on every answer wakes four times here.
    #[test]
    fn each_arrival_after_an_absence_wakes_once() {
        let answers = [true, false, false, true, true, false, true];
        let mut asked = answers.iter().copied();
        let mut slept = Vec::new();
        let mut woken = 0;
        arrivals(
            || asked.next().unwrap(),
            |poll| slept.push(poll),
            || woken += 1,
            Some(answers.len()),
        );
        assert_eq!(woken, 2);
        let expected: Vec<_> = answers
            .iter()
            .map(|&answers| if answers { PRESENT_POLL } else { ABSENT_POLL })
            .collect();
        assert_eq!(slept, expected);
    }
}
