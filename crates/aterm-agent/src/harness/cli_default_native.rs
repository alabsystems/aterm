// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

impl shared_provider::LoginProof for super::super::agy_signin::NativeProfile {
    fn identity(&self) -> &str {
        &self.profile().email
    }
    fn principal(&self) -> [u8; 32] {
        self.profile().principal
    }
    fn subscription(&self) -> Option<&str> {
        None
    }
    fn check_current(&self) -> Result<(), String> {
        self.check_current().map_err(str::to_owned)
    }
}

#[cfg(not(test))]
pub(super) fn observe(
    cli: Cli,
    home: &Path,
    row: &accounts::Row,
    now: i64,
) -> Result<Box<dyn shared_provider::LoginProof>, String> {
    let expected = row
        .expected_identity_email()
        .ok_or("default account has no expected identity")?;
    let deadline = std::time::Instant::now() + AUTH_CHECK_BUDGET;
    match cli {
        Cli::Kimi => shared_provider::verify_native_kimi_classified(
            home,
            row.vendor
                .region
                .ok_or("default Kimi account has no region")?,
            expected,
            now,
            deadline,
        )
        .map_err(|error| error.to_string())?
        .map(|proof| Box::new(proof) as Box<dyn shared_provider::LoginProof>)
        .ok_or_else(|| "default Kimi credential is absent".into()),
        Cli::Agy => super::super::agy_signin::read_native_profile(home, expected, deadline)
            .map(|proof| Box::new(proof) as Box<dyn shared_provider::LoginProof>)
            .map_err(|error| error.to_string()),
        _ => Err("provider has no native default observation".into()),
    }
}
