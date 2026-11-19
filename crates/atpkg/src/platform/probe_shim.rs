// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Immutable producer snapshots for the two local probes. Nothing rendered here is a file.

use super::{MAX_SHIM_BYTES, sh_harness_prelude, sh_quote_str, sh_shim_body, sh_shim_quote};
use std::io;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 8;
const PARENT: &str = "__atpkg_probe_parent";

/// The prepared helper code that implements `aterm_uds::tether::VERB`.
/// Production callers obtain its executable from `aterm_uds::tether::prepared_executable`;
/// inert controls supply an owned data file instead. This renderer never starts it.
#[derive(Clone, Debug)]
pub struct ProbeForwarder {
    /// The prepared helper, which may be a retained code-matched copy. Its file
    /// identity is not the original frontend's loaded-image identity.
    pub executable: PathBuf,
    /// The process that will directly spawn the outer probe shell, not that shell's `$$`.
    pub parent_pid: u32,
}

/// An image the snapshot can start, or a store backing image whose mark protects a compat copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeImage {
    /// Its resolved physical path. Register this image before using the snapshot.
    pub physical: PathBuf,
    /// The logical spelling the original forward gave argv0 (the backing file for a compat copy).
    pub argv0: PathBuf,
    /// A helper (the bridge, shell or usage tap), rather than the main queried program.
    pub helper: bool,
}

/// Shell data and all images it can start. Run as `/bin/sh -c body original_wrapper ARG`,
/// where ARG is exactly `--help` or `--version`, after registering every supplied image.
/// A helper's signal can be hidden by shell policy: a main exit alone does not release helpers.
#[derive(Debug)]
pub struct ProbeShim {
    /// The captured policy, with every selectable executable bound to its admitted image.
    pub body: String,
    /// Physical main and helper images; wrapper text itself is never executed from a file.
    pub images: Vec<ProbeImage>,
}

/// Snapshot an exact current producer body (or its older direct-forward spelling).
/// Missing, incomplete, unknown or recursively unbounded inputs refuse. Policy order and
/// each argument remain the producer's; only filesystem-selected forwards become snapshots.
#[must_use]
pub fn probe_shim(wrapper: &Path, content: &str, forwarder: &ProbeForwarder) -> Option<ProbeShim> {
    if forwarder.parent_pid <= 1 || forwarder.parent_pid > i32::MAX.unsigned_abs() {
        return None;
    }
    let bridge = physical_file(&forwarder.executable)?;
    let shell = physical_file(Path::new("/bin/sh"))?;
    let mut capture = Capture {
        bridge,
        shell,
        images: Vec::new(),
    };
    capture.image(capture.bridge.clone(), capture.bridge.clone(), true);
    capture.image(capture.shell.clone(), PathBuf::from("/bin/sh"), true);
    let body = capture.body(wrapper, content, false, 0)?;
    Some(ProbeShim {
        body: format!("{PARENT}={}\n{body}", forwarder.parent_pid),
        images: capture.images,
    })
}

struct Capture {
    bridge: PathBuf,
    shell: PathBuf,
    images: Vec<ProbeImage>,
}

struct Forward {
    words: Vec<String>,
}

impl Forward {
    fn exec(&self, extra: &[&str]) -> String {
        let mut words = self.words.clone();
        words.extend(extra.iter().map(|word| sh_quote_str(word)));
        format!("exec {} \"$@\"", words.join(" "))
    }
}

impl Capture {
    fn image(&mut self, physical: PathBuf, argv0: PathBuf, helper: bool) {
        let image = ProbeImage {
            physical,
            argv0,
            helper,
        };
        if !self.images.contains(&image) {
            self.images.push(image);
        }
    }

    fn forward(&mut self, logical: &Path, helper: bool, depth: usize) -> Option<Forward> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let physical = physical_file(logical)?;
        let head = read_head(&physical)?;
        if head.starts_with(b"#!/bin/sh\n") {
            return self.script_forward(logical, std::str::from_utf8(&head).ok()?, helper, depth);
        }
        self.image(physical.clone(), logical.to_path_buf(), helper);
        self.forward_words(&physical, logical, Vec::new())
    }

    fn script_forward(
        &mut self,
        logical: &Path,
        text: &str,
        helper: bool,
        depth: usize,
    ) -> Option<Forward> {
        let nested = self.body(logical, text, helper, depth + 1)?;
        // Passing the selected parent as data keeps it stable across another shell exec.
        let nested = format!("{PARENT}=$1; shift\n{nested}");
        self.forward_words(
            &self.shell,
            Path::new("/bin/sh"),
            vec![
                sh_quote_str("-c"),
                sh_quote_str(&nested),
                quote_path(logical)?,
                format!("\"${PARENT}\""),
            ],
        )
    }

    fn forward_words(
        &self,
        program: &Path,
        argv0: &Path,
        mut tail: Vec<String>,
    ) -> Option<Forward> {
        let mut words = vec![
            quote_path(&self.bridge)?,
            sh_quote_str(aterm_uds::tether::VERB),
            format!("\"${PARENT}\""),
            quote_path(program)?,
            quote_path(argv0)?,
        ];
        words.append(&mut tail);
        Some(Forward { words })
    }

    fn body(
        &mut self,
        wrapper: &Path,
        content: &str,
        helper: bool,
        depth: usize,
    ) -> Option<String> {
        if depth >= MAX_DEPTH || content.len() > MAX_SHIM_BYTES || !wrapper.is_absolute() {
            return None;
        }
        let super::shim_producer::Producer {
            target,
            env,
            launch,
            route,
            atpkg,
            program,
            prefix,
            prelude,
            ..
        } = super::shim_producer::recognize(wrapper, content)?;

        // These snapshots accept only --help/--version. Both skip implicit posture,
        // so remove the unused exact-launch route rather than leave an unpinned helper
        // in a snapshot whose declared images are exhaustive.
        let mut pinned = if crate::native_defaults::Launcher::recorded(&prelude).is_some() {
            let mut ordinary = super::twin_prelude_with_launcher(
                &program,
                &prefix,
                atpkg.as_deref(),
                crate::selfupdate::verbs_of(&program),
                &crate::yolo::policy_of(&program),
                None,
            );
            ordinary.push_str(&crate::launch_tap::sh_prelude(&program, atpkg.as_deref()));
            ordinary
        } else {
            prelude.clone()
        };
        let harness = sh_harness_prelude(&program, &prefix);
        if !harness.is_empty() && pinned.contains(&harness) {
            let replacement = self.harness(&program, &prefix, helper, depth)?;
            pinned = pinned.replacen(&harness, &replacement, 1);
        }
        let tap = crate::launch_tap::sh_prelude(&program, atpkg.as_deref());
        if !tap.is_empty() && pinned.contains(&tap) {
            let replacement = self.tap(&program, atpkg.as_deref(), depth)?;
            pinned = pinned.replacen(&tap, &replacement, 1);
        }

        // Render the same prefix, including policy-before-env order and build-cache policy.
        let ordinary = sh_shim_body(&target, &env, None, None, &pinned);
        let final_line = format!("exec {} \"$@\"\n", sh_shim_quote(&target));
        let mut body = ordinary.strip_suffix(final_line.as_str())?.to_string();
        if let Some(launch) = launch {
            if guarded_launch(&launch)? {
                body.push_str(&self.forward(&launch, helper, depth)?.exec(&[]));
                body.push('\n');
                return Some(body);
            }
            body.push_str(
                "export ATPKG_MANAGED_LAUNCH_PID=\"$$\"\nexport ATPKG_MANAGED_LAUNCH_PATH=",
            );
            body.push_str(&quote_path(&launch)?);
            body.push_str("\nexport ATPKG_MANAGED_LAUNCH_TARGET=");
            body.push_str(&quote_path(&target)?);
            body.push('\n');
        }
        if let Some(route) = route {
            let marker = route
                .parent()
                .and_then(Path::parent)
                .map_or_else(|| route.clone(), crate::compat::root_marker);
            if !is_link(&route)?
                && executable(&route)?
                && regular(&target)?
                && !is_link(&marker)?
                && regular(&marker)?
            {
                // Store discard also retires this compat root. Its ordinary store mark must
                // protect that retirement, in addition to the actual copy's physical identity.
                self.image(physical_file(&target)?, target.clone(), helper);
                body.push_str(&self.forward(&route, helper, depth)?.exec(&[]));
                body.push('\n');
                return Some(body);
            }
        }
        body.push_str(&self.forward(&target, helper, depth)?.exec(&[]));
        body.push('\n');
        Some(body)
    }

    fn harness(
        &mut self,
        program: &str,
        prefix: &Path,
        helper: bool,
        depth: usize,
    ) -> Option<String> {
        let layout = crate::store::Layout {
            prefix: prefix.to_path_buf(),
        };
        let Some((harness, tool)) = crate::harness::twin_harness(&layout, program) else {
            return Some(String::new());
        };
        let forward = match harness_handoff(&layout, &harness, &tool)? {
            Handoff::Vendor => return Some(String::new()),
            Handoff::Link(shim) => self.forward(&shim, helper, depth)?,
            // Select and bind the same captured regular body, never a second read that a
            // normal publication could replace after the live predicate judged it.
            Handoff::Script { shim, text } => self.script_forward(&shim, &text, helper, depth)?,
        };
        Some(format!(
            "if [ \"${{ATERM_NO_HARNESS-}}\" != 1 ]; then\n  {}\nfi\n",
            forward.exec(&["run", "--"])
        ))
    }

    fn tap(&mut self, program: &str, atpkg: Option<&Path>, depth: usize) -> Option<String> {
        if !crate::launch_tap::of(program) {
            return Some(String::new());
        }
        let helper = crate::launch_tap::aterm_beside(atpkg?)?;
        if !executable(&helper)? {
            return Some(String::new());
        }
        let forward = self.forward(&helper, true, depth)?;
        // In a command substitution $$ remains the outer shell's PID. Explicit exec replaces
        // its direct subshell, so the helper bridge's parent is that shell, not the Agent.
        Some(format!(
            "if [ -n \"${{ATERM_PARENT_SESSION_ID-}}\" ] && [ -z \"${{ATERM_NO_HARNESS-}}\" ]; then\n  __atpkg_tap=$({PARENT}=$$; {} </dev/null 2>/dev/null) || __atpkg_tap=\n  if [ -n \"$__atpkg_tap\" ]; then set -- --settings \"$__atpkg_tap\" \"$@\"; fi\nfi\n",
            forward.exec(&crate::launch_tap::VERB)
        ))
    }
}

/// What a twin's harness hand-off block ([`super::sh_harness_prelude`]) finds at
/// `<prefix>/bin/<tool>` when it runs: the one reading of that block's runtime arm, which the
/// probe snapshot binds and the native projection checks
/// ([`super::native_claude_restart::projection_rule`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Handoff {
    /// No live harness: the block falls through and the vendor runs as typed.
    Vendor,
    /// `bin/<tool>` is a link to an executable file, which the block execs.
    Link(PathBuf),
    /// `bin/<tool>` is a script whose last `exec '<target>' "$@"` line names an executable file
    /// inside a ready store build of the harness (`store/<harness>/<digits>/` with
    /// `<digits>.ready`) or, while the harness is dev-linked, anywhere: the block execs it, and
    /// `text` is the body this one read found.
    Script { shim: PathBuf, text: String },
}

/// [`Handoff`] for the harness `harness`, whose tool is `tool`, in `layout`, exactly as the block's
/// shell reads it: `bin/<tool>` an executable regular file (through a link), then either the link
/// itself or the target of its last exec line, refused for a quote, outside a ready store build
/// unless the harness's link marker exists, and unless it is itself an executable file. A
/// tombstone (no exec line), a removed or unfinished build and a missing target are
/// [`Handoff::Vendor`]. `None` where a fact it rests on was not read.
pub(crate) fn harness_handoff(
    layout: &crate::store::Layout,
    harness: &str,
    tool: &str,
) -> Option<Handoff> {
    let shim = layout.prefix.join("bin").join(tool);
    if !executable(&shim)? {
        return Some(Handoff::Vendor);
    }
    if is_link(&shim)? {
        return Some(Handoff::Link(shim));
    }
    let text = crate::metadata_io::read_bounded_regular_utf8(&shim, MAX_SHIM_BYTES).ok()?;
    // The shell's `read` keeps a carriage return, and its last matching line wins.
    let raw = text
        .split('\n')
        .filter_map(|line| line.strip_prefix("exec '")?.strip_suffix("' \"$@\""))
        .next_back()
        .unwrap_or("");
    if raw.is_empty() || raw.contains('\'') {
        return Some(Handoff::Vendor);
    }
    let store = layout.program_current(harness).parent()?.to_path_buf();
    let own_store = format!("{}/", store.to_str()?);
    let live = if let Some(rest) = raw.strip_prefix(own_store.as_str()) {
        let build = rest.split('/').next()?;
        !build.is_empty()
            && build.bytes().all(|byte| byte.is_ascii_digit())
            && regular(&store.join(format!("{build}.ready")))?
    } else {
        regular(&layout.link_marker(harness))?
    };
    if !live || !executable(Path::new(raw))? {
        return Some(Handoff::Vendor);
    }
    Some(Handoff::Script { shim, text })
}

fn quote_path(path: &Path) -> Option<String> {
    Some(sh_quote_str(path.to_str()?))
}

fn physical_file(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let physical = std::fs::canonicalize(path).ok()?;
    physical.to_str()?;
    regular(&physical)?.then_some(physical)
}

fn read_head(path: &Path) -> Option<Vec<u8>> {
    let (file, meta) = crate::metadata_io::open_regular_with_meta(path).ok()?;
    let max = u64::try_from(MAX_SHIM_BYTES + 1).ok()?;
    let mut bytes =
        Vec::with_capacity(usize::try_from(meta.len().min(max).saturating_add(1)).unwrap_or(0));
    file.take(max).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn regular(path: &Path) -> Option<bool> {
    match std::fs::metadata(path) {
        Ok(meta) => Some(meta.is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// The producer's `-L` and `-x` guard, including an unknown filesystem observation.
pub(super) fn guarded_launch(path: &Path) -> Option<bool> {
    Some(is_link(path)? && execute_access(path)?)
}

fn is_link(path: &Path) -> Option<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Some(meta.file_type().is_symlink()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// Kernel execute access to a regular file, following links; unread evidence or differing
/// real and effective credentials leave the answer unknown.
pub(crate) fn executable(path: &Path) -> Option<bool> {
    Some(regular(path)? && execute_access(path)?)
}

// The shell's `-x` also accepts search access to a directory. The guarded branch
// then attempts to exec that directory and fails, without running the fallback.
fn execute_access(path: &Path) -> Option<bool> {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: the owned NUL-terminated path remains live; access only reads it. Probes run
    // without set-id credentials, so the shell's effective-user -x and access name one user.
    if unsafe { libc::getuid() != libc::geteuid() || libc::getgid() != libc::getegid() } {
        return None;
    }
    // SAFETY: as above; no pointer is retained.
    if unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 } {
        return Some(true);
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::EACCES | libc::ENOENT | libc::ENOTDIR) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
#[path = "probe_shim_tests.rs"]
mod tests;
