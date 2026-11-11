// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn lenient_decode_refuses_malformed_input() {
    assert_eq!(decode("SGV!bG8="), Err(DecodeError::InvalidByte(3, b'!')));
    assert_eq!(decode("A"), Err(DecodeError::InvalidLength(1)));
    for bad in ["Zg=====", "Zg===", "====", "==="] {
        assert!(decode(bad).is_err(), "{bad}");
    }
}
