// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`Error`] answers as the crates.io getrandom 0.2.17 gave them, recorded on aarch64-apple-darwin
//! before the `[patch.crates-io]` row replaced it (the recorder was an out-of-workspace crate
//! pinning `getrandom = "=0.2.17"`, which the patch table cannot reach): each named constant's
//! code, then per code `raw_os_error`, `Display`, `Debug`, and the `std::io::Error` it converts to.

use core::num::NonZeroU32;

use getrandom::Error;

#[test]
fn the_named_codes_are_upstreams() {
    let named: [(&str, Error, u32); 13] = [
        ("UNSUPPORTED", Error::UNSUPPORTED, 2147483648),
        ("ERRNO_NOT_POSITIVE", Error::ERRNO_NOT_POSITIVE, 2147483649),
        ("UNEXPECTED", Error::UNEXPECTED, 2147483650),
        ("IOS_SEC_RANDOM", Error::IOS_SEC_RANDOM, 2147483651),
        (
            "WINDOWS_RTL_GEN_RANDOM",
            Error::WINDOWS_RTL_GEN_RANDOM,
            2147483652,
        ),
        ("FAILED_RDRAND", Error::FAILED_RDRAND, 2147483653),
        ("NO_RDRAND", Error::NO_RDRAND, 2147483654),
        ("WEB_CRYPTO", Error::WEB_CRYPTO, 2147483655),
        (
            "WEB_GET_RANDOM_VALUES",
            Error::WEB_GET_RANDOM_VALUES,
            2147483656,
        ),
        (
            "VXWORKS_RAND_SECURE",
            Error::VXWORKS_RAND_SECURE,
            2147483659,
        ),
        ("NODE_CRYPTO", Error::NODE_CRYPTO, 2147483660),
        (
            "NODE_RANDOM_FILL_SYNC",
            Error::NODE_RANDOM_FILL_SYNC,
            2147483661,
        ),
        ("NODE_ES_MODULE", Error::NODE_ES_MODULE, 2147483662),
    ];
    for (name, err, code) in named {
        assert_eq!(err.code().get(), code, "{name}");
    }
    assert_eq!(Error::INTERNAL_START, 2147483648);
    assert_eq!(Error::CUSTOM_START, 3221225472);
}

/// `(code, raw_os_error, Display, Debug, io raw_os_error, io Display)`.
type Row = (
    u32,
    Option<i32>,
    &'static str,
    &'static str,
    Option<i32>,
    &'static str,
);

/// The internal and custom ranges, platform-independent. The io `Display` is
/// the error's own (a custom io error).
const INTERNAL: &[Row] = &[
    (
        2147483648,
        None,
        "getrandom: this target is not supported",
        "Error { internal_code: 2147483648, description: \"getrandom: this target is not supported\" }",
        None,
        "getrandom: this target is not supported",
    ),
    (
        2147483649,
        None,
        "errno: did not return a positive value",
        "Error { internal_code: 2147483649, description: \"errno: did not return a positive value\" }",
        None,
        "errno: did not return a positive value",
    ),
    (
        2147483650,
        None,
        "unexpected situation",
        "Error { internal_code: 2147483650, description: \"unexpected situation\" }",
        None,
        "unexpected situation",
    ),
    (
        2147483651,
        None,
        "SecRandomCopyBytes: iOS Security framework failure",
        "Error { internal_code: 2147483651, description: \"SecRandomCopyBytes: iOS Security framework failure\" }",
        None,
        "SecRandomCopyBytes: iOS Security framework failure",
    ),
    (
        2147483652,
        None,
        "RtlGenRandom: Windows system function failure",
        "Error { internal_code: 2147483652, description: \"RtlGenRandom: Windows system function failure\" }",
        None,
        "RtlGenRandom: Windows system function failure",
    ),
    (
        2147483653,
        None,
        "RDRAND: failed multiple times: CPU issue likely",
        "Error { internal_code: 2147483653, description: \"RDRAND: failed multiple times: CPU issue likely\" }",
        None,
        "RDRAND: failed multiple times: CPU issue likely",
    ),
    (
        2147483654,
        None,
        "RDRAND: instruction not supported",
        "Error { internal_code: 2147483654, description: \"RDRAND: instruction not supported\" }",
        None,
        "RDRAND: instruction not supported",
    ),
    (
        2147483655,
        None,
        "Web Crypto API is unavailable",
        "Error { internal_code: 2147483655, description: \"Web Crypto API is unavailable\" }",
        None,
        "Web Crypto API is unavailable",
    ),
    (
        2147483656,
        None,
        "Calling Web API crypto.getRandomValues failed",
        "Error { internal_code: 2147483656, description: \"Calling Web API crypto.getRandomValues failed\" }",
        None,
        "Calling Web API crypto.getRandomValues failed",
    ),
    (
        2147483659,
        None,
        "randSecure: VxWorks RNG module is not initialized",
        "Error { internal_code: 2147483659, description: \"randSecure: VxWorks RNG module is not initialized\" }",
        None,
        "randSecure: VxWorks RNG module is not initialized",
    ),
    (
        2147483660,
        None,
        "Node.js crypto CommonJS module is unavailable",
        "Error { internal_code: 2147483660, description: \"Node.js crypto CommonJS module is unavailable\" }",
        None,
        "Node.js crypto CommonJS module is unavailable",
    ),
    (
        2147483661,
        None,
        "Calling Node.js API crypto.randomFillSync failed",
        "Error { internal_code: 2147483661, description: \"Calling Node.js API crypto.randomFillSync failed\" }",
        None,
        "Calling Node.js API crypto.randomFillSync failed",
    ),
    (
        2147483662,
        None,
        "Node.js ES modules are not directly supported, see https://docs.rs/getrandom#nodejs-es-module-support",
        "Error { internal_code: 2147483662, description: \"Node.js ES modules are not directly supported, see https://docs.rs/getrandom#nodejs-es-module-support\" }",
        None,
        "Node.js ES modules are not directly supported, see https://docs.rs/getrandom#nodejs-es-module-support",
    ),
    (
        2147483657,
        None,
        "Unknown Error: 2147483657",
        "Error { unknown_code: 2147483657 }",
        None,
        "Unknown Error: 2147483657",
    ),
    (
        2147483658,
        None,
        "Unknown Error: 2147483658",
        "Error { unknown_code: 2147483658 }",
        None,
        "Unknown Error: 2147483658",
    ),
    (
        2147483663,
        None,
        "Unknown Error: 2147483663",
        "Error { unknown_code: 2147483663 }",
        None,
        "Unknown Error: 2147483663",
    ),
    (
        3221225471,
        None,
        "Unknown Error: 3221225471",
        "Error { unknown_code: 3221225471 }",
        None,
        "Unknown Error: 3221225471",
    ),
    (
        3221225472,
        None,
        "Unknown Error: 3221225472",
        "Error { unknown_code: 3221225472 }",
        None,
        "Unknown Error: 3221225472",
    ),
    (
        3221225479,
        None,
        "Unknown Error: 3221225479",
        "Error { unknown_code: 3221225479 }",
        None,
        "Unknown Error: 3221225479",
    ),
    (
        4294967295,
        None,
        "Unknown Error: 4294967295",
        "Error { unknown_code: 4294967295 }",
        None,
        "Unknown Error: 4294967295",
    ),
];

/// OS error numbers whose Darwin and glibc `strerror` descriptions match upstream's recording.
#[cfg(any(target_vendor = "apple", all(target_os = "linux", target_env = "gnu")))]
const OS_RECORDED: &[Row] = &[
    (
        2147483647,
        Some(2147483647),
        "OS Error: 2147483647",
        "Error { os_error: 2147483647 }",
        Some(2147483647),
        "",
    ),
    (
        1,
        Some(1),
        "Operation not permitted",
        "Error { os_error: 1, description: \"Operation not permitted\" }",
        Some(1),
        "",
    ),
    (
        2,
        Some(2),
        "No such file or directory",
        "Error { os_error: 2, description: \"No such file or directory\" }",
        Some(2),
        "",
    ),
    (
        4,
        Some(4),
        "Interrupted system call",
        "Error { os_error: 4, description: \"Interrupted system call\" }",
        Some(4),
        "",
    ),
    (
        5,
        Some(5),
        "Input/output error",
        "Error { os_error: 5, description: \"Input/output error\" }",
        Some(5),
        "",
    ),
    (
        13,
        Some(13),
        "Permission denied",
        "Error { os_error: 13, description: \"Permission denied\" }",
        Some(13),
        "",
    ),
];

fn check(rows: &[Row]) {
    for &(code, raw, display, debug, io_raw, io_display) in rows {
        let err = Error::from(NonZeroU32::new(code).expect("recorded codes are non-zero"));
        assert_eq!(err.code().get(), code);
        assert_eq!(err.raw_os_error(), raw, "{code}");
        assert_eq!(err.to_string(), display, "{code}");
        assert_eq!(format!("{err:?}"), debug, "{code}");
        let io = std::io::Error::from(err);
        match io_raw {
            Some(errno) => assert_eq!(
                io.kind(),
                std::io::Error::from_raw_os_error(errno).kind(),
                "{code}"
            ),
            None => assert_eq!(io.kind(), std::io::ErrorKind::Other, "{code}"),
        }
        assert_eq!(io.raw_os_error(), io_raw, "{code}");
        if !io_display.is_empty() {
            assert_eq!(io.to_string(), io_display, "{code}");
        }
    }
}

#[test]
fn internal_and_custom_codes_answer_as_upstream() {
    check(INTERNAL);
}

#[cfg(unix)]
#[test]
fn os_codes_answer_as_upstream() {
    check(&[(
        2147483647,
        Some(2147483647),
        "OS Error: 2147483647",
        "Error { os_error: 2147483647 }",
        Some(2147483647),
        "",
    )]);
    for errno in [1i32, 2, 4, 5, 13] {
        let code = u32::try_from(errno).expect("positive errno");
        let err = Error::from(NonZeroU32::new(code).expect("non-zero errno"));
        assert_eq!(err.raw_os_error(), Some(errno));
        let host_io = std::io::Error::from_raw_os_error(errno);
        let suffix = format!(" (os error {errno})");
        let host_text = host_io.to_string();
        let host_desc = host_text
            .strip_suffix(&suffix)
            .filter(|d| !d.is_empty() && !d.starts_with("Unknown error"));
        match host_desc {
            Some(desc) => {
                assert_eq!(err.to_string(), desc, "{errno}");
                assert_eq!(
                    format!("{err:?}"),
                    format!("Error {{ os_error: {errno}, description: {desc:?} }}"),
                    "{errno}"
                );
            }
            None => {
                assert_eq!(err.to_string(), format!("OS Error: {errno}"), "{errno}");
                assert_eq!(
                    format!("{err:?}"),
                    format!("Error {{ os_error: {errno} }}"),
                    "{errno}"
                );
            }
        }
        let io = std::io::Error::from(err);
        assert_eq!(io.raw_os_error(), Some(errno), "{errno}");
        assert_eq!(io.kind(), host_io.kind(), "{errno}");
    }
    #[cfg(any(target_vendor = "apple", all(target_os = "linux", target_env = "gnu")))]
    check(OS_RECORDED);
}
