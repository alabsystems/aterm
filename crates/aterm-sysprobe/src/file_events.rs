// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unprivileged, bounded delivered-file-event census. Paths identify destinations, never
//! the writing process. Dropped markers and capacity overflow are reported, not hidden.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// One delivered-event sample. A path counts once per delivered event, which macOS
/// may coalesce; this is not a syscall count or a complete record of every writer.
#[derive(Debug, Default)]
pub struct Census {
    /// Delivered events per parent directory, or per file in a scoped file capture.
    /// At most 16,384 distinct paths are retained.
    pub counts: BTreeMap<String, u64>,
    /// MustScanSubDirs, UserDropped or KernelDropped markers: attribution is incomplete.
    pub dropped: u64,
    /// Events whose new path exceeded the bounded table.
    pub overflow: u64,
    #[cfg(any(target_os = "macos", test))]
    roots: Vec<std::path::PathBuf>,
    #[cfg(any(target_os = "macos", test))]
    exact: bool,
}

impl Census {
    fn configured(roots: &[std::path::PathBuf], exact: bool) -> Result<Self, String> {
        let bytes = roots.iter().fold(0usize, |n, root| {
            n.saturating_add(root.as_os_str().as_encoded_bytes().len())
        });
        if (exact && roots.is_empty())
            || roots.len() > 256
            || bytes > 65_536
            || roots
                .iter()
                .any(|root| !root.is_absolute() || root.to_str().is_none_or(|s| s.contains('\0')))
        {
            return Err("file capture needs 1–256 absolute roots, at most 64 KiB total".into());
        }
        Ok(Self {
            #[cfg(any(target_os = "macos", test))]
            roots: roots.to_vec(),
            #[cfg(any(target_os = "macos", test))]
            exact,
            ..Self::default()
        })
    }

    #[cfg(any(target_os = "macos", test))]
    fn add(&mut self, path: &str, flags: u32) {
        if flags & 7 != 0 {
            self.dropped += 1;
        }
        let path = Path::new(path);
        if !self.roots.is_empty() && !self.roots.iter().any(|root| path.starts_with(root)) {
            return;
        }
        let bucket = if self.exact {
            path
        } else {
            path.parent().unwrap_or(Path::new("/"))
        };
        let bucket = bucket.to_string_lossy();
        if let Some(count) = self.counts.get_mut(bucket.as_ref()) {
            *count += 1;
        } else if self.counts.len() < 16_384 {
            self.counts.insert(bucket.into_owned(), 1);
        } else {
            self.overflow += 1;
        }
    }

    /// Rank paths under `roots`, descending by delivered count. Empty roots selects
    /// the whole stream. Component-aware matching excludes similarly named neighbours.
    #[must_use]
    pub fn ranked(&self, roots: &[std::path::PathBuf]) -> Vec<(&str, u64)> {
        let mut rows: Vec<_> = self
            .counts
            .iter()
            .filter(|(path, _)| {
                roots.is_empty() || roots.iter().any(|root| Path::new(path).starts_with(root))
            })
            .map(|(path, count)| (path.as_str(), *count))
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        rows
    }
}

/// Subscribe on `/` with file-level events and NoDefer for `duration`. This runs the current
/// thread's CF run loop: call it on a diagnostic thread, never a window's main thread.
/// `ready` runs once after the stream starts, before sampling. A workload must wait for it;
/// process launch alone does not establish that the subscription is ready.
///
/// # Errors
/// Returns an error off macOS, for a zero duration, or when CoreServices refuses the stream.
/// Roots must be absolute UTF-8 paths without NUL, at most 256 and 64 KiB total.
/// File aggregation requires at least one root.
pub fn capture(
    duration: Duration,
    granularity: Granularity<'_>,
    ready: impl FnOnce(),
) -> Result<Census, String> {
    if duration.is_zero() {
        return Err("duration must be positive".into());
    }
    let counts = match granularity {
        Granularity::Directories(roots) => Census::configured(roots, false)?,
        Granularity::Files(roots) => Census::configured(roots, true)?,
    };
    #[cfg(target_os = "macos")]
    return mac::run(duration.as_secs_f64(), counts, ready);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (counts, ready);
        Err("FSEvents is macOS-only".into())
    }
}

/// Directory aggregation for a broad survey, or individual paths beneath a bounded scope.
/// Focused filtering happens before allocation; dropped markers remain global in both modes.
pub enum Granularity<'a> {
    /// Retain parent directories across the whole stream.
    Directories(&'a [std::path::PathBuf]),
    /// Retain file paths only under these component-aware roots.
    Files(&'a [std::path::PathBuf]),
}

/// SDK constants and context layout used by the stream, checked against Apple's headers.
#[cfg(target_os = "macos")]
pub const SDK_ROWS: &[(&str, usize)] = &[
    (
        "kFSEventStreamCreateFlagFileEvents",
        mac::FILE_EVENTS as usize,
    ),
    ("kFSEventStreamCreateFlagNoDefer", mac::NO_DEFER as usize),
    (
        "sizeof(FSEventStreamContext)",
        std::mem::size_of::<mac::Context>(),
    ),
    (
        "offsetof(FSEventStreamContext, info)",
        std::mem::offset_of!(mac::Context, info),
    ),
    (
        "offsetof(FSEventStreamContext, copyDescription)",
        std::mem::offset_of!(mac::Context, description),
    ),
];

#[cfg(target_os = "macos")]
mod mac {
    use super::Census;
    use crate::iokit::{CFAllocatorRef, CFArrayRef, CFStringCreateWithCString};
    use aterm_objc::cf::{CFRelease, CFRunLoopRef, CFRunLoopRunInMode, kCFRunLoopDefaultMode};
    use std::ffi::{CStr, c_char, c_void};
    use std::ptr;

    #[repr(C)]
    pub(super) struct Context {
        version: isize,
        pub(super) info: *mut c_void,
        retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
        release: Option<unsafe extern "C" fn(*const c_void)>,
        pub(super) description: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    }

    pub(super) const FILE_EVENTS: u32 = 0x10;
    pub(super) const NO_DEFER: u32 = 0x02;

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn FSEventStreamCreate(
            allocator: *const c_void,
            callback: unsafe extern "C" fn(
                *const c_void,
                *mut c_void,
                usize,
                *mut c_void,
                *const u32,
                *const u64,
            ),
            context: *mut Context,
            paths: *const c_void,
            since: u64,
            latency: f64,
            flags: u32,
        ) -> *const c_void;
        fn FSEventStreamScheduleWithRunLoop(
            stream: *const c_void,
            run_loop: *const c_void,
            mode: *const c_void,
        );
        fn FSEventStreamStart(stream: *const c_void) -> u8;
        fn FSEventStreamStop(stream: *const c_void);
        fn FSEventStreamInvalidate(stream: *const c_void);
        fn FSEventStreamRelease(stream: *const c_void);
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayCreate(
            allocator: CFAllocatorRef,
            values: *const *const c_void,
            count: isize,
            callbacks: *const c_void,
        ) -> CFArrayRef;
        fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    }

    struct CfObject(*const c_void);

    impl Drop for CfObject {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: each non-null value came from a Create call and owns one retain.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    struct Stream {
        raw: *const c_void,
        started: bool,
    }

    impl Drop for Stream {
        fn drop(&mut self) {
            if !self.raw.is_null() {
                // SAFETY: this guard owns the created stream, on its scheduling thread.
                // Invalidation also runs if another run-loop source unwinds, before the
                // callback context and its borrowed Census leave the stack.
                unsafe {
                    if self.started {
                        FSEventStreamStop(self.raw);
                    }
                    FSEventStreamInvalidate(self.raw);
                    FSEventStreamRelease(self.raw);
                }
            }
        }
    }

    unsafe extern "C" fn receive(
        _: *const c_void,
        info: *mut c_void,
        n: usize,
        paths: *mut c_void,
        flags: *const u32,
        _: *const u64,
    ) {
        // SAFETY: the stream calls this synchronously on our run loop, with n entries and
        // the Census borrowed in run() until after invalidation. UseCFTypes is not enabled.
        unsafe {
            let counts = &mut *info.cast::<Census>();
            for i in 0..n {
                let path = CStr::from_ptr(*paths.cast::<*const c_char>().add(i)).to_string_lossy();
                counts.add(&path, *flags.add(i));
            }
        }
    }

    pub fn run(seconds: f64, mut counts: Census, ready: impl FnOnce()) -> Result<Census, String> {
        let mut context = Context {
            version: 0,
            info: (&raw mut counts).cast(),
            retain: None,
            release: None,
            description: None,
        };
        // SAFETY: valid NUL-terminated root; array borrows the string until stream teardown.
        // The context and all CoreFoundation objects outlive the synchronous run loop.
        unsafe {
            let watched = [std::path::PathBuf::from("/")];
            let mut strings = Vec::with_capacity(watched.len());
            for path in watched {
                use std::os::unix::ffi::OsStrExt as _;
                let text = std::ffi::CString::new(path.as_os_str().as_bytes())
                    .map_err(|_| "event root contains NUL")?;
                let value = CfObject(
                    CFStringCreateWithCString(ptr::null(), text.as_ptr(), 0x0800_0100).cast(),
                );
                if value.0.is_null() {
                    return Err("CFStringCreateWithCString failed".into());
                }
                strings.push(value);
            }
            let values: Vec<_> = strings.iter().map(|value| value.0).collect();
            let paths = CfObject(
                CFArrayCreate(
                    ptr::null(),
                    values.as_ptr(),
                    values.len() as isize,
                    ptr::null(),
                )
                .cast(),
            );
            if paths.0.is_null() {
                return Err("CFArrayCreate failed".into());
            }
            let mut stream = Stream {
                raw: FSEventStreamCreate(
                    ptr::null(),
                    receive,
                    &raw mut context,
                    paths.0,
                    u64::MAX,
                    0.2,
                    FILE_EVENTS | NO_DEFER,
                ),
                started: false,
            };
            if stream.raw.is_null() {
                return Err("FSEventStreamCreate failed".into());
            }
            FSEventStreamScheduleWithRunLoop(
                stream.raw,
                CFRunLoopGetCurrent().cast(),
                kCFRunLoopDefaultMode.cast(),
            );
            stream.started = FSEventStreamStart(stream.raw) != 0;
            if !stream.started {
                return Err("FSEventStreamStart refused".into());
            }
            ready();
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 0);
            // End callbacks before moving their Census into the return value.
            drop(stream);
        }
        Ok(counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn census_ranks_destinations_and_reports_incomplete_delivery() {
        let mut census = Census::default();
        census.add("/owned/logs/a", 0);
        census.add("/owned/logs/b", 0);
        census.add("/owned/state/c", 1);
        // Negative control: a lexical-prefix match must not attribute a neighbour.
        census.add("/owned-other/logs/d", 2);
        assert_eq!(
            census.ranked(&["/owned".into()]),
            vec![("/owned/logs", 2), ("/owned/state", 1)]
        );
        assert_eq!(census.dropped, 2);
        for i in 0..16_384 {
            census.add(&format!("/many/{i}/file"), 0);
        }
        assert_eq!(census.counts.len(), 16_384);
        assert_eq!(census.overflow, 3);
        census.add("/owned/logs/a", 0);
        assert_eq!(census.ranked(&["/owned".into()])[0].1, 3);
    }

    #[test]
    fn focused_files_distinguish_journals_and_keys_without_hiding_global_drops() {
        let mut files = Census::configured(&["/owned/state".into()], true).unwrap();
        for path in ["/owned/state/control.astream", "/owned/state/device.age"] {
            files.add(path, 0);
        }
        files.add("/owned/state-neighbour/other", 2);
        assert_eq!(
            files.ranked(&[]),
            vec![
                ("/owned/state/control.astream", 1),
                ("/owned/state/device.age", 1)
            ]
        );
        assert_eq!(files.dropped, 1);
        // Negative control: the broad directory instrument deliberately coalesces these.
        let mut directories = Census::default();
        directories.add("/owned/state/control.astream", 0);
        directories.add("/owned/state/device.age", 0);
        assert_eq!(directories.ranked(&[]), vec![("/owned/state", 2)]);
        assert!(Census::configured(&[], true).is_err());
        assert!(Census::configured(&["relative".into()], true).is_err());
        assert!(Census::configured(&vec!["/root".into(); 257], true).is_err());
        assert!(Census::configured(&[format!("/{}", "x".repeat(65_536)).into()], true).is_err());
    }

    #[test]
    fn invalid_capture_never_announces_a_ready_subscription() {
        assert!(
            capture(Duration::ZERO, Granularity::Directories(&[]), || panic!(
                "not started"
            ))
            .is_err()
        );
        assert!(
            capture(Duration::from_secs(1), Granularity::Files(&[]), || panic!(
                "not started"
            ))
            .is_err()
        );
    }
}
