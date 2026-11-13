// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

std::thread_local! {
    pub(super) static STOP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(super) static REACHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(super) static LOOPBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(super) static EXCHANGES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
pub(super) fn stop() -> bool {
    REACHED.with(|reached| reached.set(true));
    STOP.with(std::cell::Cell::get)
}

pub(super) fn drive(
    request: &super::super::login_drive::Request<'_>,
    home: &std::path::Path,
    native: &super::super::codex_enrollment::NativeContext<'_>,
    prepared: super::super::codex_grant_owner::PreparedGrant,
) -> (
    super::super::login_drive::Drove,
    Option<super::super::codex_grant_owner::CaptureOutcome>,
) {
    #[cfg(unix)]
    if LOOPBACK.with(std::cell::Cell::get) {
        let (drove, outcome) = super::super::codex_app_login::tests::current_global_login_fixture(
            request.enrollment.expect("actual native artifact"),
            prepared,
            native.scope,
            None,
            true,
            false,
            native.env,
        );
        EXCHANGES.with(|count| count.set(count.get() + 1));
        return (drove, Some(outcome));
    }
    super::super::login_drive::sign_in_live_scoped_custody(
        request,
        home,
        &|name| std::env::var_os(name),
        Some(native.scope.clone()),
        prepared,
    )
    .into_parts()
}
