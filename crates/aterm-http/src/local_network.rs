// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether macOS's Local Network privacy refuses this process a `.local` name.
//!
//! An app a person answered *Don't Allow* (or whose switch is off in System
//! Settings > Privacy & Security > Local Network) gets every mDNS query refused
//! with `kDNSServiceErr_PolicyDenied`, which `getaddrinfo` reports as a definite
//! "nodename nor servname provided": the same words as a name nobody on the
//! network has. Only mDNSResponder tells the two apart, so after a failed
//! lookup of a local name the connector asks it once (measured on macOS 27.0:
//! the refusal reaches a query's callback within milliseconds of the query,
//! while a name nobody has stays silent, and a question still on screen holds
//! the query unanswered).

use std::time::Duration;

/// Whether `host` is a local DNS name (`.local`, or `.local.`), the one kind of
/// name Local Network privacy gates (TN3179: a non-local name resolves through
/// the system resolver without it).
pub(crate) fn is_local_name(host: &str) -> bool {
    let name = host.strip_suffix('.').unwrap_or(host);
    name.len() > ".local".len()
        && name.is_char_boundary(name.len() - ".local".len())
        && name[name.len() - ".local".len()..].eq_ignore_ascii_case(".local")
}

/// Whether mDNSResponder refuses this process an mDNS query for `host`,
/// waiting at most `within` for its first word on it; anything else (an
/// answer, silence, a query that cannot be made, no word in time) is no
/// refusal.
#[cfg(target_os = "macos")]
pub(crate) fn denied(host: &str, within: Duration) -> bool {
    asked_within(host, within, mdns::refused)
}

/// `ask(host, within)` on a helper thread, its answer if it comes within
/// `within`, else no: making a query talks to mDNSResponder before any reply
/// can be waited for (a connect, then the daemon's acknowledgement, which
/// dns_sd waits up to 10 s for), so only a thread keeps the caller to its
/// wait, as `stream::resolve_within` does for `getaddrinfo`. A late answer is
/// dropped with its thread; a thread that cannot start is no.
#[cfg(any(target_os = "macos", test))]
fn asked_within(
    host: &str,
    within: Duration,
    ask: impl FnOnce(&str, Duration) -> bool + Send + 'static,
) -> bool {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let host = host.to_owned();
    let spawned = std::thread::Builder::new()
        .name("aterm-http-mdns".into())
        .spawn(move || {
            // The receiver may be gone (the wait ended): the answer is dropped.
            let _ = tx.send(ask(&host, within));
        });
    spawned.is_ok() && rx.recv_timeout(within).unwrap_or(false)
}

/// No other platform gates mDNS per app.
#[cfg(not(target_os = "macos"))]
pub(crate) fn denied(_host: &str, _within: Duration) -> bool {
    false
}

/// dns_sd (`<dns_sd.h>`, in libSystem), hand-rolled like the crate's other
/// platform calls.
#[cfg(target_os = "macos")]
mod mdns {
    use std::cell::Cell;
    use std::ffi::{CString, c_char, c_void};
    use std::time::{Duration, Instant};

    type DnsServiceRef = *mut c_void;
    type DnsServiceErrorType = i32;
    type QueryRecordReply = unsafe extern "C" fn(
        DnsServiceRef,
        u32,
        u32,
        DnsServiceErrorType,
        *const c_char,
        u16,
        u16,
        u16,
        *const c_void,
        u32,
        *mut c_void,
    );

    /// `kDNSServiceType_A`.
    const TYPE_A: u16 = 1;
    /// `kDNSServiceClass_IN`.
    const CLASS_IN: u16 = 1;
    /// `kDNSServiceErr_PolicyDenied`.
    const POLICY_DENIED: DnsServiceErrorType = -65570;
    /// `POLLIN`.
    const POLLIN: i16 = 0x1;

    /// `struct pollfd`.
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }

    unsafe extern "C" {
        fn DNSServiceQueryRecord(
            sd_ref: *mut DnsServiceRef,
            flags: u32,
            interface_index: u32,
            fullname: *const c_char,
            rrtype: u16,
            rrclass: u16,
            callback: QueryRecordReply,
            context: *mut c_void,
        ) -> DnsServiceErrorType;
        fn DNSServiceRefSockFD(sd_ref: DnsServiceRef) -> i32;
        fn DNSServiceProcessResult(sd_ref: DnsServiceRef) -> DnsServiceErrorType;
        fn DNSServiceRefDeallocate(sd_ref: DnsServiceRef);
        fn poll(fds: *mut PollFd, nfds: u32, timeout: i32) -> i32;
    }

    /// A running query, deallocated exactly once on drop.
    struct Query(DnsServiceRef);

    impl Drop for Query {
        fn drop(&mut self) {
            // SAFETY: `Query` holds a ref `DNSServiceQueryRecord` returned with
            // no error, and this is the only deallocation of it.
            unsafe { DNSServiceRefDeallocate(self.0) };
        }
    }

    /// Records the first reply's error code in the `Cell<Option<i32>>` `context` names.
    unsafe extern "C" fn first_word(
        _sd_ref: DnsServiceRef,
        _flags: u32,
        _interface_index: u32,
        error: DnsServiceErrorType,
        _fullname: *const c_char,
        _rrtype: u16,
        _rrclass: u16,
        _rdlen: u16,
        _rdata: *const c_void,
        _ttl: u32,
        context: *mut c_void,
    ) {
        // SAFETY: `context` is the `Cell` `refused` passed, alive until its
        // query is deallocated, and this callback runs only inside
        // `DNSServiceProcessResult` on `refused`'s own thread.
        let word = unsafe { context.cast::<Cell<Option<DnsServiceErrorType>>>().as_ref() };
        if let Some(word) = word
            && word.get().is_none()
        {
            word.set(Some(error));
        }
    }

    /// [`super::denied`]: one A query for `host`, its replies read until the
    /// first, `within`, or a failure to read them.
    pub(super) fn refused(host: &str, within: Duration) -> bool {
        let Ok(name) = CString::new(host) else {
            return false;
        };
        let word: Cell<Option<DnsServiceErrorType>> = Cell::new(None);
        let mut raw: DnsServiceRef = std::ptr::null_mut();
        // SAFETY: `raw` and `name` outlive the call, and `word` outlives the
        // query (`Query` deallocates it before `word` goes out of scope), so
        // the callback's `context` is valid whenever it runs.
        let started = unsafe {
            DNSServiceQueryRecord(
                &raw mut raw,
                0,
                0,
                name.as_ptr(),
                TYPE_A,
                CLASS_IN,
                first_word,
                (&raw const word).cast_mut().cast(),
            )
        };
        if started != 0 {
            return started == POLICY_DENIED;
        }
        let query = Query(raw);
        // SAFETY: `query.0` is a live ref.
        let fd = unsafe { DNSServiceRefSockFD(query.0) };
        if fd < 0 {
            return false;
        }
        let now = Instant::now();
        let until = now.checked_add(within).unwrap_or(now);
        while word.get().is_none() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let mut ready = PollFd {
                fd,
                events: POLLIN,
                revents: 0,
            };
            let millis = i32::try_from(left.as_millis().max(1)).unwrap_or(i32::MAX);
            // SAFETY: `ready` is one valid `pollfd` for the call's duration.
            let polled = unsafe { poll(&raw mut ready, 1, millis) };
            if polled < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            // SAFETY: `query.0` is live and `poll` says a reply is waiting;
            // the callback writes only `word`.
            if polled > 0 && unsafe { DNSServiceProcessResult(query.0) } != 0 {
                break;
            }
        }
        drop(query);
        word.get() == Some(POLICY_DENIED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_name_under_local_is_a_local_name() {
        for local in [
            "alab-secrets.local",
            "alab-secrets.local.",
            "M22.LOCAL",
            "a.b.Local.",
            "é.local",
        ] {
            assert!(is_local_name(local), "{local}");
        }
        // The last splits six bytes from its end inside `é`.
        for not in [
            "local",
            ".local",
            "local.",
            "alab-secrets.locals",
            "alab-secrets.local.example",
            "example.com",
            "192.168.1.242",
            "",
            "é12345",
        ] {
            assert!(!is_local_name(not), "{not}");
        }
    }

    /// A wait for something that must happen is a hang detector, never a latency budget.
    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    /// The helper's answer is the answer when it comes in time; one that does
    /// not come in time, or never, is no, and the caller is not held for it.
    #[test]
    fn a_question_answered_late_or_never_is_no_refusal() {
        assert!(asked_within("a.local", HANG_DETECTOR, |_, _| true));
        assert!(!asked_within("a.local", HANG_DETECTOR, |_, _| false));
        let (never_tx, never_rx) = std::sync::mpsc::channel::<()>();
        assert!(!asked_within(
            "a.local",
            Duration::from_millis(100),
            move |_, _| {
                let _ = never_rx.recv_timeout(HANG_DETECTOR);
                true
            }
        ));
        drop(never_tx);
    }

    /// No refusal, over the real dns_sd, for each other first word: silence (a
    /// name nobody has), an answer (`localhost`, which mDNSResponder answers
    /// itself), a query dns_sd will not make (a label over 63 bytes is
    /// `kDNSServiceErr_BadParam` at once) and a name no C string carries.
    /// mDNSResponder never refuses a test binary's query: it judges it as the
    /// binary's own, measured on macOS 27.0 even in a denied app's tab, where
    /// the binary's connect is still refused; a refusal was measured from a
    /// denied app's own process (module docs).
    #[cfg(target_os = "macos")]
    #[test]
    fn silence_an_answer_and_a_query_not_made_are_no_refusal() {
        let wait = Duration::from_millis(500);
        let nobody = format!("aterm-http-nobody-{}.local", std::process::id());
        assert!(!denied(&nobody, wait));
        assert!(!denied("localhost", wait));
        assert!(!denied(&format!("{}.local", "a".repeat(64)), wait));
        assert!(!denied("bad\0name.local", wait));
    }
}
