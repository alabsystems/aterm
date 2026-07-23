// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for aterm-error: derive macro + Context trait + ad-hoc macros.

use crate::{Context, Error};

// ── Derive macro tests ──────────────────────────────────────────────────────

#[derive(Debug, Error)]
enum SimpleError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("permission denied")]
    PermissionDenied,

    #[error("invalid field '{field}': {reason}")]
    InvalidField { field: String, reason: String },
}

#[test]
fn test_derive_display_tuple_variant() {
    let err = SimpleError::NotFound("foo.txt".into());
    assert_eq!(err.to_string(), "not found: foo.txt");
}

#[test]
fn test_derive_display_unit_variant() {
    let err = SimpleError::PermissionDenied;
    assert_eq!(err.to_string(), "permission denied");
}

#[test]
fn test_derive_display_named_fields() {
    let err = SimpleError::InvalidField {
        field: "name".into(),
        reason: "too long".into(),
    };
    assert_eq!(err.to_string(), "invalid field 'name': too long");
}

#[test]
fn test_derive_error_trait_no_source() {
    let err = SimpleError::PermissionDenied;
    assert!(std::error::Error::source(&err).is_none());
}

// ── #[from] tests ───────────────────────────────────────────────────────────

#[derive(Debug, Error)]
enum IoWrapperError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("other: {0}")]
    #[allow(dead_code)]
    Other(String),
}

#[test]
fn test_derive_from_impl() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
    let wrapped: IoWrapperError = io_err.into();
    assert_eq!(wrapped.to_string(), "I/O error: gone");
}

#[test]
fn test_derive_source_from_field() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
    let wrapped: IoWrapperError = io_err.into();
    let source = std::error::Error::source(&wrapped);
    assert!(source.is_some());
    assert_eq!(source.unwrap().to_string(), "gone");
}

// ── #[source] tests ─────────────────────────────────────────────────────────

#[derive(Debug, Error)]
enum SourceError {
    #[error("device error: {msg}")]
    Device {
        msg: String,
        #[source]
        cause: std::io::Error,
    },
}

#[test]
fn test_derive_source_named_field() {
    let io_err = std::io::Error::other("disk full");
    let err = SourceError::Device {
        msg: "write failed".into(),
        cause: io_err,
    };
    assert_eq!(err.to_string(), "device error: write failed");
    let source = std::error::Error::source(&err).unwrap();
    assert_eq!(source.to_string(), "disk full");
}

// ── #[error(transparent)] tests ─────────────────────────────────────────────

#[derive(Debug, Error)]
enum TransparentError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[test]
fn test_derive_transparent_display_delegates() {
    let io_err = std::io::Error::other("bad stuff");
    let wrapped: TransparentError = io_err.into();
    assert_eq!(wrapped.to_string(), "bad stuff");
}

#[test]
fn test_derive_transparent_source_delegates() {
    let io_err = std::io::Error::other("bad stuff");
    let wrapped: TransparentError = io_err.into();
    // For transparent, source returns the inner error
    let source = std::error::Error::source(&wrapped);
    assert!(source.is_some());
}

// ── Context trait tests ─────────────────────────────────────────────────────

#[test]
fn test_context_result_ok_passes_through() {
    let result: Result<i32, std::io::Error> = Ok(42);
    let contextualized = result.context("should not fail");
    assert_eq!(contextualized.unwrap(), 42);
}

#[test]
fn test_context_result_err_wraps() {
    let result: Result<i32, std::io::Error> =
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"));
    let err = result.context("failed to read config").unwrap_err();
    assert_eq!(err.to_string(), "failed to read config");
    // The original error is the source
    let source = std::error::Error::source(&*err).unwrap();
    assert_eq!(source.to_string(), "missing");
}

#[test]
fn test_with_context_lazy_evaluation() {
    let result: Result<i32, std::io::Error> = Err(std::io::Error::other("fail"));
    let err = result
        .with_context(|| format!("context for {}", "test"))
        .unwrap_err();
    assert_eq!(err.to_string(), "context for test");
}

#[test]
fn test_context_option_some_passes_through() {
    let opt: Option<i32> = Some(99);
    let result = opt.context("should have value");
    assert_eq!(result.unwrap(), 99);
}

#[test]
fn test_context_option_none_produces_error() {
    let opt: Option<i32> = None;
    let err = opt.context("missing value").unwrap_err();
    assert_eq!(err.to_string(), "missing value");
    assert!(std::error::Error::source(&*err).is_none());
}

// ── Clone/PartialEq derive combinations ─────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Error)]
enum CloneableError {
    #[error("code {0}")]
    Code(u32),

    #[error("message: {0}")]
    #[allow(dead_code)]
    Message(String),
}

#[test]
fn test_derive_with_clone_and_eq() {
    let err = CloneableError::Code(404);
    let cloned = err.clone();
    assert_eq!(err, cloned);
    assert_eq!(cloned.to_string(), "code 404");
}

// ── Debug format in error messages ──────────────────────────────────────────

#[derive(Debug, Error)]
enum DebugFormatError {
    #[error("invalid mode: {0:?}")]
    InvalidMode(String),
}

#[test]
fn test_derive_debug_format_specifier() {
    let err = DebugFormatError::InvalidMode("strict".into());
    assert_eq!(err.to_string(), r#"invalid mode: "strict""#);
}

// ── Hex format in error messages ────────────────────────────────────────────

#[derive(Debug, Error)]
enum HexFormatError {
    #[error("invalid base64 character: 0x{0:02X}")]
    InvalidChar(u8),
}

#[test]
fn test_derive_hex_format_specifier() {
    let err = HexFormatError::InvalidChar(0xFF);
    assert_eq!(err.to_string(), "invalid base64 character: 0xFF");
}

// ── Struct-level derive tests ───────────────────────────────────────────────

#[derive(Debug, Error)]
#[error("connection refused: {host}:{port}")]
struct ConnectionError {
    host: String,
    port: u16,
}

#[test]
fn test_derive_struct_named_fields() {
    let err = ConnectionError {
        host: "localhost".into(),
        port: 5432,
    };
    assert_eq!(err.to_string(), "connection refused: localhost:5432");
    assert!(std::error::Error::source(&err).is_none());
}

#[derive(Debug, Error)]
#[error("parse error at offset {0}")]
struct ParseError(usize);

#[test]
fn test_derive_struct_tuple() {
    let err = ParseError(42);
    assert_eq!(err.to_string(), "parse error at offset 42");
    assert!(std::error::Error::source(&err).is_none());
}

#[derive(Debug, Error)]
#[error("config load failed")]
struct ConfigError {
    #[source]
    cause: std::io::Error,
}

#[test]
fn test_derive_struct_with_source() {
    let err = ConfigError {
        cause: std::io::Error::other("file not found"),
    };
    assert_eq!(err.to_string(), "config load failed");
    let source = std::error::Error::source(&err).unwrap();
    assert_eq!(source.to_string(), "file not found");
}

#[derive(Debug, Error)]
#[error(transparent)]
struct WrappedIoError(#[from] std::io::Error);

#[test]
fn test_derive_struct_transparent() {
    let io_err = std::io::Error::other("boom");
    let err: WrappedIoError = io_err.into();
    assert_eq!(err.to_string(), "boom");
    assert!(std::error::Error::source(&err).is_some());
}

// ── Ad-hoc error macro tests ────────────────────────────────────────────────

#[test]
fn test_err_macro_creates_error() {
    let e = err!("port {} is invalid", 0);
    assert_eq!(e.to_string(), "port 0 is invalid");
}

#[test]
fn test_bail_macro_returns_early() {
    fn might_fail(ok: bool) -> crate::Result<()> {
        if !ok {
            bail!("something went wrong");
        }
        Ok(())
    }
    assert!(might_fail(true).is_ok());
    let err = might_fail(false).unwrap_err();
    assert_eq!(err.to_string(), "something went wrong");
}

#[test]
fn test_ensure_macro_passes_when_true() {
    fn check(n: i32) -> crate::Result<()> {
        ensure!(n > 0, "n must be positive, got {}", n);
        Ok(())
    }
    assert!(check(1).is_ok());
    let err = check(-1).unwrap_err();
    assert_eq!(err.to_string(), "n must be positive, got -1");
}

#[test]
fn test_result_type_alias() {
    fn parse_port(s: &str) -> crate::Result<u16> {
        let port: u16 = s
            .parse()
            .map_err(|e: std::num::ParseIntError| err!("{e}"))?;
        ensure!(port > 0, "port must be nonzero");
        Ok(port)
    }
    assert_eq!(parse_port("8080").unwrap(), 8080);
    assert!(parse_port("abc").is_err());
    assert!(parse_port("0").is_err());
}

#[test]
fn test_context_with_box_error_result() {
    fn inner() -> crate::Result<i32> {
        let result: Result<i32, std::io::Error> = Err(std::io::Error::other("disk full"));
        result.context("backup failed")
    }
    let err = inner().unwrap_err();
    assert_eq!(err.to_string(), "backup failed");
}
