use tracing::trace;

macro_rules! trace_scope {
    ($s:literal) => {
        let _crate = $crate::platform_impl::platform::util::TraceGuard::new(module_path!(), $s);
    };
}

// LOCAL PATCH (aterm): both fields are read ONLY inside the `trace!` calls
// below, and aterm patches `tracing` to a first-party no-op facade
// (`crates/aterm-tracing`) whose macros discard their arguments without
// evaluating them — which is what makes it behaviourally identical to the real
// facade under the `NoSubscriber` this process always has. The consequence is
// local and unavoidable: with no reader left, rustc correctly reports these two
// fields as dead. They are kept rather than deleted because they are upstream's
// and because restoring the real `tracing` must restore their readers with no
// further edit here. Scoped to this struct, so a genuinely dead field anywhere
// else in the fork still warns.
#[allow(dead_code, reason = "read only by the trace! calls aterm's tracing shim discards")]
pub(crate) struct TraceGuard {
    module_path: &'static str,
    called_from_fn: &'static str,
}

impl TraceGuard {
    #[inline]
    pub(crate) fn new(module_path: &'static str, called_from_fn: &'static str) -> Self {
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
