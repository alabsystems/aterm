// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/util.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use tracing::trace;

macro_rules! trace_scope {
    ($s:literal) => {
        let _crate = $crate::platform_impl::platform::util::TraceGuard::new(module_path!(), $s);
    };
}

#[allow(dead_code, reason = "read only by the trace! calls aterm's tracing shim discards")]
pub(crate) struct TraceGuard {
    module_path: &'static str,
    called_from_fn: &'static str,
}

impl TraceGuard {
    #[inline]
    pub(crate) const fn new(module_path: &'static str, called_from_fn: &'static str) -> Self {
        trace!(target = module_path, "Triggered `{}`", called_from_fn);
        Self { module_path, called_from_fn }
    }
}

impl Drop for TraceGuard {
    #[inline]
    fn drop(&mut self) {
        trace!(target = self.module_path, "Completed `{}`", self.called_from_fn);
    }
}
