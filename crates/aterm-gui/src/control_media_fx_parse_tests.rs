// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::parse_fx_form;
use crate::FxCtlOp;
use aterm_effects::kitty_sing::{CelebrateOn, song_signature};

#[test]
fn fx_celebrate_parses_its_keys_in_any_order_with_defaults() {
    assert_eq!(parse_fx_form(""), Ok(FxCtlOp::Status));
    assert_eq!(parse_fx_form(" status "), Ok(FxCtlOp::Status));
    assert_eq!(
        parse_fx_form("celebrate"),
        Ok(FxCtlOp::Celebrate {
            sig: song_signature('C'),
            bars: 2,
            on: CelebrateOn::Green
        })
    );
    assert_eq!(
        parse_fx_form("celebrate bars=4 on=enter sig=g"),
        Ok(FxCtlOp::Celebrate {
            sig: song_signature('g'),
            bars: 4,
            on: CelebrateOn::Enter
        })
    );
    assert_eq!(
        parse_fx_form("celebrate sig=C bars=2"),
        Ok(FxCtlOp::Celebrate {
            sig: song_signature('C'),
            bars: 2,
            on: CelebrateOn::Green
        })
    );
    assert_eq!(
        parse_fx_form("celebrate on=done"),
        Ok(FxCtlOp::Celebrate {
            sig: song_signature('C'),
            bars: 2,
            on: CelebrateOn::Done
        })
    );
}

#[test]
fn fx_refuses_every_malformed_form() {
    for bad in [
        "fire",
        "status now",
        "celebrate loud",
        "celebrate key=C",
        "celebrate sig=CD",
        "celebrate sig=",
        "celebrate bars=0",
        "celebrate bars=5",
        "celebrate bars=two",
        "celebrate on=red",
        "celebrate on=DONE",
        "celebrate on=done on=green",
        "celebrate bars=2 bars=3",
    ] {
        assert!(parse_fx_form(bad).is_err(), "{bad:?} must be refused");
    }
    let usage = parse_fx_form("celebrate on=red").expect_err("refused");
    assert!(usage.contains("on=green|enter|done"), "{usage}");
}
