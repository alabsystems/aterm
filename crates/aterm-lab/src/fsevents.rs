// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File-level, unprivileged filesystem census around one command. Counts delivered records,
//! not syscalls: FSEvents coalesces writes and reports drops separately. The full path table
//! stays in the caller's private output file; stdout contains only totals.

#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::{CStr, c_char, c_void};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::ptr;
use std::time::{Duration, Instant};

type Ref = *const c_void;

const USAGE: &str = "fsevents OUT -- PROGRAM ARGS\nfsevents summarize OUT PREFIX DEPTH";

#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(Ref) -> Ref>,
    release: Option<unsafe extern "C" fn(Ref)>,
    description: Option<unsafe extern "C" fn(Ref) -> Ref>,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: Ref;
    static kCFTypeArrayCallBacks: u8;
    fn CFStringCreateWithCString(allocator: Ref, string: *const c_char, encoding: u32) -> Ref;
    fn CFArrayCreate(allocator: Ref, values: *const Ref, count: isize, callbacks: Ref) -> Ref;
    fn CFRelease(value: Ref);
    fn CFRunLoopGetCurrent() -> Ref;
    fn CFRunLoopRunInMode(mode: Ref, seconds: f64, return_after_source: u8) -> i32;
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(
        allocator: Ref,
        callback: unsafe extern "C" fn(
            Ref,
            *mut c_void,
            usize,
            *mut c_void,
            *const u32,
            *const u64,
        ),
        context: *mut Context,
        paths: Ref,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> Ref;
    fn FSEventStreamScheduleWithRunLoop(stream: Ref, run_loop: Ref, mode: Ref);
    fn FSEventStreamStart(stream: Ref) -> u8;
    fn FSEventStreamStop(stream: Ref);
    fn FSEventStreamInvalidate(stream: Ref);
    fn FSEventStreamRelease(stream: Ref);
}

#[derive(Default)]
struct Counts {
    paths: BTreeMap<String, u64>,
    delivered: u64,
    must_scan: u64,
    user_dropped: u64,
    kernel_dropped: u64,
    first_id: Option<u64>,
    last_id: u64,
}

unsafe extern "C" fn receive(
    _: Ref,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    // SAFETY: the stream calls this only on the scheduling thread, with `info` pointing to
    // the live Counts, and arrays of `count` entries. Without UseCFTypes, paths is char**.
    unsafe {
        let counts = &mut *info.cast::<Counts>();
        let paths = paths.cast::<*const c_char>();
        for i in 0..count {
            let path = CStr::from_ptr(*paths.add(i)).to_string_lossy().into_owned();
            *counts.paths.entry(path).or_default() += 1;
            counts.delivered += 1;
            let flag = *flags.add(i);
            counts.must_scan += u64::from(flag & 1 != 0);
            counts.user_dropped += u64::from(flag & 2 != 0);
            counts.kernel_dropped += u64::from(flag & 4 != 0);
            let id = *ids.add(i);
            counts.first_id.get_or_insert(id);
            counts.last_id = id;
        }
    }
}

struct Stream<'a>(Ref, std::marker::PhantomData<&'a mut Counts>);

impl Drop for Stream<'_> {
    fn drop(&mut self) {
        // SAFETY: this owns the successfully created and scheduled stream. Invalidation
        // removes its callback before the borrowed Counts goes away.
        unsafe {
            FSEventStreamStop(self.0);
            FSEventStreamInvalidate(self.0);
            FSEventStreamRelease(self.0);
        }
    }
}

fn start(counts: &mut Counts) -> Result<Stream<'_>, String> {
    let mut context = Context {
        version: 0,
        info: ptr::from_mut(counts).cast(),
        retain: None,
        release: None,
        description: None,
    };
    // SAFETY: Create reads the null-terminated root string. The CF type callbacks retain
    // the string in the array, which the stream retains. The context's info outlives the stream.
    unsafe {
        let root = CFStringCreateWithCString(ptr::null(), c"/".as_ptr(), 0x0800_0100);
        if root.is_null() {
            return Err("create FSEvents root string".into());
        }
        let paths = CFArrayCreate(
            ptr::null(),
            &raw const root,
            1,
            (&raw const kCFTypeArrayCallBacks).cast(),
        );
        if paths.is_null() {
            CFRelease(root);
            return Err("create FSEvents root array".into());
        }
        // FileEvents | NoDefer; SinceNow avoids replaying unrelated history.
        let stream = FSEventStreamCreate(
            ptr::null(),
            receive,
            &raw mut context,
            paths,
            u64::MAX,
            0.2,
            0x10 | 0x02,
        );
        CFRelease(paths);
        CFRelease(root);
        if stream.is_null() {
            return Err("create FSEventStream on /".into());
        }
        let stream = Stream(stream, std::marker::PhantomData);
        FSEventStreamScheduleWithRunLoop(stream.0, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
        if FSEventStreamStart(stream.0) == 0 {
            return Err("start FSEventStream on /".into());
        }
        Ok(stream)
    }
}

pub(crate) fn cmd(args: &[String]) -> crate::Res {
    if matches!(args, [arg] if arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return Ok(0);
    }
    if let [verb, input, prefix, depth] = args
        && verb == "summarize"
    {
        return summarize(input, prefix, depth);
    }
    let [out, separator, program, rest @ ..] = args else {
        return Err(crate::Fail::usage(USAGE));
    };
    if separator != "--" {
        return Err(crate::Fail::usage(USAGE));
    }
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(out)
        .map_err(|e| crate::Fail::die(format!("create {out}: {e}")))?;
    let mut counts = Counts::default();
    let stream = start(&mut counts).map_err(crate::Fail::die)?;
    let began = Instant::now();
    let mut command = aterm_uds::spawnfd::command(program);
    command.args(rest);
    for (name, _) in std::env::vars_os() {
        if name
            .to_str()
            .is_some_and(aterm_toolchain::build_cache::said_by_cargo_run)
        {
            command.env_remove(name);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|e| crate::Fail::die(format!("start {program}: {e}")))?;
    let status = loop {
        // SAFETY: the default run-loop mode is a valid CFString for the process lifetime.
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.2, 0);
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|e| crate::Fail::die(e.to_string()))?
        {
            break status;
        }
    };
    // Keep the same latency and two-second delivery tail in every run.
    let tail = Instant::now();
    while tail.elapsed() < Duration::from_secs(2) {
        // SAFETY: same process-lifetime default mode as above.
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.2, 0);
        }
    }
    drop(stream);
    let summary = format!(
        "seconds={:.3} status={status} delivered={} must_scan={} user_dropped={} kernel_dropped={} first_id={} last_id={}",
        began.elapsed().as_secs_f64(),
        counts.delivered,
        counts.must_scan,
        counts.user_dropped,
        counts.kernel_dropped,
        counts.first_id.unwrap_or(0),
        counts.last_id,
    );
    writeln!(output, "# {summary}").map_err(|e| crate::Fail::die(e.to_string()))?;
    for (path, count) in counts.paths {
        writeln!(output, "{count}\t{}", path.escape_default())
            .map_err(|e| crate::Fail::die(e.to_string()))?;
    }
    println!("fsevents: {summary}; paths in {out}");
    Ok(status.code().unwrap_or(1))
}

fn summarize(input: &str, prefix: &str, depth: &str) -> crate::Res {
    let depth: usize = depth
        .parse()
        .map_err(|_| crate::Fail::usage("summarize OUT PREFIX DEPTH"))?;
    let text = std::fs::read_to_string(input).map_err(|e| crate::Fail::die(e.to_string()))?;
    let declared: u64 = text
        .lines()
        .next()
        .filter(|line| line.starts_with("# seconds="))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|word| word.strip_prefix("delivered="))
        })
        .and_then(|count| count.parse().ok())
        .ok_or_else(|| crate::Fail::die("census has no final record; capture is incomplete"))?;
    let mut totals = BTreeMap::<std::path::PathBuf, u64>::new();
    let prefix = std::path::Path::new(prefix);
    let mut total = 0;
    let mut delivered = 0u64;
    for line in text.lines() {
        if line.starts_with('#') {
            println!("{line}");
            continue;
        }
        let (count, path) = line
            .split_once('\t')
            .ok_or_else(|| crate::Fail::die("invalid census row"))?;
        let count: u64 = count
            .parse()
            .map_err(|_| crate::Fail::die("invalid census count"))?;
        delivered = delivered
            .checked_add(count)
            .ok_or_else(|| crate::Fail::die("census count overflow"))?;
        let path = std::path::Path::new(path);
        if let Ok(relative) = path.strip_prefix(prefix) {
            let relative: std::path::PathBuf = relative.components().take(depth).collect();
            *totals.entry(prefix.join(relative)).or_default() += count;
            total += count;
        }
    }
    if delivered != declared {
        return Err(crate::Fail::die(
            "census path table does not match its final count",
        ));
    }
    let mut totals: Vec<_> = totals.into_iter().collect();
    totals.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    println!("total={total} prefix={}", prefix.display());
    for (path, count) in totals.into_iter().take(100) {
        println!("{count}\t{}", path.display());
    }
    Ok(0)
}
