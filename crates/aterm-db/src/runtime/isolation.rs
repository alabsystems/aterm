// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private native attachment for an ordinary executable's startup measurement.
//! Neither local custody nor the automatic runtime is replaced by the lab.

use std::ffi::OsStr;

pub(super) fn port() -> crate::Result<Option<u16>> {
    parse(
        std::env::var_os(super::LOOPBACK_PORT_ENV).as_deref(),
        std::env::var_os(aterm_net::loopback::SEAM).as_deref(),
    )
}

fn parse(value: Option<&OsStr>, guard: Option<&OsStr>) -> crate::Result<Option<u16>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let port = value
        .to_str()
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|port| *port != 0);
    match (port, guard) {
        (Some(port), Some(guard)) if guard == "1" => Ok(Some(port)),
        _ => Err(crate::Error(format!(
            "{} requires a nonzero port and {}=1; refusing the attachment",
            super::LOOPBACK_PORT_ENV,
            aterm_net::loopback::SEAM,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invalid_or_unguarded_private_attachment_never_falls_back_to_the_lan() {
        for value in ["", "0", "65536", "private", "-1"] {
            assert!(parse(Some(OsStr::new(value)), Some(OsStr::new("1"))).is_err());
        }
        for guard in [None, Some(OsStr::new("0")), Some(OsStr::new(""))] {
            assert!(parse(Some(OsStr::new("49152")), guard).is_err());
        }
        assert_eq!(parse(None, None).unwrap(), None);
        assert_eq!(
            parse(Some(OsStr::new("49152")), Some(OsStr::new("1"))).unwrap(),
            Some(49152)
        );
    }
}
