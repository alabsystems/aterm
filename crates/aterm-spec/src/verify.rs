// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The shared verification helpers for aterm's derived models and their conformance tests, in two tiers with one switch between them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use crate::derive::{Liveness, Model};
use crate::interp;

/// Discover the Trust `ty` model checker: the atpkg store's shim, then `ty` on `PATH` ([`locate_ty`] says which answered).
#[must_use]
pub fn find_ty() -> Option<PathBuf> {
    checker("ty", locate_ty)
}

/// Discover `ty` and where it came from, whatever [`checkers_required`] says: what `spec-check` asks before it turns the checkers on.
#[must_use]
pub fn locate_ty() -> Option<Located> {
    discover("ty")
}

/// [`locate_ty`] for `ay`.
#[must_use]
pub fn locate_ay() -> Option<Located> {
    discover("ay")
}

static CHECKERS_REQUIRED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Turn the external checkers on for this process: from here every `ty` and `ay` obligation runs, and a missing checker fails it.
pub fn require_checkers() {
    let _ = CHECKERS_REQUIRED.set(true);
    assert!(
        checkers_required(),
        "require_checkers() after this process already ran without the external checkers"
    );
}

/// Whether this process runs the external checkers.
#[must_use]
pub fn checkers_required() -> bool {
    *CHECKERS_REQUIRED.get_or_init(|| cfg!(feature = "checkers"))
}

#[cfg_attr(
    trust_verify,
    trust::contract_panic(message_contains = "VERIFICATION GATE")
)]
#[cfg_attr(trust_verify, trust::skip)]
fn checker(bin: &str, locate: fn() -> Option<Located>) -> Option<PathBuf> {
    if !checkers_required() {
        return None;
    }
    Some(locate().map(|l| l.path).unwrap_or_else(|| {
        let (state, tail) = missing_tool_tail(bin);
        panic!(
            "VERIFICATION GATE: the external checkers were asked for, and Trust `{bin}` {state}.{tail}"
        )
    }))
}

/// The discovery tier that answered, in precedence order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrustBinOrigin {
    Store,
    Path,
}

impl TrustBinOrigin {
    /// The tier as a developer reads it in the provenance line.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Store => "atpkg managed store",
            Self::Path => "PATH",
        }
    }
}

/// A discovered Trust binary and the tier that answered.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Located {
    /// What the caller will execute.
    pub path: PathBuf,
    /// Which discovery tier produced it.
    pub origin: TrustBinOrigin,
}

#[cfg_attr(trust_verify, trust::skip)]
fn discover(bin: &'static str) -> Option<Located> {
    discover_identified(bin).map(|(located, _)| located)
}

#[cfg_attr(trust_verify, trust::skip)]
fn discover_identified(bin: &'static str) -> Option<(Located, Option<BinaryIdentity>)> {
    let homes = home_dirs();
    let store_bin = atpkg_store_bin_dir();
    let path_var = std::env::var_os("PATH");
    let found = find_trust_bin(bin, store_bin.as_deref(), path_var.as_deref()).map(|located| {
        let identity = binary_identity(&located.path);
        (located, identity)
    });
    report_provenance_on_change(bin, found.as_ref(), &homes, store_bin.as_deref());
    found
}

#[cfg_attr(trust_verify, trust::skip)]
fn find_trust_bin(
    bin: &str,
    store_bin_dir: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<Located> {
    let exe = exe_name(bin);
    if let Some(p) = store_bin_dir.and_then(|d| resolve_store_shim(&d.join(&exe))) {
        return Some(Located {
            path: p,
            origin: TrustBinOrigin::Store,
        });
    }
    path_search_in(&exe, path_var).map(|path| Located {
        path,
        origin: TrustBinOrigin::Path,
    })
}

#[cfg_attr(trust_verify, trust::skip)]
fn atpkg_store_bin_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        let local = std::env::var("LOCALAPPDATA")
            .ok()
            .filter(|d| !d.is_empty())?;
        Some(PathBuf::from(local).join("aterm").join("pkg").join("bin"))
    } else {
        Some(unix_store_bin_dir(&home_dirs().into_iter().next()?))
    }
}

fn unix_store_bin_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library")
            .join("Application Support")
            .join("aterm")
            .join("pkg")
            .join("bin")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".local")
            .join("share")
            .join("aterm")
            .join("pkg")
            .join("bin")
    }
}

/// The file a store shim forwards to, when it forwards to a real one.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn resolve_store_shim(shim: &Path) -> Option<PathBuf> {
    let meta = std::fs::symlink_metadata(shim).ok()?;
    let target = if meta.file_type().is_symlink() {
        shim.to_path_buf()
    } else if meta.file_type().is_file() && meta.len() <= MAX_STORE_SHIM_BYTES {
        let content = std::fs::read_to_string(shim).ok()?;
        let target = parse_exec_stub_target(&content)?;
        if !target.is_absolute() {
            return None;
        }
        target
    } else {
        return None;
    };
    let resolved = std::fs::canonicalize(target).ok()?;
    resolved.is_file().then_some(resolved)
}

const MAX_STORE_SHIM_BYTES: u64 = 64 * 1024;

fn parse_exec_stub_target(content: &str) -> Option<PathBuf> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("exec '")
            && let Some(end) = rest.rfind("' \"$@\"")
        {
            return Some(PathBuf::from(rest[..end].replace("'\\''", "'")));
        }
    }
    None
}

fn exe_name(bin: &str) -> String {
    format!("{bin}{}", std::env::consts::EXE_SUFFIX)
}

fn home_dirs() -> Vec<PathBuf> {
    #[cfg(unix)]
    if let Some(home) = account_home() {
        return vec![home];
    }
    let mut homes = Vec::new();
    for var in ["HOME", "USERPROFILE"] {
        if let Ok(dir) = std::env::var(var)
            && !dir.is_empty()
        {
            homes.push(PathBuf::from(normalize_home(dir)));
        }
    }
    homes
}

#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn account_home() -> Option<PathBuf> {
    if let Some(home) = ACCOUNT_HOME.get() {
        return Some(home.clone());
    }
    let home = lookup_account_home().filter(|home| home.is_absolute())?;
    Some(ACCOUNT_HOME.get_or_init(|| home).clone())
}

#[cfg(unix)]
static ACCOUNT_HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn lookup_account_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: `passwd` is plain integers and pointers, for which all-zero is a valid value; it is only an out-param here.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = [0u8; 4096];
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: `pw`, `buf` (of the stated length) and `out` are valid writable out-params, and getpwuid_r writes nothing past buflen.
    unsafe {
        let rc = libc::getpwuid_r(
            libc::getuid(),
            &raw mut pw,
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len(),
            &raw mut out,
        );
        if rc != 0 || out.is_null() || pw.pw_dir.is_null() {
            return None;
        }
        let bytes = std::ffi::CStr::from_ptr(pw.pw_dir).to_bytes();
        (!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
}

fn normalize_home(dir: String) -> String {
    if cfg!(windows) {
        let b = dir.as_bytes();
        if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/' {
            return format!("{}:{}", b[1] as char, &dir[2..]);
        }
    }
    dir
}

#[cfg_attr(trust_verify, trust::skip)]
fn path_search_in(exe: &str, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path_var?)
        .map(|dir| dir.join(exe))
        .find(|cand| cand.is_file() && !is_pending_stub(cand))
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct StoreCopy {
    program: String,
    build: String,
    path: PathBuf,
    built: Option<String>,
}

impl StoreCopy {
    fn describe(&self) -> String {
        let exe = self
            .path
            .file_name()
            .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
        format!(
            "store/{}/{}/bin/{exe} (built {})",
            self.program,
            self.build,
            self.built.as_deref().unwrap_or("date unknown")
        )
    }
}

#[cfg_attr(trust_verify, trust::skip)]
fn store_copies_in(store_root: &Path, exe: &str) -> Vec<StoreCopy> {
    let Ok(entries) = std::fs::read_dir(store_root) else {
        return Vec::new();
    };
    let mut copies: Vec<StoreCopy> = entries
        .flatten()
        .filter_map(|e| {
            let build_dir = std::fs::canonicalize(e.path().join("current")).ok()?;
            let path = build_dir.join("bin").join(exe);
            if !path.is_file() || is_pending_stub(&path) {
                return None;
            }
            Some(StoreCopy {
                program: e.file_name().to_string_lossy().into_owned(),
                build: build_dir.file_name()?.to_string_lossy().into_owned(),
                built: build_date(&path),
                path,
            })
        })
        .collect();
    copies.sort_by(|a, b| a.program.cmp(&b.program));
    copies
}

#[cfg_attr(trust_verify, trust::skip)]
fn exec_stub_target(p: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(p).ok()?;
    if !meta.is_file() || meta.len() > MAX_STORE_SHIM_BYTES {
        return None;
    }
    parse_exec_stub_target(&std::fs::read_to_string(p).ok()?)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct FileIdentity {
    len: u64,
    modified: Option<std::time::SystemTime>,
    dev: u64,
    ino: u64,
    ctime: (i64, i64),
}

impl FileIdentity {
    #[cfg_attr(trust_verify, trust::skip)]
    fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let (dev, ino, ctime) = {
            use std::os::unix::fs::MetadataExt as _;
            (meta.dev(), meta.ino(), (meta.ctime(), meta.ctime_nsec()))
        };
        #[cfg(not(unix))]
        let (dev, ino, ctime) = (0, 0, (0, 0));
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            dev,
            ino,
            ctime,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum ExecForm {
    AtpkgShim,
    LiteralExec,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct ChainLink {
    path: PathBuf,
    file: FileIdentity,
    execs: Option<ExecForm>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum ChainEnd {
    Binary,
    OpaqueScript,
    Dangling(PathBuf),
    TooDeep,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct BinaryIdentity {
    links: Vec<ChainLink>,
    end: ChainEnd,
}

impl BinaryIdentity {
    fn last(&self) -> Option<&ChainLink> {
        self.links.last()
    }
}

const MAX_EXEC_HOPS: usize = 8;

#[cfg_attr(trust_verify, trust::skip)]
fn binary_identity(path: &Path) -> Option<BinaryIdentity> {
    let mut links: Vec<ChainLink> = Vec::new();
    let mut at = path.to_path_buf();
    loop {
        let meta = match std::fs::metadata(&at) {
            Ok(meta) => meta,
            Err(_) if links.is_empty() => return None,
            Err(_) => {
                return Some(BinaryIdentity {
                    links,
                    end: ChainEnd::Dangling(at),
                });
            }
        };
        let file = FileIdentity::of(&meta);
        if links.len() == MAX_EXEC_HOPS || links.iter().any(|l| l.file == file) {
            return Some(BinaryIdentity {
                links,
                end: ChainEnd::TooDeep,
            });
        }
        let hop = if meta.is_file() {
            script_exec_target(&at, meta.len())
        } else {
            Hop::Binary
        };
        let (execs, next) = match hop {
            Hop::Execs(target, form) => (Some(form), Ok(target)),
            Hop::Binary => (None, Err(ChainEnd::Binary)),
            Hop::Opaque => (None, Err(ChainEnd::OpaqueScript)),
        };
        links.push(ChainLink {
            path: at,
            file,
            execs,
        });
        match next {
            Ok(target) => at = target,
            Err(end) => return Some(BinaryIdentity { links, end }),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Hop {
    Binary,
    Opaque,
    Execs(PathBuf, ExecForm),
}

#[cfg_attr(trust_verify, trust::skip)]
fn script_exec_target(p: &Path, len: u64) -> Hop {
    use std::io::Read as _;
    let Ok(mut file) = std::fs::File::open(p) else {
        return Hop::Binary;
    };
    let mut magic = [0u8; 2];
    if file.read_exact(&mut magic).is_err() || magic != *b"#!" {
        return Hop::Binary;
    }
    let mut text = String::from("#!");
    if len > MAX_STORE_SHIM_BYTES
        || file
            .take(MAX_STORE_SHIM_BYTES)
            .read_to_string(&mut text)
            .is_err()
    {
        return Hop::Opaque;
    }
    let atpkg = text
        .lines()
        .nth(1)
        .is_some_and(|l| l.starts_with(ATPKG_SHIM_HEADER));
    let (target, form) = if atpkg {
        (parse_exec_stub_target(&text), ExecForm::AtpkgShim)
    } else {
        (literal_exec_target(&text), ExecForm::LiteralExec)
    };
    match target {
        Some(t) if t.is_absolute() => Hop::Execs(t, form),
        _ => Hop::Opaque,
    }
}

const ATPKG_SHIM_HEADER: &str = "# atpkg shim";

fn literal_exec_target(text: &str) -> Option<PathBuf> {
    let mut exec_line = None;
    for line in text.lines().skip(1).map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if exec_line.is_some() {
            return None;
        }
        if !is_shell_option_line(line) {
            exec_line = Some(line);
        }
    }
    let rest = exec_line?.strip_prefix("exec")?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let (target, after) = shell_literal_word(rest.trim_start())?;
    matches!(after.trim(), "" | "\"$@\"").then(|| PathBuf::from(target))
}

fn is_shell_option_line(line: &str) -> bool {
    let mut words = line.split_whitespace();
    words.next() == Some("set")
        && words.all(|w| {
            let name = w.strip_prefix(['-', '+']).unwrap_or(w);
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic())
        })
}

fn shell_literal_word(s: &str) -> Option<(&str, &str)> {
    let quoted = |q: char, forbidden: &str| {
        let (word, after) = s.strip_prefix(q)?.split_once(q)?;
        let glued = after.chars().next().is_some_and(|c| !c.is_whitespace());
        (!word.chars().any(|c| forbidden.contains(c)) && !glued).then_some((word, after))
    };
    match s.chars().next()? {
        '\'' => quoted('\'', ""),
        '"' => quoted('"', "$`\\"),
        _ => {
            let after = s.trim_start_matches(|c: char| !c.is_whitespace());
            let word = s.strip_suffix(after)?;
            let meta = "$`\\\"'*?[]{}~;&|<>()";
            (!word.chars().any(|c| meta.contains(c))).then_some((word, after))
        }
    }
}

fn utc_timestamp(t: std::time::SystemTime) -> Option<String> {
    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let (y, m, d) = civil_from_unix_days(i64::try_from(secs / 86_400).ok()?);
    let s = secs % 86_400;
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    ))
}

#[cfg_attr(trust_verify, trust::skip)]
fn build_date(p: &Path) -> Option<String> {
    let secs = std::fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let (y, m, d) = civil_from_unix_days(i64::try_from(secs / 86_400).ok()?);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

#[cfg_attr(trust_verify, trust::skip)]
fn civil_from_unix_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        y,
        u32::try_from(m).unwrap_or(0),
        u32::try_from(d).unwrap_or(0),
    )
}

const PROBE_HANG: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg_attr(trust_verify, trust::skip)]
fn probe_output(bin: &Path, args: &[&str]) -> Option<std::process::Output> {
    probe_output_within(bin, args, PROBE_HANG)
}

#[cfg_attr(trust_verify, trust::skip)]
fn probe_output_within(
    bin: &Path,
    args: &[&str],
    bound: std::time::Duration,
) -> Option<std::process::Output> {
    use std::process::Stdio;
    use std::time::Instant;
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + bound;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    child.wait_with_output().ok()
}

#[cfg_attr(trust_verify, trust::skip)]
fn probe_version(bin: &Path) -> Option<String> {
    let out = probe_output(bin, &["--version"])?;
    if !out.status.success() {
        return None;
    }
    let first = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.chars().take(160).collect::<String>())
    };
    first(&out.stdout).or_else(|| first(&out.stderr))
}

const CAP_TRACE_CASES: &str = "trace-cases-v1";
const CAP_BUDGET_SELF: &str = "check-budget-self-v1";

#[cfg_attr(trust_verify, trust::skip)]
fn ty_caps(ty: &Path) -> Vec<String> {
    static CAPS: CapsByBuild = Mutex::new(BTreeMap::new());
    caps_cached(&CAPS, ty, |ty| {
        probe_output(ty, &["capabilities", "--json"])
            .map(|out| caps_from(out.status.code(), &out.stdout))
            .unwrap_or_default()
    })
}

type CapsByBuild = Mutex<BTreeMap<(PathBuf, Option<BinaryIdentity>), Vec<String>>>;

#[cfg_attr(trust_verify, trust::skip)]
fn caps_cached(
    cache: &CapsByBuild,
    ty: &Path,
    probe: impl FnOnce(&Path) -> Vec<String>,
) -> Vec<String> {
    let build = (ty.to_path_buf(), binary_identity(ty));
    let mut caps = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(known) = caps.get(&build) {
        return known.clone();
    }
    let probed = probe(ty);
    caps.insert(build, probed.clone());
    probed
}

fn caps_from(code: Option<i32>, stdout: &[u8]) -> Vec<String> {
    if code != Some(0) {
        return Vec::new();
    }
    let Ok(doc) = aterm_json::from_slice::<aterm_json::Value>(stdout.trim_ascii()) else {
        return Vec::new();
    };
    doc.get("capabilities")
        .and_then(aterm_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn caps_clause(caps: &[String]) -> String {
    if caps.is_empty() {
        "; caps=none".to_string()
    } else {
        format!("; caps={}", caps.join(","))
    }
}

fn tilde(p: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum StoreShim {
    Absent,
    Refusal,
    Forwarding,
}

#[cfg_attr(trust_verify, trust::skip)]
fn classify_store_shim(shim: &Path) -> StoreShim {
    match std::fs::symlink_metadata(shim) {
        Err(_) => StoreShim::Absent,
        Ok(m) if m.file_type().is_symlink() => StoreShim::Forwarding,
        Ok(_) if exec_stub_target(shim).is_some() => StoreShim::Forwarding,
        Ok(_) => StoreShim::Refusal,
    }
}

fn render_provenance(
    bin: &str,
    found: Option<&Located>,
    identity: Option<&BinaryIdentity>,
    version: Option<&str>,
    shadowed: &[StoreCopy],
    shim: &StoreShim,
    home: Option<&Path>,
) -> String {
    let copies = shadowed
        .iter()
        .map(StoreCopy::describe)
        .collect::<Vec<_>>()
        .join(", ");
    let Some(found) = found else {
        let mut line = format!(
            "VERIFY ESCALATION TIER: {bin} = NOT FOUND (probed the atpkg store shim, PATH)"
        );
        if !shadowed.is_empty() {
            let why = match shim {
                StoreShim::Absent => {
                    format!("no `{bin}` shim in the store's bin/ exposes it")
                }
                StoreShim::Refusal => format!(
                    "the store's `{bin}` shim is a refusal script (tombstone or pending stub)"
                ),
                StoreShim::Forwarding => format!(
                    "the store's `{bin}` shim does not resolve to a file (a reclaimed build?)"
                ),
            };
            line.push_str(&format!(
                " — the managed store holds {copies} but discovery does not reach it: {why}"
            ));
        }
        return line;
    };
    let last = identity.and_then(BinaryIdentity::last);
    let built = last
        .and_then(|l| l.file.modified)
        .and_then(utc_timestamp)
        .unwrap_or_else(|| "date unknown".to_owned());
    let bytes = last.map_or_else(
        || "size unknown".to_owned(),
        |l| format!("{} bytes", l.file.len),
    );
    let mut line = format!(
        "VERIFY ESCALATION TIER: {bin} = {} [{}{}; {}; built {built}; {bytes}]",
        tilde(&found.path, home),
        found.origin.label(),
        identity.map_or_else(String::new, |id| describe_exec_chain(id, home)),
        version.unwrap_or("version unknown"),
    );
    if !shadowed.is_empty() {
        line.push_str(&format!(" — the managed store also holds {copies}"));
    }
    line
}

fn describe_exec_chain(id: &BinaryIdentity, home: Option<&Path>) -> String {
    let mut note = String::new();
    if let Some(form) = id.links.first().and_then(|l| l.execs) {
        note.push_str(match form {
            ExecForm::AtpkgShim => "atpkg shim",
            ExecForm::LiteralExec => "wrapper",
        });
        for link in id.links.iter().skip(1) {
            note.push_str(&format!(" → {}", tilde(&link.path, home)));
        }
    }
    match &id.end {
        ChainEnd::Binary => {}
        ChainEnd::OpaqueScript => {
            if !note.is_empty() {
                note.push_str("; ");
            }
            note.push_str(
                "script whose exec target does not read statically: built/bytes are the \
                 script's own",
            );
        }
        ChainEnd::Dangling(p) => note.push_str(&format!(" → {} (missing)", tilde(p, home))),
        ChainEnd::TooDeep => {
            note.push_str(&format!(" → … (over {MAX_EXEC_HOPS} hops, or a loop)"));
        }
    }
    if note.is_empty() {
        note
    } else {
        format!(" ({note})")
    }
}

#[cfg_attr(trust_verify, trust::skip)]
fn provenance_line(
    bin: &str,
    found: Option<&Located>,
    identity: Option<&BinaryIdentity>,
    version: Option<&str>,
    homes: &[PathBuf],
    store_bin: Option<&Path>,
) -> String {
    let exe = exe_name(bin);
    let copies = store_bin
        .and_then(Path::parent)
        .map(|prefix| store_copies_in(&prefix.join("store"), &exe))
        .unwrap_or_default();
    let chosen = identity
        .and_then(BinaryIdentity::last)
        .and_then(|l| std::fs::canonicalize(&l.path).ok());
    let shadowed: Vec<StoreCopy> = copies
        .into_iter()
        .filter(|c| std::fs::canonicalize(&c.path).ok() != chosen)
        .collect();
    let shim = store_bin.map_or(StoreShim::Absent, |d| classify_store_shim(&d.join(&exe)));
    render_provenance(
        bin,
        found,
        identity,
        version,
        &shadowed,
        &shim,
        homes.first().map(PathBuf::as_path),
    )
}

type ProvenanceKey = Option<(Located, Option<BinaryIdentity>)>;

type LastLine = Mutex<Option<ProvenanceKey>>;

type LastReported = Mutex<BTreeMap<&'static str, std::sync::Arc<LastLine>>>;

#[derive(Clone, PartialEq, Eq, Debug)]
enum Claim {
    Unchanged,
    First,
    Changed(ProvenanceKey),
}

fn claim_report(last: &mut Option<ProvenanceKey>, now: &ProvenanceKey) -> Claim {
    if last.as_ref() == Some(now) {
        return Claim::Unchanged;
    }
    match last.replace(now.clone()) {
        None => Claim::First,
        Some(prev) => Claim::Changed(prev),
    }
}

const SAME_PATH_CHANGED: &str =
    "this same path; a file it runs was since rewritten, replaced or had its metadata changed";

fn change_note(prev: &ProvenanceKey, now: Option<&Located>, home: Option<&Path>) -> String {
    let was = match (prev, now) {
        (None, _) => "NOT FOUND".to_owned(),
        (Some((p, _)), Some(n)) if p.path == n.path => SAME_PATH_CHANGED.to_owned(),
        (Some((p, _)), _) => tilde(&p.path, home),
    };
    format!(" — changed mid-process (was {was})")
}

#[cfg_attr(trust_verify, trust::skip)]
fn report_provenance_on_change(
    bin: &'static str,
    found: Option<&(Located, Option<BinaryIdentity>)>,
    homes: &[PathBuf],
    store_bin: Option<&Path>,
) {
    use std::io::Write;
    static LAST: LastReported = Mutex::new(BTreeMap::new());
    report_on_change(
        &LAST,
        bin,
        found,
        homes,
        store_bin,
        |path| {
            let caps = if bin == "ty" {
                caps_clause(&ty_caps(path))
            } else {
                String::new()
            };
            (probe_version(path), caps)
        },
        |line| {
            let _ = writeln!(std::io::stderr().lock(), "{line}");
        },
    );
}

#[cfg_attr(trust_verify, trust::skip)]
fn report_on_change(
    last: &LastReported,
    bin: &'static str,
    found: Option<&(Located, Option<BinaryIdentity>)>,
    homes: &[PathBuf],
    store_bin: Option<&Path>,
    probe: impl FnOnce(&Path) -> (Option<String>, String),
    write: impl FnOnce(&str),
) {
    let now: ProvenanceKey = found.cloned();
    let mut tools = last
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let slot = std::sync::Arc::clone(tools.entry(bin).or_default());
    drop(tools);
    let mut last = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let prev = match claim_report(&mut last, &now) {
        Claim::Unchanged => return,
        Claim::First => None,
        Claim::Changed(prev) => Some(prev),
    };
    let located = now.as_ref().map(|(l, _)| l);
    let identity = now.as_ref().and_then(|(_, id)| id.as_ref());
    let (version, ending) = located.map(|l| probe(&l.path)).unwrap_or_default();
    let mut line = provenance_line(bin, located, identity, version.as_deref(), homes, store_bin);
    line.push_str(&ending);
    if let Some(prev) = &prev {
        line.push_str(&change_note(
            prev,
            located,
            homes.first().map(PathBuf::as_path),
        ));
    }
    write(&line);
    drop(last);
}

#[cfg_attr(trust_verify, trust::skip)]
fn unreached_store_note(bin: &str) -> String {
    let copies = atpkg_store_bin_dir()
        .as_deref()
        .and_then(Path::parent)
        .map(|prefix| store_copies_in(&prefix.join("store"), &exe_name(bin)))
        .unwrap_or_default();
    if copies.is_empty() {
        return String::new();
    }
    let list = copies
        .iter()
        .map(StoreCopy::describe)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        " The managed store holds {list}, but no discovery tier reaches it (see the \
         `VERIFY ESCALATION TIER: {bin} = NOT FOUND` line)."
    )
}

#[cfg_attr(trust_verify, trust::skip)]
fn is_pending_stub(cand: &Path) -> bool {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(cand) else {
        return false;
    };
    let mut head = Vec::new();
    if file.take(PENDING_STUB_HEAD).read_to_end(&mut head).is_err() {
        return false;
    }
    head.windows(PENDING_STUB_MARKER.len())
        .any(|w| w == PENDING_STUB_MARKER)
}

const PENDING_STUB_HEAD: u64 = 256;

const PENDING_STUB_MARKER: &[u8] = b"atpkg pending-program stub";

fn missing_tool_tail(bin: &str) -> (&'static str, String) {
    let store_note = unreached_store_note(bin);
    if store_note.is_empty() {
        (
            "is not installed",
            format!(" Install: aterm pkg install {bin}"),
        )
    } else {
        (
            "was not found",
            format!("{store_note} Repair: aterm pkg doctor, then aterm pkg repair"),
        )
    }
}

/// How a tiered obligation was discharged; every variant is real coverage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Covered {
    Interpreter,
    InterpreterAndTy,
    TyOnly,
}

/// The obligation did not run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NotRun {
    /// The model whose obligation went unchecked (`Model::name`).
    pub model: &'static str,
}

/// Run `ty check` on a derived model's generated spec and `cfg` (overrides pre-rendered); panics if `ty` cannot run.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn ty_check_derived(ty: &Path, m: &Model, cfg: &str, label: &str) -> (bool, String) {
    let dir = scratch_dir("aterm-tier", m.name);
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join(format!("{}.tla", m.name));
    let cfgp = dir.join(format!("{}.cfg", m.name));
    std::fs::write(&spec, m.to_tla()).expect("write derived spec");
    std::fs::write(&cfgp, cfg).expect("write derived cfg");
    let out = TyCommand::check(ty, &spec, &cfgp)
        .output()
        .unwrap_or_else(|e| panic!("run ty check for {label}: {e}"));
    let _ = std::fs::remove_dir_all(&dir);
    let built = ty_build_stamp(ty);
    let evidence = format!(
        "ty binary: {} [{built}] ({:?})\n--- ty stdout ---\n{}\n--- ty stderr ---\n{}",
        ty.display(),
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    (out.status.success(), evidence)
}

/// A fresh scratch directory for one checker call, `<temp>/<kind>-<model>-<pid>-<n>`.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn scratch_dir(kind: &str, model: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "{kind}-{model}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

const WHOLE_SPACE: [&str; 10] = [
    "--no-auto-por",
    "--no-auto-symmetry",
    "--initial-capacity",
    "8192",
    "--backend",
    "interpreter",
    "--bfs-only",
    "--workers",
    "1",
    "--force",
];

/// One `ty` run, and the only way to start one here.
#[derive(Debug)]
pub struct TyCommand(Command);

impl TyCommand {
    /// `ty check <spec> --config <cfg>`, armed for the whole space.
    #[must_use]
    pub fn check(ty: &Path, spec: &Path, cfg: &Path) -> Self {
        let mut cmd = Command::new(ty);
        cmd.arg("check")
            .arg(spec)
            .arg("--config")
            .arg(cfg)
            .args(WHOLE_SPACE);
        Self(cmd)
    }

    /// `ty trace validate <trace> --spec <spec> --config <cfg>`: a two-state trace, not a state space, so it takes no arming.
    #[must_use]
    pub fn trace_validate(ty: &Path, trace: &Path, spec: &Path, cfg: &Path) -> Self {
        let mut cmd = Command::new(ty);
        cmd.arg("trace")
            .arg("validate")
            .arg(trace)
            .arg("--spec")
            .arg(spec)
            .arg("--config")
            .arg(cfg);
        Self(cmd)
    }

    /// One more argument (`--strict-vacuity`, `--require-exhaustive`, …).
    pub fn arg(&mut self, arg: impl AsRef<std::ffi::OsStr>) -> &mut Self {
        self.0.arg(arg);
        self
    }

    /// Run it, failing closed on a search that stopped early.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn output(&mut self) -> std::io::Result<std::process::Output> {
        with_scratch_ty_home(&mut self.0);
        let out = self.0.output()?;
        if stopped_early(&out) {
            return Err(std::io::Error::other(format!(
                "ty INCOMPLETE: the search was stopped by a resource limit — nothing was proved\n\
                 {}{:?} ({:?})\n--- ty stdout ---\n{}\n--- ty stderr ---\n{}",
                ty_evidence_header(Path::new(self.0.get_program())),
                self.0,
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            )));
        }
        Ok(out)
    }
}

fn stopped_early(out: &std::process::Output) -> bool {
    out.status.code() == Some(5)
        || String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|l| l.starts_with("Model checking stopped:"))
}

/// Spawn a long-lived `ty` co-process (the `--cases` pool's one door) with [`TyCommand::output`]'s scratch HOME and piped stdin and stdout.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn ty_child(
    cmd: &mut aterm_uds::spawnfd::Command,
) -> std::io::Result<aterm_uds::spawnfd::Child> {
    use aterm_uds::spawnfd::Stdio;
    for (key, value) in scratch_ty_env() {
        cmd.env(key, value);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()
}

#[cfg_attr(trust_verify, trust::skip)]
fn with_scratch_ty_home(cmd: &mut Command) {
    for (key, value) in scratch_ty_env() {
        cmd.env(key, value);
    }
}

fn scratch_ty_env() -> Vec<(&'static str, std::path::PathBuf)> {
    let home = std::env::temp_dir().join("aterm-ty-home");
    let _ = std::fs::remove_file(home.join(".ty/cache.json"));
    let mut env = Vec::with_capacity(2);
    if std::env::var_os("TY_CACHE_DIR").is_none() {
        env.push(("TY_CACHE_DIR", home.join(".cache/ty/compiled")));
    }
    env.push(("HOME", home));
    env
}

/// The identity of the checker a transcript came from, path and build stamp, so a gate panic names the binary to act on (`--version` does not tell builds of one version apart; the mtime does).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ty_evidence_header(ty: &Path) -> String {
    format!("ty binary: {} [{}]\n", ty.display(), ty_build_stamp(ty))
}

#[cfg_attr(trust_verify, trust::skip)]
fn ty_build_stamp(ty: &Path) -> String {
    let identity = binary_identity(ty);
    let Some((last, modified)) = identity
        .as_ref()
        .and_then(BinaryIdentity::last)
        .and_then(|l| Some((l, l.file.modified?)))
    else {
        return "mtime unknown".to_string();
    };
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if identity.as_ref().is_some_and(|id| id.links.len() > 1) {
        format!("mtime {secs} (unix) of {}", last.path.display())
    } else {
        format!("mtime {secs} (unix)")
    }
}

/// The distinct states `ty` reports exploring (its `States found: N` line).
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn ty_states_explored(evidence: &str) -> Option<u64> {
    evidence
        .lines()
        .find_map(|l| l.trim().strip_prefix("States found:"))
        .and_then(|n| n.trim().parse().ok())
}

#[cfg_attr(trust_verify, trust::skip)]
fn assert_same_space_explored(m: &Model, interp_states: usize, evidence: &str, label: &str) {
    let Some(ty_states) = ty_states_explored(evidence) else {
        panic!(
            "{label}: {} — `ty` returned a CLEAN verdict with no `States found:` line, so there \
             is no evidence it explored anything. The escalation tier cannot be credited on an \
             unparseable transcript (output-format drift?).\n{evidence}",
            m.name
        )
    };
    assert!(
        ty_states == interp_states as u64,
        "{label}: {} — TIER DISAGREEMENT ON THE EXPLORED SPACE: the interpreter walked \
         {interp_states} reachable states, `ty` reports {ty_states}. Both tiers claim an \
         exhaustive walk of the SAME machine, so these must be equal; they are not, which means \
         one tier's \"clean\" is about a space the other never saw. A `ty` count that is SMALLER \
         is a false proof (a state-space reduction that dropped reachable behaviour); a LARGER \
         one means the emitted TLA+ admits states the model does not.\n{evidence}",
        m.name
    );
}

/// Tiered prove-and-catch (the `Buggy` convention).
#[cfg_attr(trust_verify, trust::skip)]
pub fn prove_and_catch_tiered(m: &Model, label: &str) -> Result<Covered, NotRun> {
    let interp_states = if m.fn_vars.is_empty() {
        Some(interp::prove_and_catch(m))
    } else {
        None
    };
    let interp_ran = interp_states.is_some();
    match find_ty() {
        Some(ty) => {
            let (ok, evidence) = ty_check_derived(&ty, m, &m.to_cfg(), label);
            assert!(
                ok,
                "{label}: ty check FAILED at Buggy=0 on {}{}\n{evidence}\n--- generated ---\n{}",
                m.name,
                if interp_ran {
                    " — TIER DISAGREEMENT (interpreter proved it; checker bug?)"
                } else {
                    ""
                },
                m.to_tla()
            );
            if let Some(n) = interp_states {
                assert_same_space_explored(m, n, &evidence, label);
            }
            let (caught_ok, evidence) =
                ty_check_derived(&ty, m, &m.to_cfg_with(&[("Buggy", 1)]), label);
            assert!(
                !caught_ok,
                "{label}: ty found NO counterexample at Buggy=1 on {}{}\n{evidence}",
                m.name,
                if interp_ran {
                    " — TIER DISAGREEMENT (interpreter caught it; checker bug?)"
                } else {
                    " — the property is trivial / does not catch the defect"
                }
            );
            eprintln!(
                "{label}: {} {}proven (Buggy=0) and caught (Buggy=1) by ty.",
                m.name,
                if interp_ran { "additionally " } else { "" }
            );
            if interp_ran {
                Ok(Covered::InterpreterAndTy)
            } else {
                Ok(Covered::TyOnly)
            }
        }
        None if interp_ran => Ok(Covered::Interpreter),
        None => Err(NotRun { model: m.name }),
    }
}

/// Prove-and-catch a scalar model, asserting the shape that makes the `_tiered` form's `Err` unreachable.
#[cfg_attr(trust_verify, trust::skip)]
pub fn prove_and_catch_scalar(m: &Model, label: &str) -> Covered {
    assert_scalar(m, label, "prove_and_catch_scalar");
    match prove_and_catch_tiered(m, label) {
        Ok(c) => c,
        Err(n) => unreachable!("{label}: scalar model {} reported {n:?}", m.name),
    }
}

/// Prove, catch and multiply.
#[cfg_attr(trust_verify, trust::skip)]
pub fn prove_catch_and_multiply_scalar(
    m: &Model,
    scenarios: &[&[(&'static str, i64)]],
    buggy_at: &[(&'static str, i64)],
    label: &str,
) -> Covered {
    assert_scalar(m, label, "prove_catch_and_multiply_scalar");
    let committed = |name: &str| -> i64 {
        m.consts
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| {
                panic!(
                    "{label}: {} declares no `{name}` constant — a MULTIPLY obligation needs \
                     the `Buggy` / `Local` / `Instances` dials to exist",
                    m.name
                )
            })
    };
    assert_eq!(
        committed("Buggy"),
        0,
        "{label}: {} commits Buggy != 0",
        m.name
    );
    assert_eq!(
        committed("Local"),
        0,
        "{label}: {} commits Local != 0",
        m.name
    );
    let instances = committed("Instances");
    assert!(
        instances >= 2,
        "{label}: {} commits Instances = {instances} — the committed world must already \
         contain SEVERAL enforcers, or G2 proves nothing about aggregation",
        m.name
    );
    assert!(
        !scenarios.is_empty(),
        "{label}: {} was given no scenario corners to prove over",
        m.name
    );

    let ty = find_ty();
    let gate = |extra: &[(&'static str, i64)], scenario: &[(&'static str, i64)], gate: &str| {
        let mut cfg: Vec<(&'static str, i64)> = scenario.to_vec();
        for (n, v) in extra {
            match cfg.iter_mut().find(|(c, _)| c == n) {
                Some(slot) => slot.1 = *v,
                None => cfg.push((*n, *v)),
            }
        }
        let interp_verdict = interp::bmc(&interp::with_consts(m, &cfg));
        let ty_verdict = ty
            .as_ref()
            .map(|t| ty_check_derived(t, m, &m.to_cfg_with(&cfg), label));
        if let Some((ok, evidence)) = &ty_verdict {
            assert_eq!(
                *ok,
                interp_verdict.is_ok(),
                "{label}: {} TIER DISAGREEMENT at {gate} ({cfg:?}) — interpreter said {}, ty \
                 said {}\n{evidence}",
                m.name,
                if interp_verdict.is_ok() {
                    "HOLDS"
                } else {
                    "VIOLATED"
                },
                if *ok { "HOLDS" } else { "VIOLATED" },
            );
        }
        (cfg, interp_verdict)
    };

    for scenario in scenarios {
        let (cfg, verdict) = gate(&[], scenario, "G2 PROVE");
        match verdict {
            Ok(n) => eprintln!(
                "{label}: G2 PROVE {} holds over {n} states at {cfg:?}.",
                m.name
            ),
            Err((st, inv)) => panic!(
                "{label}: G2 PROVE FAILED — {} invariant `{inv}` VIOLATED at {st:?} ({cfg:?})",
                m.name
            ),
        }
    }
    {
        let (cfg, verdict) = gate(&[("Buggy", 1)], buggy_at, "G3 CATCH");
        match verdict {
            Ok(n) => panic!(
                "{label}: G3 CATCH FAILED — {} found NO counterexample over {n} states at \
                 {cfg:?}; the `Buggy` mutant is not caught, so the property is trivial",
                m.name
            ),
            Err((st, inv)) => {
                eprintln!(
                    "{label}: G3 CATCH {} `{inv}` VIOLATED at {st:?} ({cfg:?}).",
                    m.name
                );
            }
        }
    }
    for scenario in scenarios {
        let (cfg, verdict) = gate(&[("Local", 1)], scenario, "G4 MULTIPLY");
        match verdict {
            Ok(n) => panic!(
                "{label}: G4 MULTIPLY FAILED — {} stayed GREEN over {n} states at {cfg:?} with \
                 every enforcer applying the bound LOCALLY. That is the per-instance blindness \
                 this model exists to expose: either the aggregate invariant is not charged \
                 against every instance, or the `Local` dial does not actually make the \
                 enforcers blind to each other. Fix the MODEL — this gate is the machine-\
                 checked form of the sentence a scope-cardinality claim asserts in prose.",
                m.name
            ),
            Err((st, inv)) => {
                let witness: Vec<String> = st
                    .iter()
                    .filter(|(_, v)| **v != 0)
                    .map(|(k, v)| format!("{k}:{v}"))
                    .collect();
                eprintln!(
                    "{label}: G4 MULTIPLY {} `{inv}` VIOLATED at {{{}}} ({cfg:?}) — every \
                     enforcer individually within budget.",
                    m.name,
                    witness.join(", ")
                );
            }
        }
    }
    for scenario in scenarios {
        let (cfg, verdict) = gate(
            &[("Local", 1), ("Instances", 1)],
            scenario,
            "G5 ATTRIBUTION",
        );
        match verdict {
            Ok(n) => eprintln!(
                "{label}: G5 ATTRIBUTION {} holds over {n} states at {cfg:?} — the G4 \
                 counterexample is due to the instance COUNT, nothing else.",
                m.name
            ),
            Err((st, inv)) => panic!(
                "{label}: G5 ATTRIBUTION FAILED — {} invariant `{inv}` VIOLATED at {st:?} \
                 ({cfg:?}) with a SINGLE enforcer. The `Local` dial breaks the model even \
                 without multiplication, so G4's counterexample proves nothing about \
                 instance count.",
                m.name
            ),
        }
    }
    if ty.is_some() {
        eprintln!(
            "{label}: {} proved/caught/multiplied on BOTH tiers.",
            m.name
        );
        Covered::InterpreterAndTy
    } else {
        Covered::Interpreter
    }
}

#[cfg_attr(trust_verify, trust::skip)]
fn assert_scalar(m: &Model, label: &str, caller: &str) {
    assert!(
        m.fn_vars.is_empty(),
        "{label}: {} is FUNCTION-VALUED, so `{caller}` cannot discharge it — the interpreter \
         cannot evaluate it and the `ty` tier may be absent. Call the `_tiered` form and state a \
         policy for the `NotRun` case.",
        m.name
    );
}

/// Refuse a `ty` dead set measured on a smaller space than the interpreter walked.
#[cfg_attr(trust_verify, trust::skip)]
pub fn assert_ty_saw_whole_space(m: &Model, evidence: &str, label: &str) {
    let committed = interp::with_buggy(m, 0);
    let interp_states = match interp::bmc(&committed) {
        Ok(n) => n,
        Err((state, invariant)) => panic!(
            "{label}: {} — the interpreter finds invariant `{invariant}` VIOLATED at {state:?} \
             at the committed config, while `ty` returned a clean verdict. A clean `ty` over a \
             violated model is a false proof, not a disagreement about coverage.\n{evidence}",
            m.name
        ),
    };
    assert_same_space_explored(m, interp_states, evidence, label);
}

/// `ty`'s dead-action report for one configuration, held to the interpreter's.
pub fn dead_actions_agree(
    m: &Model,
    ty_reported_dead: &[&str],
) -> Result<BTreeSet<&'static str>, String> {
    for d in ty_reported_dead {
        if !m.actions.iter().any(|a| a.name == *d) {
            return Err(format!(
                "{}: ty reported dead action `{d}` that the model does not declare — \
                 spec/emission drift",
                m.name
            ));
        }
    }
    let fired = interp::fired_actions(m);
    let interp_dead: BTreeSet<&'static str> = m
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !fired.contains(a))
        .collect();
    let ty_dead: BTreeSet<&str> = ty_reported_dead.iter().copied().collect();
    if ty_dead != interp_dead.iter().copied().collect() {
        return Err(format!(
            "{}: TIER DISAGREEMENT on the dead set — ty reports {ty_dead:?} but the \
             interpreter finds {interp_dead:?}",
            m.name
        ));
    }
    Ok(interp_dead)
}

/// Whether the actions of a model that never fire with no mutant on are negative controls rather than vacuity.
pub fn audit_dead_negative_controls(configs: &[Model]) -> Result<usize, String> {
    let Some(m) = configs.first() else {
        return Ok(0);
    };
    if !m.fn_vars.is_empty() {
        return Err(format!(
            "{}: function-valued model — the interpreter cannot verify negative controls, so \
             no dead-action relaxation is available; keep the model strictly non-vacuous",
            m.name
        ));
    }
    let mut dead: BTreeSet<&'static str> = m.actions.iter().map(|a| a.name).collect();
    for config in configs {
        let fired = interp::fired_actions(&interp::with_buggy(config, 0));
        dead.retain(|a| !fired.contains(a));
    }
    if dead.is_empty() {
        return Ok(0);
    }
    if !m.consts.iter().any(|(n, v)| *n == "Buggy" && *v == 0) {
        return Err(format!(
            "{}: dead action(s) {dead:?} in a model with no committed `Buggy = 0` dial — there \
             is no catch config under which they could be negative controls; REAL vacuity",
            m.name
        ));
    }
    let mutants: Vec<Model> = configs
        .iter()
        .flat_map(|config| {
            m.mutant_dials()
                .into_iter()
                .map(|dial| interp::with_buggy(config, dial))
        })
        .collect();
    // The mutants are walked at once. A verdict on one action does not depend on which others a
    // walk watches, so `stepped` is the same set in any order; `fires_lawfully` is read only
    // for actions no mutant steps, which every walk watched.
    let verdicts: Mutex<(BTreeSet<&'static str>, BTreeSet<&'static str>)> = Mutex::default();
    let open = || -> Vec<&'static str> {
        let held = verdicts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        dead.difference(&held.0).copied().collect()
    };
    crate::check::each(&mutants, |mutant| {
        let open = open();
        if open.is_empty() {
            return;
        }
        let steps = interp::violation_steps(mutant, &open);
        let mut held = verdicts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (stepped, fires_lawfully) = &mut *held;
        for (d, verdict) in open.into_iter().zip(steps) {
            match verdict {
                Some(true) => stepped.insert(d),
                Some(false) => fires_lawfully.insert(d),
                None => false,
            };
        }
    });
    let (stepped, fires_lawfully) = verdicts
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let fired_under: Vec<std::sync::OnceLock<BTreeSet<&'static str>>> =
        mutants.iter().map(|_| std::sync::OnceLock::new()).collect();
    let baseline_of: Vec<std::sync::OnceLock<Result<usize, (interp::State, &'static str)>>> =
        mutants.iter().map(|_| std::sync::OnceLock::new()).collect();
    let unstepped: Vec<&'static str> = dead.difference(&stepped).copied().collect();
    // Each action's isolation is its own; the first failing action, in name order, is the
    // error.
    let isolations = crate::check::each(&unstepped, |&d| -> Result<(), String> {
        if !fires_lawfully.contains(d)
            && !mutants.iter().zip(&fired_under).any(|(mutant, fired)| {
                fired
                    .get_or_init(|| interp::fired_actions(mutant))
                    .contains(d)
            })
        {
            return Err(format!(
                "{}: action `{d}` is dead with no mutant on AND fires under no mutant dial \
                 either — REAL vacuity (a typo'd guard, or a configuration no `scenario` \
                 declares)",
                m.name
            ));
        }
        let (mut unsafe_baseline, mut any_safe, mut fires_alone) = (None, false, false);
        let isolated = mutants.iter().zip(&baseline_of).any(|(mutant, baseline)| {
            let baseline = baseline.get_or_init(|| {
                let mut baseline = mutant.clone();
                baseline.actions.retain(|a| !dead.contains(a.name));
                interp::bmc(&baseline)
            });
            if let Err(broken) = baseline {
                unsafe_baseline.get_or_insert_with(|| broken.clone());
                return false;
            }
            any_safe = true;
            let mut alone = mutant.clone();
            alone
                .actions
                .retain(|a| !dead.contains(a.name) || a.name == d);
            let (verdict, fired) = interp::bmc_fired(&alone);
            fires_alone |= fired.contains(d);
            verdict.is_err()
        });
        if isolated {
            return Ok(());
        }
        Err(match unsafe_baseline.filter(|_| !any_safe) {
            Some((state, invariant)) => format!(
                "{}: action `{d}` is no violation step, and with every dead action removed \
                 each mutant still violates (`{invariant}` at {state:?}) — another branch \
                 supplies the counterexample, so `{d}` cannot be credited as an independently \
                 caught negative control",
                m.name
            ),
            _ if !fires_alone => format!(
                "{}: action `{d}` fires only beside another dead action — it is no \
                 independently exercisable negative control",
                m.name
            ),
            _ => format!(
                "{}: action `{d}` fires when added alone to the mutant baseline, but NO \
                 invariant is violated — this mutant is independently caught by nothing, so it \
                 proves nothing",
                m.name
            ),
        })
    });
    isolations.into_iter().collect::<Result<(), String>>()?;
    Ok(dead.len())
}

/// Tiered deadlock freedom (the `CHECK_DEADLOCK` protocol).
#[cfg_attr(trust_verify, trust::skip)]
pub fn deadlock_free_and_catches_tiered(
    m: &Model,
    is_final: impl Fn(&interp::State) -> bool,
    label: &str,
) -> Covered {
    assert_scalar(m, label, "deadlock_free_and_catches_tiered");
    assert!(
        interp::find_deadlock(&interp::with_buggy(m, 0), &is_final).is_none(),
        "{label}: {} (Buggy=0) must be DEADLOCK-FREE (interpreter tier)",
        m.name
    );
    let wedge = interp::find_deadlock(&interp::with_buggy(m, 1), &is_final);
    assert!(
        wedge.is_some(),
        "{label}: {} (Buggy=1) MUST reach a wedge (interpreter tier)",
        m.name
    );
    eprintln!(
        "{label}: {} deadlock-free (Buggy=0) and wedge caught at {:?} (interpreter, Buggy=1).",
        m.name,
        wedge.unwrap()
    );
    match find_ty() {
        Some(ty) => {
            let dir = scratch_dir("aterm-dl", m.name);
            std::fs::create_dir_all(&dir).expect("mk tempdir");
            let spec = dir.join(format!("{}.tla", m.name));
            std::fs::write(&spec, m.to_tla()).expect("write spec");
            let run = |cfg_name: &str, cfg: String| -> (bool, String) {
                let cfgp = dir.join(cfg_name);
                std::fs::write(&cfgp, cfg).expect("write cfg");
                let out = TyCommand::check(&ty, &spec, &cfgp)
                    .output()
                    .expect("run ty check");
                (
                    out.status.success(),
                    format!(
                        "{}{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    ),
                )
            };
            let (ok, out) = run("ok.cfg", m.to_cfg_deadlock_with(&[]));
            assert!(
                ok,
                "{label}: ty says {} (Buggy=0) deadlocks — TIER DISAGREEMENT\n{out}",
                m.name
            );
            let (bug_ok, bug_out) = run("bug.cfg", m.to_cfg_deadlock_with(&[("Buggy", 1)]));
            assert!(
                !bug_ok,
                "{label}: ty found NO wedge at Buggy=1 on {} — TIER DISAGREEMENT\n{bug_out}",
                m.name
            );
            assert!(
                bug_out.contains("Deadlock"),
                "{label}: {} (Buggy=1) failure must be a DEADLOCK, not an invariant \
                 violation\n{bug_out}",
                m.name
            );
            let _ = std::fs::remove_dir_all(&dir);
            eprintln!(
                "{label}: {} additionally deadlock-checked by ty (free at Buggy=0, wedge at Buggy=1).",
                m.name
            );
            Covered::InterpreterAndTy
        }
        None => Covered::Interpreter,
    }
}

/// The interpreter's full discharge of a [`Liveness`] obligation, returned rather than panicked so the registry sweep can name every failure at once.
#[cfg_attr(trust_verify, trust::skip)]
pub fn audit_liveness(m: &Model, live: &Liveness) -> Result<usize, String> {
    liveness_preflight(m, live)?;
    audit_liveness_on(m, live, &interp::Space::of(&interp::with_buggy(m, 0)))
}

#[cfg_attr(trust_verify, trust::skip)]
fn liveness_preflight(m: &Model, live: &Liveness) -> Result<(), String> {
    let label = format!("{} liveness `{}`", m.name, live.name);
    if !m.fn_vars.is_empty() {
        return Err(format!(
            "{label}: function-valued model — the interpreter cannot walk it"
        ));
    }
    if !m.consts.iter().any(|(n, v)| *n == "Buggy" && *v == 0) {
        return Err(format!(
            "{label}: no committed `Buggy = 0` dial, so no mutant can be isolated"
        ));
    }
    if live.mutants.is_empty() {
        return Err(format!(
            "{label}: names no mutant — a liveness property nothing breaks is a ghost"
        ));
    }
    let declared: BTreeSet<&str> = m.actions.iter().map(|a| a.name as &str).collect();
    for taken in [live.name.to_string(), live.spec_name()] {
        let clashes = ["Init", "Next", "Spec", "vars"].contains(&taken.as_str())
            || declared.contains(taken.as_str())
            || m.invariants.iter().any(|i| i.name == taken)
            || m.vars.iter().any(|v| v.name == taken)
            || m.consts.iter().any(|(c, _)| *c == taken);
        if clashes {
            return Err(format!(
                "{label}: `{taken}` is already a name in the model; the emitted module \
                 would define it twice"
            ));
        }
    }
    Ok(())
}

#[cfg_attr(trust_verify, trust::skip)]
fn audit_liveness_on(
    m: &Model,
    live: &Liveness,
    committed: &interp::Space,
) -> Result<usize, String> {
    let label = format!("{} liveness `{}`", m.name, live.name);
    let declared: BTreeSet<&str> = m.actions.iter().map(|a| a.name as &str).collect();
    let fired = committed.fired();
    for name in live.weak.iter().chain(&live.strong) {
        if !declared.contains(name) {
            return Err(format!(
                "{label}: fairness names undeclared action `{name}`"
            ));
        }
        if !fired.contains(name) {
            return Err(format!(
                "{label}: fairness names `{name}`, which never fires at the committed \
                 config — an assumption about a dead action assumes nothing"
            ));
        }
    }
    let dead: BTreeSet<&str> = declared
        .iter()
        .copied()
        .filter(|a| !fired.contains(a))
        .collect();
    for mutant in &live.mutants {
        if !dead.contains(mutant) {
            return Err(format!(
                "{label}: mutant `{mutant}` is not an action dead at the committed config \
                 (dead: {dead:?}) — a negative control must be one"
            ));
        }
    }
    if let Some(cycle) = committed.nonprogress_under(live) {
        return Err(format!(
            "{label}: VIOLATED at the committed config (Buggy=0) — a fair behaviour \
             leaves the goal for good:\n{cycle}"
        ));
    }
    let assumptions = live
        .weak
        .iter()
        .map(|a| (a, "weak"))
        .chain(live.strong.iter().map(|a| (a, "strong")));
    for (name, kind) in assumptions {
        let mut without = live.clone();
        without.weak.retain(|a| a != name);
        without.strong.retain(|a| a != name);
        if committed.nonprogress_under(&without).is_none() {
            return Err(format!(
                "{label}: the {kind} fairness of `{name}` is not needed — the property holds \
                 without it. Drop it, so the verdict does not claim to rest on it"
            ));
        }
        if kind == "strong" {
            without.weak.push(name);
            if committed.nonprogress_under(&without).is_none() {
                return Err(format!(
                    "{label}: `{name}` needs only WEAK fairness — the property holds with it; \
                     state the weaker assumption"
                ));
            }
        }
    }
    let mut baseline = interp::with_buggy(m, 1);
    baseline.actions.retain(|a| !dead.contains(a.name));
    if let Some(cycle) = interp::nonprogress_under(&baseline, live) {
        return Err(format!(
            "{label}: the Buggy=1 baseline with every committed-dead action removed \
             already breaks it, so no mutant's catch is its own:\n{cycle}"
        ));
    }
    for mutant in &live.mutants {
        if interp::nonprogress_under(&isolate_mutant(m, fired, mutant), live).is_none() {
            return Err(format!(
                "{label}: mutant `{mutant}`, alone on the Buggy=1 baseline, does not break \
                 it — the property cannot see the defect that mutant is"
            ));
        }
    }
    Ok(live.mutants.len())
}

/// The `Buggy = 1` copy of `m` with every committed-dead action removed except `mutant`: the healthy lane plus that one defect.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn buggy_baseline_with(m: &Model, mutant: &str) -> Model {
    isolate_mutant(m, &interp::fired_actions(&interp::with_buggy(m, 0)), mutant)
}

/// [`buggy_baseline_with`] for a caller that already holds the actions firing at the committed config, so the space is not walked again to learn them.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn isolate_mutant(m: &Model, fired: &BTreeSet<&'static str>, mutant: &str) -> Model {
    let mut isolated = interp::with_buggy(m, 1);
    isolated
        .actions
        .retain(|a| fired.contains(a.name) || a.name == mutant);
    isolated
}

/// Run `ty check` on a liveness module (`Liveness::to_tla`) with its cfg, in a directory of its own per call ([`scratch_dir`]).
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn ty_check_liveness(
    ty: &Path,
    m: &Model,
    module: &str,
    cfg: &str,
    tag: &str,
    label: &str,
) -> (bool, String) {
    let dir = scratch_dir(&format!("aterm-live-{tag}"), m.name);
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join(format!("{}.tla", m.name));
    let cfgp = dir.join(format!("{}.cfg", m.name));
    std::fs::write(&spec, module).expect("write liveness spec");
    std::fs::write(&cfgp, cfg).expect("write liveness cfg");
    let out = TyCommand::check(ty, &spec, &cfgp)
        .output()
        .unwrap_or_else(|e| panic!("run ty check for {label}: {e}"));
    let _ = std::fs::remove_dir_all(&dir);
    let evidence = format!(
        "{}({:?})\n--- ty stdout ---\n{}\n--- ty stderr ---\n{}",
        ty_evidence_header(ty),
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    (out.status.success(), evidence)
}

/// Tiered liveness: [`audit_liveness`] on the interpreter, plus `ty` checking `PROPERTY <name>` under [`Liveness::to_tla`] when checkers run.
#[cfg_attr(trust_verify, trust::skip)]
pub fn liveness_proves_and_catches_tiered(m: &Model, live: &Liveness, label: &str) -> Covered {
    assert_scalar(m, label, "liveness_proves_and_catches_tiered");
    let committed = interp::with_buggy(m, 0);
    let ty = find_ty();
    let (space, committed_check, mutant_checks) = std::thread::scope(|scope| {
        let committed_check = ty.as_deref().map(|ty| {
            let (module, cfg) = (
                live.to_tla(&committed),
                live.to_cfg_with(m, &[("Buggy", 0)]),
            );
            scope.spawn(move || ty_check_liveness(ty, m, &module, &cfg, "committed", label))
        });
        let (caught, space) = match liveness_preflight(m, live).and_then(|()| {
            let space = interp::Space::of(&committed);
            audit_liveness_on(m, live, &space).map(|caught| (caught, space))
        }) {
            Ok(audited) => audited,
            Err(why) => panic!("{label}: {why}"),
        };
        eprintln!(
            "{label}: {} `{}` proven under its fairness (Buggy=0) and broken by {caught} \
             mutant(s) alone (interpreter).",
            m.name, live.name
        );
        let mutant_checks: Vec<_> = match ty.as_deref() {
            Some(ty) => live
                .mutants
                .iter()
                .map(|mutant| {
                    let isolated = isolate_mutant(m, space.fired(), mutant);
                    let (module, cfg) =
                        (live.to_tla(&isolated), live.to_cfg_with(m, &[("Buggy", 1)]));
                    let check =
                        scope.spawn(move || ty_check_liveness(ty, m, &module, &cfg, mutant, label));
                    (mutant, check)
                })
                .collect(),
            None => Vec::new(),
        };
        let join = |check: std::thread::ScopedJoinHandle<'_, (bool, String)>| {
            check
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        };
        (
            space,
            committed_check.map(join),
            mutant_checks
                .into_iter()
                .map(|(mutant, check)| (mutant, check.join()))
                .collect::<Vec<_>>(),
        )
    });
    let Some((ok, evidence)) = committed_check else {
        return Covered::Interpreter;
    };
    assert!(
        ok,
        "{label}: ty says {} `{}` FAILS at Buggy=0 — TIER DISAGREEMENT (the interpreter \
         proved it)\n{evidence}\n--- generated ---\n{}",
        m.name,
        live.name,
        live.to_tla(&committed)
    );
    let reachable = space.states();
    assert_same_space_explored(m, reachable, &evidence, label);
    for (mutant, check) in mutant_checks {
        let (held, evidence) = check.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        assert!(
            !held,
            "{label}: ty found NO violation of {} `{}` with mutant `{mutant}` alone — TIER \
             DISAGREEMENT (the interpreter found a fair non-progress cycle)\n{evidence}",
            m.name, live.name
        );
        assert!(
            evidence.contains("Temporal properties were violated")
                || evidence.contains("Liveness violation"),
            "{label}: ty rejected {} with mutant `{mutant}` alone, but not with a TEMPORAL \
             violation — a parse or config failure is not a catch\n{evidence}",
            m.name
        );
    }
    eprintln!(
        "{label}: {} `{}` additionally proven (Buggy=0, {reachable} states) and broken by \
         each mutant by ty.",
        m.name, live.name
    );
    Covered::InterpreterAndTy
}

/// Tiered per-transition conformance (the Tier-1 `ty trace validate` twin).
#[cfg_attr(trust_verify, trust::skip)]
pub fn validate_transition_tiered(
    m: &Model,
    overrides: &[(&'static str, i64)],
    prev: &BTreeMap<&'static str, i64>,
    next: &BTreeMap<&'static str, i64>,
    action: Option<&str>,
    label: &str,
) -> (bool, String) {
    let m_eff = interp::with_consts(m, overrides);
    let admitted = interp::admits(&m_eff, prev, next);
    let interp_ok = match action {
        Some(a) => m_eff.successors(a, prev).contains(next),
        None => admitted.is_some(),
    };
    let interp_why = match admitted {
        Some(a) => format!("interpreter: admitted by action `{a}`"),
        None => "interpreter: NO action admits this transition".to_string(),
    };

    let Some(ty) = find_ty() else {
        return (interp_ok, interp_why);
    };

    let spec_text = m.transition_spec();
    let cfg_text = m.transition_cfg(overrides);
    let trace_text = transition_trace_json(m, prev, next, action.or(admitted));
    let (ty_ok, combined) = ty_trace_verdict(&ty, m.name, spec_text, cfg_text, trace_text, label);
    assert!(
        ty_ok == interp_ok,
        "{label}: TIER DISAGREEMENT on {} transition {prev:?} -> {next:?}: \
         interpreter={interp_ok} ty={ty_ok} (checker bug — never swallow this)\n\
         {interp_why}\n--- ty ---\n{combined}",
        m.name
    );
    (ty_ok, combined)
}

#[cfg_attr(trust_verify, trust::skip)]
fn ty_trace_verdict(
    ty: &Path,
    model: &str,
    spec_text: String,
    cfg_text: String,
    trace_text: String,
    label: &str,
) -> (bool, String) {
    memoized_verdict(
        ty,
        spec_text,
        cfg_text,
        trace_text,
        |checker, spec, cfg, trace| {
            checker
                .and_then(|checker| cases_verdict(checker, model, spec, cfg, trace, label))
                .unwrap_or_else(|| ty_trace_per_process(ty, model, spec, cfg, trace, label))
        },
    )
}

type TyChecker = (PathBuf, BinaryIdentity);

#[cfg_attr(trust_verify, trust::skip)]
fn memoized_verdict(
    ty: &Path,
    spec_text: String,
    cfg_text: String,
    trace_text: String,
    run: impl FnOnce(Option<&TyChecker>, &str, &str, &str) -> (bool, String),
) -> (bool, String) {
    type Verdicts = BTreeMap<(TyChecker, String), BTreeMap<(String, String), (bool, String)>>;
    static SEEN: Mutex<Verdicts> = Mutex::new(BTreeMap::new());
    let Some(identity) = binary_identity(ty) else {
        return run(None, &spec_text, &cfg_text, &trace_text);
    };
    let question = ((ty.to_path_buf(), identity), spec_text);
    let step = (cfg_text, trace_text);
    let seen = SEEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&question)
        .and_then(|by_step| by_step.get(&step).cloned());
    if let Some(verdict) = seen {
        return verdict;
    }
    let verdict = run(Some(&question.0), &question.1, &step.0, &step.1);
    if binary_identity(ty).as_ref() == Some(&question.0.1) {
        SEEN.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(question)
            .or_default()
            .insert(step, verdict.clone());
    }
    verdict
}

#[cfg_attr(trust_verify, trust::skip)]
fn ty_trace_per_process(
    ty: &Path,
    model: &str,
    spec_text: &str,
    cfg_text: &str,
    trace_text: &str,
    label: &str,
) -> (bool, String) {
    let dir = scratch_dir("aterm-conf", model);
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join(format!("{model}.tla"));
    let cfg = dir.join(format!("{model}.cfg"));
    let trace = dir.join("t.json");
    std::fs::write(&spec, spec_text).expect("write spec");
    std::fs::write(&cfg, cfg_text).expect("write cfg");
    std::fs::write(&trace, trace_text).expect("write trace");
    let out = TyCommand::trace_validate(ty, &trace, &spec, &cfg)
        .output()
        .unwrap_or_else(|e| panic!("run ty trace validate for {label}: {e}"));
    let verdict = (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    );
    let _ = std::fs::remove_dir_all(&dir);
    verdict
}

const CASES_HANG: std::time::Duration = std::time::Duration::from_secs(60);

const CASES_POOL_CAP: usize = 16;

const CASES_TRIES: u8 = 2;

type CasesSlot = std::sync::Arc<Mutex<Option<CasesChild>>>;

#[cfg_attr(trust_verify, trust::skip)]
fn cases_verdict(
    checker: &TyChecker,
    model: &str,
    spec: &str,
    cfg: &str,
    trace: &str,
    label: &str,
) -> Option<(bool, String)> {
    use std::sync::PoisonError;
    type Failures = BTreeMap<TyChecker, BTreeMap<String, BTreeMap<String, u8>>>;
    static POOL: Mutex<Vec<(TyChecker, String, String, CasesSlot)>> = Mutex::new(Vec::new());
    static FAILED: Mutex<Failures> = Mutex::new(BTreeMap::new());
    let ty = checker.0.as_path();
    let failures = || {
        FAILED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(checker)
            .and_then(|by_spec| by_spec.get(spec))
            .and_then(|by_cfg| by_cfg.get(cfg).copied())
            .unwrap_or(0)
    };
    let caps = ty_caps(ty);
    let has = |cap: &str| caps.iter().any(|c| c == cap);
    if !(has(CAP_TRACE_CASES) && has(CAP_BUDGET_SELF))
        || trace.contains('\n')
        || failures() >= CASES_TRIES
    {
        return None;
    }
    let slot = {
        let mut pool = POOL.lock().unwrap_or_else(PoisonError::into_inner);
        let at = pool
            .iter()
            .position(|(k, s, c, _)| k == checker && s == spec && c == cfg);
        let entry = match at {
            Some(at) => pool.remove(at),
            None => {
                if pool.len() >= CASES_POOL_CAP {
                    // Its child is killed once no case holds it any more.
                    pool.remove(0);
                }
                (
                    checker.clone(),
                    spec.to_string(),
                    cfg.to_string(),
                    CasesSlot::default(),
                )
            }
        };
        let slot = std::sync::Arc::clone(&entry.3);
        pool.push(entry);
        slot
    };
    let mut held = slot.lock().unwrap_or_else(PoisonError::into_inner);
    let answer = match held.as_mut() {
        Some(child) => child.case(trace),
        None if failures() >= CASES_TRIES => return None,
        None => CasesChild::start(ty, model, spec, cfg, trace)
            .and_then(|child| held.insert(child).case(trace)),
    };
    match answer {
        Ok(answer) => {
            if answer.end {
                *held = None;
            }
            drop(held);
            assert!(
                !answer.unevaluated,
                "{label}: ty COULD NOT EVALUATE this `{model}` transition — an evaluation \
                 error is not a refutation, so neither tier's verdict stands\n{}",
                answer.evidence
            );
            Some((answer.ok, answer.evidence))
        }
        Err(why) => {
            *held = None;
            drop(held);
            let tries = {
                let mut failed = FAILED.lock().unwrap_or_else(PoisonError::into_inner);
                let n = failed
                    .entry(checker.clone())
                    .or_default()
                    .entry(spec.to_string())
                    .or_default()
                    .entry(cfg.to_string())
                    .or_default();
                *n += 1;
                *n
            };
            let then = if tries >= CASES_TRIES {
                "so is every later case of this spec and cfg in this process"
            } else {
                "the next case respawns the co-process once"
            };
            eprintln!(
                "{label}: ty --cases co-process for `{model}` failed, so this case is answered by \
                 one `ty` process of its own and {then}: {why}"
            );
            None
        }
    }
}

struct CasesAnswer {
    ok: bool,
    evidence: String,
    end: bool,
    unevaluated: bool,
}

struct CasesChild {
    child: aterm_uds::spawnfd::Child,
    stdin: std::process::ChildStdin,
    lines: std::sync::mpsc::Receiver<String>,
    stderr: std::fs::File,
    who: String,
    next_id: u64,
}

impl CasesChild {
    #[cfg_attr(trust_verify, trust::skip)]
    fn start(ty: &Path, model: &str, spec: &str, cfg: &str, trace: &str) -> Result<Self, String> {
        let dir = scratch_dir("aterm-cases", model);
        let started = Self::spawn(ty, model, spec, cfg, &dir).and_then(|mut child| {
            match child.expect_header(model, trace) {
                Ok(()) => Ok(child),
                Err(why) => Err(child.failure(&why)),
            }
        });
        let _ = std::fs::remove_dir_all(&dir);
        started
    }

    #[cfg_attr(trust_verify, trust::skip)]
    fn spawn(ty: &Path, model: &str, spec: &str, cfg: &str, dir: &Path) -> Result<Self, String> {
        let io = |e: std::io::Error| format!("ty (cases) {}: {e}", ty.display());
        std::fs::create_dir_all(dir).map_err(io)?;
        let spec_path = dir.join(format!("{model}.tla"));
        let cfg_path = dir.join(format!("{model}.cfg"));
        let stderr_path = dir.join("stderr");
        std::fs::write(&spec_path, spec).map_err(io)?;
        std::fs::write(&cfg_path, cfg).map_err(io)?;
        let stderr = std::fs::File::create(&stderr_path).map_err(io)?;
        let stderr_reader = std::fs::File::open(&stderr_path).map_err(io)?;
        let mut cmd = aterm_uds::spawnfd::command(ty);
        cmd.arg("trace")
            .arg("validate")
            .arg("--spec")
            .arg(&spec_path)
            .arg("--config")
            .arg(&cfg_path)
            .arg("--cases")
            .arg("-")
            .stderr(stderr);
        let mut child = ty_child(&mut cmd).map_err(io)?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("ty (cases) {}: no stdio pipes", ty.display()));
        };
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead as _;
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            who: format!(
                "ty (cases) {} [{}] pid {}",
                ty.display(),
                ty_build_stamp(ty),
                child.id()
            ),
            child,
            stdin,
            lines,
            stderr: stderr_reader,
            next_id: 0,
        })
    }

    fn recv(&self, what: &str) -> Result<String, String> {
        use std::sync::mpsc::RecvTimeoutError;
        self.lines.recv_timeout(CASES_HANG).map_err(|e| match e {
            RecvTimeoutError::Timeout => format!("no {what} within {} s", CASES_HANG.as_secs()),
            RecvTimeoutError::Disconnected => format!("stdout closed before {what}"),
        })
    }

    #[cfg_attr(trust_verify, trust::skip)]
    fn expect_header(&self, model: &str, trace: &str) -> Result<(), String> {
        use aterm_json::Value;
        let line = self.recv("its header")?;
        let names = |doc: &Value| -> Option<Vec<String>> {
            let mut names = doc
                .get("variables")?
                .as_array()?
                .iter()
                .map(|n| n.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()?;
            names.sort();
            Some(names)
        };
        let want = aterm_json::from_str::<Value>(trace)
            .ok()
            .and_then(|t| names(&t));
        let fits = aterm_json::from_str::<Value>(&line).is_ok_and(|h| {
            h.get("ty_trace_cases").and_then(Value::as_u64) == Some(1)
                && h.get("module").and_then(Value::as_str) == Some(model)
                && h.get("action_label_mode").and_then(Value::as_str) == Some("error")
                && want.is_some()
                && names(&h) == want
        });
        if fits {
            Ok(())
        } else {
            Err(format!("a header that is not `{model}`'s: {line}"))
        }
    }

    #[cfg_attr(trust_verify, trust::skip)]
    fn case(&mut self, trace: &str) -> Result<CasesAnswer, String> {
        use aterm_json::Value;
        use std::io::Write as _;
        let id = self.next_id;
        self.next_id += 1;
        writeln!(self.stdin, "{{\"id\":{id},\"trace\":{trace}}}")
            .and_then(|()| self.stdin.flush())
            .map_err(|e| self.failure(&format!("request {id} could not be written: {e}")))?;
        let line = self
            .recv(&format!("the verdict for request {id}"))
            .map_err(|why| self.failure(&why))?;
        let verdict = aterm_json::from_str::<Value>(&line).ok();
        let field = |key: &str| verdict.as_ref().and_then(|v| v.get(key));
        let ok = field("ok").and_then(Value::as_bool);
        let class = field("class").and_then(Value::as_str);
        let (Some(ok), true) = (ok, field("id").and_then(Value::as_u64) == Some(id)) else {
            return Err(self.failure(&format!("not the verdict for request {id}: {line}")));
        };
        if !ok && !matches!(class, Some("refuted" | "error")) {
            return Err(self.failure(&format!("a refusal with no class: {line}")));
        }
        let text = |key: &str| field(key).and_then(Value::as_str).unwrap_or("?");
        let mut evidence = if ok {
            let steps = field("steps").and_then(Value::as_u64).unwrap_or(0);
            format!("{}\naccepted: {steps} steps", self.who)
        } else {
            let step = field("step")
                .and_then(Value::as_u64)
                .map_or_else(|| "null".to_string(), |s| s.to_string());
            format!(
                "{}\n{} at step {step}: {}",
                self.who,
                text("kind"),
                text("message")
            )
        };
        let warnings = field("warnings").and_then(Value::as_array);
        for warning in warnings.into_iter().flatten() {
            evidence.push_str(&format!("\nwarning: {}", warning.as_str().unwrap_or("?")));
        }
        Ok(CasesAnswer {
            ok,
            evidence,
            end: field("end").is_some(),
            unevaluated: class == Some("error"),
        })
    }

    #[cfg_attr(trust_verify, trust::skip)]
    fn failure(&mut self, why: &str) -> String {
        use std::io::Read as _;
        let _ = self.child.kill();
        let status = self
            .child
            .wait()
            .map_or_else(|e| e.to_string(), |s| s.to_string());
        let mut stderr = String::new();
        let _ = self.stderr.read_to_string(&mut stderr);
        let lines: Vec<&str> = stderr.lines().collect();
        let tail = lines[lines.len().saturating_sub(20)..].join("\n");
        format!(
            "{}: {why} ({status})\n--- ty stderr (tail) ---\n{tail}",
            self.who
        )
    }
}

impl Drop for CasesChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The two-step `ty trace validate` JSON for a derived model.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn transition_trace_json(
    m: &Model,
    prev: &BTreeMap<&'static str, i64>,
    next: &BTreeMap<&'static str, i64>,
    action: Option<&str>,
) -> String {
    let vars: Vec<&str> = m.vars.iter().map(|v| v.name).collect();
    let state_json = |s: &BTreeMap<&'static str, i64>| -> String {
        let fields: Vec<String> = vars
            .iter()
            .map(|v| {
                let Some(value) = s.get(v) else {
                    panic!(
                        "{}: the transition state {s:?} has no `{v}` — project every model \
                         variable",
                        m.name
                    )
                };
                format!("\"{v}\":{{\"type\":\"int\",\"value\":{value}}}")
            })
            .collect();
        format!("{{{}}}", fields.join(","))
    };
    let var_list: Vec<String> = vars.iter().map(|v| format!("\"{v}\"")).collect();
    let action_field = action
        .map(|a| format!(",\"action\":{{\"name\":\"{a}\"}}"))
        .unwrap_or_default();
    format!(
        "{{\"version\":\"1\",\"module\":\"{}\",\"variables\":[{}],\"steps\":[\
         {{\"index\":0,\"state\":{}}},\
         {{\"index\":1,\"state\":{}{action_field}}}\
         ]}}",
        m.name,
        var_list.join(","),
        state_json(prev),
        state_json(next),
    )
}

/// Per-invariant non-vacuity: the invariants of `m` that no `Buggy = 1` member falsifies when each is checked alone, in model order.
#[must_use]
pub fn uncaught_invariants(m: &Model) -> Vec<&'static str> {
    let violated = interp::violated_invariants(&interp::with_buggy(m, 1));
    m.invariants
        .iter()
        .map(|inv| inv.name)
        .filter(|name| !violated.contains(name))
        .collect()
}

/// What the non-vacuity sweep finds wrong with one model, one line per fault, over every configuration it declares (`configs[0]` the committed one, then each `scenario`).
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn sweep_faults(configs: &[Model]) -> Vec<String> {
    use crate::derive::InvariantKind;
    let Some(m) = configs.first() else {
        return Vec::new();
    };
    let mut faults = m.malformed_bounds();
    if m.is_function_valued() {
        return faults;
    }
    for (i, config) in configs.iter().enumerate() {
        for law in interp::violated_invariants(&interp::with_buggy(config, 0)) {
            faults.push(format!(
                "{}::{law}: broken with no mutant on, {}",
                m.name,
                if i == 0 {
                    "at the committed config".to_string()
                } else {
                    format!("in the scenario {:?}", config_overrides(m, config))
                }
            ));
        }
    }
    let mutants: Vec<Model> = configs
        .iter()
        .flat_map(|config| {
            m.mutant_dials()
                .into_iter()
                .map(|dial| interp::with_buggy(config, dial))
        })
        .collect();
    let caught: BTreeSet<&'static str> =
        crate::check::each(&mutants, interp::violated_within_budget)
            .into_iter()
            .flatten()
            .collect();
    for inv in &m.invariants {
        match (inv.kind, caught.contains(inv.name)) {
            (InvariantKind::Claim, false) => faults.push(format!(
                "{}::{}: no mutant breaks it — give it one, delete it if it restates another \
                 law or no defect can reach it, or declare it a `bound` if it states the space",
                m.name, inv.name
            )),
            (InvariantKind::Bound, true) => faults.push(format!(
                "{}::{}: declared `bound`, but a mutant breaks it — it is a caught design \
                 claim: declare it `invariant`",
                m.name, inv.name
            )),
            _ => {}
        }
    }
    if let Err(why) = audit_dead_negative_controls(configs) {
        faults.push(why);
    }
    faults
}

fn config_overrides(m: &Model, config: &Model) -> Vec<(&'static str, i64)> {
    config
        .consts
        .iter()
        .filter(|c| !m.consts.contains(c))
        .copied()
        .collect()
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
