// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn a_carry_crosses_the_wire_form_intact() {
    let carry = ParserCarry {
        state: State::CsiParam,
        params: vec![38, 5],
        subparam_mask: 0b10,
        current_param: 19,
        param_started: true,
        last_was_colon: true,
        intermediates: vec![b'?'],
        ..ParserCarry::default()
    };
    let repr = ParserRepr::from_carry(&carry);
    assert_eq!(repr.intermediates, "3f");
    assert_eq!(repr.to_carry(), Some(carry));
    let osc = ParserCarry {
        state: State::OscString,
        osc: b"0;t\xff".to_vec(),
        ..ParserCarry::default()
    };
    assert_eq!(ParserRepr::from_carry(&osc).to_carry(), Some(osc));
    let string = ParserCarry {
        state: State::SosPmApcString,
        string_utf8_tail: vec![0xE6],
        ..ParserCarry::default()
    };
    let repr = ParserRepr::from_carry(&string);
    assert_eq!(repr.string_utf8_tail, "e6");
    assert_eq!(repr.to_carry(), Some(string));
    for (carry, name) in [
        (
            ParserCarry {
                state: State::DcsIgnore,
                tmux: TmuxPassthrough::Head(3),
                ..ParserCarry::default()
            },
            "head3",
        ),
        (
            ParserCarry {
                state: State::CsiParam,
                params: vec![1],
                current_param: 2,
                param_started: true,
                tmux: TmuxPassthrough::Escape,
                ..ParserCarry::default()
            },
            "escape",
        ),
    ] {
        let repr = ParserRepr::from_carry(&carry);
        assert_eq!(repr.tmux, name);
        assert_eq!(repr.to_carry(), Some(carry));
    }
    let unknown = ParserRepr {
        state: "Ground".into(),
        tmux: "head9".into(),
        ..ParserRepr::default()
    };
    assert_eq!(unknown.to_carry(), None, "a wrapper no build writes");
}

#[test]
fn a_dcs_header_rides_beside_the_form_an_older_build_reads() {
    let header = ParserCarry {
        state: State::DcsIntermediate,
        params: vec![1],
        current_param: 2,
        param_started: true,
        intermediates: vec![b'$'],
        ..ParserCarry::default()
    };
    let repr = ParserRepr::from_carry(&header);
    let older_reads = ParserRepr {
        exact: None,
        ..repr.clone()
    };
    assert_eq!(
        older_reads,
        ParserRepr {
            state: "DcsIgnore".into(),
            ..ParserRepr::default()
        },
        "nothing beside `exact` that an older build's restore refuses"
    );
    let older_carry = older_reads.to_carry().expect("reads");
    assert_eq!(header.for_older_builds(), Some(older_carry.clone()));
    aterm_parser::Parser::new()
        .restore_carry(&older_carry)
        .expect("the older form restores");
    assert_eq!(repr.to_carry(), Some(header.clone()), "this build reads it");
    let mut parser = aterm_parser::Parser::new();
    parser
        .restore_carry(&repr.to_carry().expect("reads"))
        .expect("the header restores");
    assert_eq!(parser.state(), State::DcsIntermediate);

    let older_producer = ParserRepr {
        state: "DcsIgnore".into(),
        ..ParserRepr::default()
    };
    assert_eq!(older_producer.to_carry(), Some(older_carry));

    for carry in [
        ParserCarry {
            state: State::CsiParam,
            params: vec![38],
            ..ParserCarry::default()
        },
        ParserCarry {
            state: State::DcsIgnore,
            ..ParserCarry::default()
        },
    ] {
        let repr = ParserRepr::from_carry(&carry);
        assert_eq!(repr.exact, None, "{carry:?}");
        assert_eq!(repr.to_carry(), Some(carry));
    }
}

#[test]
fn a_wire_form_that_does_not_read_is_none() {
    for repr in [
        ParserRepr {
            state: "Nowhere".into(),
            ..ParserRepr::default()
        },
        ParserRepr {
            state: "Ground".into(),
            utf8_tail: "e".into(),
            ..ParserRepr::default()
        },
        ParserRepr {
            state: "OscString".into(),
            osc: "zz".into(),
            ..ParserRepr::default()
        },
    ] {
        assert_eq!(repr.to_carry(), None, "{repr:?}");
    }
    assert!(
        ParserRepr {
            state: "Ground".into(),
            utf8_tail: "e6".into(),
            utf8_expected: 3,
            ..ParserRepr::default()
        }
        .to_carry()
        .is_some()
    );
}
