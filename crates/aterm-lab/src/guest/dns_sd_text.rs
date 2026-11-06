// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a `dns-sd` resolve's transcript says, read the same on this Mac (`lab mac nux`'s
//! `advert` module) and in the guest (`aterm-lab-guest dns-sd`): the hosts a `-L` names, the
//! addresses a `-G` names, and whether either has answered yet ([`answered`]).
//!
//! A resolve of an advert that was heard must answer, so a runner listens until it does, bounded
//! by its one-minute hang detector, rather than for a fixed window. The fixed two seconds ended
//! one resolve of this Mac's own vault advert over tart's bridge before its answer came, in E3's
//! vault row (2026-11-05, m22 at load average 9), and the advert read as a guest's.

use std::net::IpAddr;

/// The hosts a `dns-sd -L` transcript says its instance can be reached at
/// (`… can be reached at alab-secrets.local.:7484 (interface 25)`): lowercase, without the
/// trailing dot, each once. A host with other characters than a DNS name's is left out.
#[must_use]
pub fn reach_hosts(transcript: &str) -> Vec<String> {
    const REACHED: &str = " can be reached at ";
    let mut out: Vec<String> = Vec::new();
    for line in transcript.lines() {
        let Some((_, rest)) = line.split_once(REACHED) else {
            continue;
        };
        let target = rest.split(" (interface ").next().unwrap_or(rest).trim();
        let Some((host, _port)) = target.rsplit_once(':') else {
            continue;
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let named = !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'));
        if named && !out.contains(&host) {
            out.push(host);
        }
    }
    out
}

/// The fields of a `dns-sd -G` answer row (`Timestamp A/R Flags IF Hostname Address TTL`). A row
/// with more carries `dns-sd`'s word on an error (`No Such Record`, `Error code N`).
const ANSWER_FIELDS: usize = 7;

/// The addresses the `Add` rows of a `dns-sd -G v4v6 <host>` transcript name, each once: an IPv4
/// one as written, an IPv6 one without its scope (`FE80:…:C0EA%en0`, `…%<0>`). A negative answer
/// (`0.0.0.0 108 No Such Record`: mDNSResponder's word that the host has no address of that
/// family there), an error's row and an unspecified address name none.
#[must_use]
pub fn addresses(transcript: &str) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    for line in transcript.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.get(1) != Some(&"Add") || f.len() != ANSWER_FIELDS {
            continue;
        }
        let Some(address) = f
            .get(5)
            .and_then(|a| a.split('%').next())
            .and_then(|a| a.parse::<IpAddr>().ok())
        else {
            continue;
        };
        if !address.is_unspecified() && !out.contains(&address) {
            out.push(address);
        }
    }
    out
}

/// Whether the transcript of `dns-sd ARGS...` holds the answer its run waits for: a `-L` a host
/// it can be reached at ([`reach_hosts`]), a `-G` an address ([`addresses`]; a negative answer is
/// none). Any other run (a `-B` browse) listens for its window alone: what it should hear is not
/// known, so its window ends it.
#[must_use]
pub fn answered(args: &[String], transcript: &str) -> bool {
    match args.iter().find(|a| a.starts_with('-') && *a != "-i") {
        Some(op) if op == "-L" => !reach_hosts(transcript).is_empty(),
        Some(op) if op == "-G" => !addresses(transcript).is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    /// `dns-sd -G v4v6 m22.local` rows as m22 printed them (2026-11-05), and the negative answer
    /// `dns-sd` prints for a family the host has no record of (`addrinfo_reply`'s
    /// `No Such Record`, its address the zeroed sockaddr's).
    const NEGATIVE_V4: &str = " 8:43:01.120  Add  40000002       6  m22.local.                             0.0.0.0                                      120   No Such Record\r\n";
    const V6_ROWS: &str = " 8:47:30.395  Add  40000003       8  m22.local.                             FE80:0000:0000:0000:1C92:EBFC:C6EB:C0EA%en7  4500\r\n \
                           8:47:30.395  Add  40000003       8  m22.local.                             FD2A:6605:108E:463A:1C76:712F:4048:97E7%<0>  4500\r\n";
    const REACHED: &str = "Lookup m22._alab-vault._tcp.local.\r\n 9:12:01.001  ...STARTING...\r\n \
                           9:12:01.002  m22._alab-vault._tcp.local. can be reached at m22.local.:7484 (interface 25)\r\n";

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).collect()
    }

    /// A negative answer names no address, so an advert whose host only answered no is
    /// unresolved, never placed at `0.0.0.0`; an IPv6 row names its address without its scope.
    #[test]
    fn a_negative_answer_names_no_address_and_a_v6_row_names_its_own() {
        assert!(
            addresses(NEGATIVE_V4).is_empty(),
            "{:?}",
            addresses(NEGATIVE_V4)
        );
        // NEGATIVE CONTROL: the row read as the v4 reader read it (field 5 parsed, the error words
        // ignored) is the address `0.0.0.0` that failed E3's second run.
        let old: Vec<Ipv4Addr> = NEGATIVE_V4
            .lines()
            .filter_map(|l| l.split_whitespace().nth(5)?.parse().ok())
            .collect();
        assert_eq!(old, [Ipv4Addr::UNSPECIFIED]);

        let six = addresses(V6_ROWS);
        assert_eq!(
            six,
            [
                "fe80::1c92:ebfc:c6eb:c0ea".parse::<IpAddr>().unwrap(),
                "fd2a:6605:108e:463a:1c76:712f:4048:97e7".parse().unwrap(),
            ]
        );
        assert!(addresses(&format!("{NEGATIVE_V4}{V6_ROWS}")).len() == 2);
    }

    /// A resolve has answered once its transcript names a host (`-L`) or an address (`-G`): a
    /// start line alone, or only a negative answer, is no answer yet, so its runner listens on.
    /// A browse is never waited past its window.
    #[test]
    fn a_resolve_has_answered_only_once_it_names_a_host_or_an_address() {
        let lookup = words(&["-i", "25", "-L", "m22", "_alab-vault._tcp", "local."]);
        let started = "Lookup m22._alab-vault._tcp.local.\r\n 9:12:01.001  ...STARTING...\r\n";
        assert!(!answered(&lookup, started));
        assert!(!answered(&lookup, ""));
        assert!(answered(&lookup, REACHED));
        let get = words(&["-i", "25", "-G", "v4v6", "m22.local"]);
        assert!(!answered(&get, NEGATIVE_V4), "a negative answer alone");
        assert!(answered(&get, &format!("{NEGATIVE_V4}{V6_ROWS}")));
        // NEGATIVE CONTROL: a reader that took any `Add` row for an answer would stop on the
        // negative one, which is what read this Mac's advert as unplaced.
        assert!(NEGATIVE_V4.split_whitespace().nth(1) == Some("Add"));
        let browse = words(&["-B", "_alab-vault._tcp", "local."]);
        assert!(answered(&browse, ""), "a browse ends with its window");
    }
}
