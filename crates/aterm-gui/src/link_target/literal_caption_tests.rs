// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_dotted_address_is_one_identity_not_dns_labels() {
    assert!(
        "2001:db8::ffff:192.0.2.1"
            .parse::<std::net::Ipv6Addr>()
            .is_ok()
    );
    let cap = caption("https://[2001:db8::ffff:192.0.2.1]:8443/x", 100);
    assert_eq!(cap.host_text(), "[2001:db8::ffff:192.0.2.1]:8443");
    assert_eq!(cap.emphasis_text(), cap.host_text());
}

#[test]
fn an_elided_address_keeps_its_identity_after_the_bracket_is_lost() {
    let url = "https://[2001:db8::ffff:192.0.2.1]:8443/x";
    let mut covered = 0;
    for width in 8..40 {
        let cap = caption(url, width);
        if cap.text.starts_with(ELLIPSIS) && !cap.host_text().starts_with('[') {
            covered += 1;
            assert_eq!(cap.emphasis(), cap.host, "width={width} cap={cap:?}");
        }
    }
    assert!(covered > 0);
}

#[test]
fn bracketed_identity_survives_ports_userinfo_scopes_and_every_elision_width() {
    for prefix in ["::ffff:", "2001:db8::"] {
        for tail in 0..16 {
            let address = format!("{prefix}192.0.2.{tail}");
            assert!(address.parse::<std::net::Ipv6Addr>().is_ok());
            for zone in ["", "%25en0.100"] {
                for port in ["", ":8443"] {
                    for userinfo in ["", "google.com@"] {
                        let url = format!("https://{userinfo}[{address}{zone}]{port}/x");
                        for width in 0..=100 {
                            let cap = caption(&url, width);
                            assert_eq!(cap.emphasis(), cap.host, "width={width} cap={cap:?}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn only_the_original_host_controls_the_emphasis_rule() {
    for url in [
        "https://[::ffff:192.0.2.1]@www.evil.example/x",
        "https://www.evil.example/[::ffff:192.0.2.1]",
        "https://www.evil.example/?next=[::ffff:192.0.2.1]",
    ] {
        let cap = caption(url, 200);
        assert_eq!(cap.emphasis_text(), "evil.example", "{url}");
    }
    let cap = caption("https://aa.bb.cc.dd/x", 100);
    assert_eq!(
        cap.emphasis_text(),
        "bb.cc.dd",
        "hex-looking DNS labels stay labels"
    );
    let cap = caption("https://[v1.a.b.c]:8443/x", 100);
    assert_eq!(
        cap.emphasis_text(),
        "[v1.a.b.c]:8443",
        "brackets are structural, not an IPv6 parser"
    );
}

#[test]
fn unread_authorities_do_not_gain_an_emphasis_run() {
    let url = format!("https://[{}192.0.2.1]/x", "a".repeat(SCAN_BYTES));
    let cap = caption(&url, 40);
    assert!(cap.host.is_empty());
    assert!(cap.emphasis().is_empty());
    assert_eq!(cap.text, "https:…");
}

#[test]
fn the_entire_visible_address_and_port_are_bold_in_the_row() {
    let url = "https://[2001:db8::ffff:192.0.2.1]:8443/x";
    for cols in [20, 36, 50, 80, 160] {
        for tracking in [false, true] {
            for seam in [false, true] {
                let body = cols - MARGIN * 2;
                let lead = if tracking { OPENS_TRACKED } else { OPENS };
                let with_lead = body.saturating_sub(lead.len() + GAP);
                let budget = if with_lead >= LEAD_IN_URL_CELLS {
                    with_lead
                } else {
                    body
                };
                let cap = caption(url, budget);
                let row = caption_row(url, cols, Theme::default(), seam, tracking).unwrap();
                let bold: String = row
                    .iter()
                    .filter(|cell| cell.bold)
                    .map(|cell| cell.ch)
                    .collect();
                assert_eq!(
                    bold,
                    cap.host_text(),
                    "cols={cols} tracking={tracking} seam={seam}"
                );
            }
        }
    }
}
