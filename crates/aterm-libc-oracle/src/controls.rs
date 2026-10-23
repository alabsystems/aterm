// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Negative controls planted in a copy of the shim for oracle verification.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::Path;

use crate::cargo::{Build, DocTarget, Outcome, Workspace};
use crate::cells::{self, Cell, Os};
use crate::rustdoc::Doc;
use crate::shape::{self, ShapeDiff, ShapeRecord, Side};
use crate::siblings::{self, SiblingFinding};
use crate::symbols::{self, SymbolDiff};
use crate::trust::Trust;
use crate::verdict::CouldNotRun;
use aterm_json::Value;

/// One planted defect class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlId {
    /// An architecture-only edit to one family's hand-written bodies.
    SiblingBody { os: Os },
    /// The unmutated copy, which must pass for any mutation to mean anything.
    Baseline,
    /// `c_int` withdrawn on a cell this box does not host: this cell stays green.
    ForeignPrimitive,
    /// `c_int` withdrawn on this cell.
    NativePrimitive,
    /// A nominal alias pointee widened to `c_void` (the Darwin cells have one).
    NominalAlias,
    /// An extern declaration replaced by an inline body of the same signature.
    ExternToInline,
    /// An extra public inherent method.
    ExtraInherentMethod,
    /// An `unsafe` method widened to safe.
    HeaderSafety,
    /// `Clone`/`Copy` dropped from a struct.
    TraitSurface,
    /// An extern declaration bound to a symbol the reference does not bind.
    LinkName,
    /// FD_SET and FD_ISSET both shifted to the wrong bit.
    FdMapping,
    /// A public method made private.
    MethodSet,
    /// A method's return type changed.
    MethodSignature,
    /// A method reading the wrong payload offset.
    MethodBehaviour,
    /// A C-macro body changed.
    MacroBehaviour,
}

impl ControlId {
    #[must_use]
    pub fn name(self) -> String {
        match self {
            Self::SiblingBody { os } => format!("sibling body ({})", os.label()),
            Self::Baseline => "baseline".into(),
            Self::ForeignPrimitive => "foreign primitive re-export".into(),
            Self::NativePrimitive => "native primitive re-export".into(),
            Self::NominalAlias => "nominal alias".into(),
            Self::ExternToInline => "extern to inline body".into(),
            Self::ExtraInherentMethod => "extra inherent method".into(),
            Self::HeaderSafety => "function-header safety".into(),
            Self::TraitSurface => "trait surface".into(),
            Self::LinkName => "linker symbol".into(),
            Self::FdMapping => "correlated FD mapping".into(),
            Self::MethodSet => "method set".into(),
            Self::MethodSignature => "method signature".into(),
            Self::MethodBehaviour => "method behaviour".into(),
            Self::MacroBehaviour => "macro behaviour".into(),
        }
    }
}

/// How a control failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlFailure {
    /// The unmutated subject already shows the defect, so planting it proves nothing.
    Baseline { detail: String },
    /// The text the mutation replaces is not in the shim `expected` times: the shim was regenerated
    /// and the control must follow it.
    Anchor {
        file: String,
        anchor: String,
        found: usize,
        expected: usize,
    },
    /// The oracle stayed green with the defect planted.
    Escaped,
    /// The oracle went red, but without the evidence the defect must produce.
    Unattributed { expected: String, detail: String },
    /// A defect planted for another cell turned this one red.
    Spurious { detail: String },
}

impl fmt::Display for ControlFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Baseline { detail } => {
                write!(f, "the unmutated subject did not pass:\n{detail}")
            }
            Self::Anchor {
                file,
                anchor,
                found,
                expected,
            } => write!(
                f,
                "{file} carries the mutation's source {found} times, not {expected} (update the \
                 control with the shim): {:?}",
                anchor.lines().next().unwrap_or_default()
            ),
            Self::Escaped => f.write_str("the oracle stayed green with the defect planted"),
            Self::Unattributed { expected, detail } => {
                write!(f, "the oracle went red without {expected}:\n{detail}")
            }
            Self::Spurious { detail } => {
                write!(
                    f,
                    "a defect planted for another cell broke this one:\n{detail}"
                )
            }
        }
    }
}

/// What every control decided.
#[derive(Clone, Debug, Default)]
pub struct ControlsReport {
    pub caught: Vec<ControlId>,
    /// Controls this cell has no subject for (decided on the other family).
    pub not_on_this_cell: Vec<ControlId>,
    pub failed: Vec<(ControlId, ControlFailure)>,
}

impl ControlsReport {
    fn record(&mut self, id: ControlId, verdict: Result<(), ControlFailure>) {
        match verdict {
            Ok(()) => self.caught.push(id),
            Err(failure) => self.failed.push((id, failure)),
        }
    }
}

/// The evidence a planted build must produce.
#[derive(Clone, Copy, Debug)]
enum Expect {
    /// The build passes.
    Green,
    /// The build fails with error `code` naming `names`.
    Error {
        code: &'static str,
        names: &'static str,
    },
    /// The runtime test whose function is this fails.
    TestFails(&'static str),
}

impl Expect {
    fn describe(self) -> String {
        match self {
            Self::Green => "a passing build".into(),
            Self::Error { code, names } => format!("error {code} naming `{names}`"),
            Self::TestFails(test) => format!("a failed `{test}`"),
        }
    }

    fn judge(self, outcome: &Outcome) -> Result<(), ControlFailure> {
        match self {
            Self::Green if outcome.success => Ok(()),
            Self::Green => Err(ControlFailure::Spurious {
                detail: outcome.detail(),
            }),
            _ if outcome.success => Err(ControlFailure::Escaped),
            Self::Error { code, names } if outcome.has_error(code, names) => Ok(()),
            Self::TestFails(test) if outcome.test_failed(test) => Ok(()),
            _ => Err(ControlFailure::Unattributed {
                expected: self.describe(),
                detail: outcome.detail(),
            }),
        }
    }
}

/// One edit of a source file.
#[derive(Clone, Debug)]
enum Edit {
    /// Replace `anchor`, which must occur exactly `count` times.
    Replace {
        anchor: String,
        with: String,
        count: usize,
    },
    Append(&'static str),
}

impl Edit {
    fn once(anchor: &str, with: &str) -> Self {
        Self::Replace {
            anchor: anchor.into(),
            with: with.into(),
            count: 1,
        }
    }
}

fn apply(file: &str, text: &str, edits: &[Edit]) -> Result<String, ControlFailure> {
    let mut out = text.to_string();
    for edit in edits {
        match edit {
            Edit::Replace {
                anchor,
                with,
                count,
            } => {
                let found = out.matches(anchor.as_str()).count();
                if found != *count {
                    return Err(ControlFailure::Anchor {
                        file: file.into(),
                        anchor: anchor.clone(),
                        found,
                        expected: *count,
                    });
                }
                out = out.replace(anchor.as_str(), with);
            }
            Edit::Append(tail) => out.push_str(tail),
        }
    }
    Ok(out)
}

struct Fixture {
    alias: Option<(&'static str, &'static str, Expect)>,
    signature: (&'static str, &'static str),
    body: (&'static str, &'static str),
    mac: (&'static str, &'static str),
}

fn fixture(os: Os) -> Fixture {
    match os {
        Os::Darwin => Fixture {
            alias: Some((
                "pub type vm_statistics64_t = *mut vm_statistics64;",
                "pub type vm_statistics64_t = *mut c_void;",
                Expect::Error {
                    code: "E0308",
                    names: "vm_statistics64",
                },
            )),
            signature: (
                "    pub unsafe fn si_pid(&self) -> pid_t {\n        self.si_pid\n    }",
                "    pub unsafe fn si_pid(&self) -> c_long {\n        self.si_pid as c_long\n    }",
            ),
            body: (
                "        unsafe { (*(self as *const siginfo_t).cast::<siginfo_timer>()).si_value }",
                "        sigval { sival_ptr: self.si_addr }",
            ),
            mac: (
                "pub const extern \"C\" fn WIFEXITED(status: c_int) -> bool {\n    (status & 0o177) == 0\n}",
                "pub const extern \"C\" fn WIFEXITED(status: c_int) -> bool {\n    (status & 0o177) == 1\n}",
            ),
        },
        Os::GnuLinux => Fixture {
            alias: None,
            signature: (
                "    pub unsafe fn si_pid(&self) -> pid_t {\n        unsafe { self.sigchld().si_pid }\n    }",
                "    pub unsafe fn si_pid(&self) -> c_long {\n        unsafe { self.sigchld().si_pid as c_long }\n    }",
            ),
            body: (
                "        unsafe { (*(self as *const siginfo_t).cast::<siginfo_timer>()).si_sigval }",
                "        sigval { sival_ptr: unsafe { self.si_addr() } }",
            ),
            mac: (
                "pub const extern \"C\" fn WIFEXITED(status: c_int) -> bool {\n    (status & 0x7f) == 0\n}",
                "pub const extern \"C\" fn WIFEXITED(status: c_int) -> bool {\n    (status & 0x7f) == 1\n}",
            ),
        },
    }
}

const FD_SET_GOOD: &str = "unsafe { (*set).__opaque[fd / bits] |= 1 << (fd % bits) };";
const FD_SET_BAD: &str = "unsafe { (*set).__opaque[fd / bits] |= 1 << ((fd + 1) % bits) };";

const PRIMITIVES: &str = "pub use core::ffi::{\n    c_char, c_double, c_float, c_int, c_long, c_longlong, c_schar, c_short, c_uchar, c_uint,\n    c_ulong, c_ulonglong, c_ushort, c_void,\n};";

const FOREIGN_CFG: &str =
    r#"all(target_os = "windows", target_env = "msvc", target_arch = "x86_64")"#;

fn without_c_int_on(cfg: &str) -> String {
    format!(
        "{}\n\n#[cfg(not({cfg}))]\npub use core::ffi::c_int;",
        PRIMITIVES.replace("c_float, c_int, c_long", "c_float, c_long")
    )
}

const STRLEN_DECL: &str = "    pub fn strlen(cs: *const c_char) -> size_t;";
const GETPID_DECL: &str = "    pub fn getpid() -> pid_t;";
const PLANTED_SYMBOL: &str = "getpid$PLANTED";

const INLINE_TAIL: &str = "\n#[inline]\npub unsafe extern \"C\" fn strlen(_cs: *const c_char) -> size_t { 0 }\n\nimpl siginfo_t {\n    pub fn aterm_oracle_extra_method(&self) {}\n}\n";

const TIMESPEC: &str = "#[repr(C)]\n#[derive(Clone, Copy, Debug)]\n#[cfg_attr(feature = \"extra_traits\", derive(PartialEq, Eq, Hash))]\npub struct timespec {";

fn io_failure(what: &str) -> impl Fn(io::Error) -> CouldNotRun + '_ {
    move |error| {
        CouldNotRun::new(
            format!("{what}: {error}"),
            "check that the checkout is readable and $TMPDIR has space, then rerun",
        )
    }
}

fn sibling_controls(sources: &BTreeMap<&'static str, String>, report: &mut ControlsReport) {
    let unplanted = siblings::compare(sources);
    for (os, _, right) in cells::sibling_pairs() {
        let already: Vec<String> = unplanted
            .iter()
            .filter(|finding| finding.os() == Some(os))
            .map(ToString::to_string)
            .collect();
        if !already.is_empty() {
            let detail = already.join("\n");
            report.record(
                ControlId::SiblingBody { os },
                Err(ControlFailure::Baseline { detail }),
            );
            continue;
        }
        let verdict = apply(
            right.source,
            &sources[right.source],
            &[Edit::once(FD_SET_GOOD, FD_SET_BAD)],
        )
        .and_then(|mutated| {
            let mut planted = sources.clone();
            planted.insert(right.source, mutated);
            let caught = siblings::compare(&planted)
                .iter()
                .any(|f| matches!(f, SiblingFinding::Differs { os: o, .. } if *o == os));
            if caught {
                Ok(())
            } else {
                Err(ControlFailure::Escaped)
            }
        });
        report.record(ControlId::SiblingBody { os }, verdict);
    }
}

fn copy_oracle(root: &Path, dest: &Path) -> io::Result<()> {
    let oracle = root.join("libc-oracle");
    let into = dest.join("libc-oracle");
    std::fs::create_dir_all(into.join(".cargo"))?;
    std::fs::create_dir_all(dest.join(".cargo"))?;
    for file in ["Cargo.toml", "Cargo.lock"] {
        std::fs::copy(oracle.join(file), into.join(file))?;
    }
    for (config, to) in [
        (
            oracle.join(".cargo/config.toml"),
            into.join(".cargo/config.toml"),
        ),
        (
            root.join(".cargo/config.toml"),
            dest.join(".cargo/config.toml"),
        ),
    ] {
        if config.is_file() {
            std::fs::copy(config, to)?;
        }
    }
    copy_dir(&oracle.join("conformance"), &into.join("conformance"))?;
    copy_dir(&oracle.join("under-test"), &into.join("under-test"))?;
    copy_dir(
        &root.join("crates/aterm-libc/src"),
        &dest.join("crates/aterm-libc/src"),
    )
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir(&entry.path(), &to.join(entry.file_name()))?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

struct Scratch<'t> {
    ws: Workspace<'t>,
    src: std::path::PathBuf,
    cell: &'static Cell,
}

impl Scratch<'_> {
    fn planted<T>(
        &self,
        file: &str,
        edits: &[Edit],
        then: impl FnOnce(&Workspace<'_>) -> io::Result<T>,
    ) -> Result<Result<T, ControlFailure>, CouldNotRun> {
        let path = self.src.join(file);
        let original = std::fs::read_to_string(&path).map_err(io_failure("reading the copy"))?;
        let mutated = match apply(file, &original, edits) {
            Ok(mutated) => mutated,
            Err(failure) => return Ok(Err(failure)),
        };
        std::fs::write(&path, mutated).map_err(io_failure("planting a defect in the copy"))?;
        let result = then(&self.ws);
        std::fs::write(&path, original).map_err(io_failure("restoring the copy"))?;
        result
            .map(Ok)
            .map_err(io_failure("running targo on the copy"))
    }

    fn build(
        &self,
        file: &str,
        edits: &[Edit],
        build: Build,
        expect: Expect,
    ) -> Result<Result<(), ControlFailure>, CouldNotRun> {
        Ok(self
            .planted(file, edits, |ws| ws.build(build))?
            .and_then(|outcome| expect.judge(&outcome)))
    }

    fn surface(
        &self,
        edits: &[Edit],
        reference: &Reference,
    ) -> Result<Result<Surface, ControlFailure>, CouldNotRun> {
        let documented = self.planted(self.cell.source, edits, |ws| {
            let (outcome, path) = ws.rustdoc(DocTarget::Shim)?;
            Ok(if outcome.success {
                Ok(std::fs::read_to_string(path)?)
            } else {
                Err(outcome)
            })
        })?;
        let text = match documented {
            Ok(Ok(text)) => text,
            Ok(Err(outcome)) => {
                return Ok(Err(ControlFailure::Unattributed {
                    expected: "a documented shim".into(),
                    detail: outcome.detail(),
                }));
            }
            Err(failure) => return Ok(Err(failure)),
        };
        let unreadable = |e: crate::rustdoc::FormatError| CouldNotRun::bare(e.to_string());
        let doc = Doc::parse(&text).map_err(unreadable)?;
        let public = doc.public().map_err(unreadable)?;
        let actual = shape::public_shape(&doc, &public, None).map_err(unreadable)?;
        let bound = symbols::extern_symbols(&public).map_err(unreadable)?;
        Ok(Ok(Surface {
            shape: shape::diff(&reference.shape, &actual),
            symbols: symbols::diff(&reference.symbols, &bound),
        }))
    }
}

/// Reference shape and extern symbols for a cell.
pub struct Reference {
    pub shape: BTreeSet<ShapeRecord>,
    /// Extern name -> the linker symbol it binds.
    pub symbols: BTreeMap<String, String>,
}

struct Surface {
    shape: Vec<ShapeDiff>,
    symbols: Vec<SymbolDiff>,
}

impl Surface {
    fn shape_diffs(&self) -> &[ShapeDiff] {
        &self.shape
    }

    fn symbol_diffs(&self) -> &[SymbolDiff] {
        &self.symbols
    }
}

fn has(diffs: &[ShapeDiff], side: Side, record: &ShapeRecord) -> bool {
    diffs.iter().any(|d| d.side == side && &d.record == record)
}

fn si_pid_header(diffs: &[ShapeDiff], side: Side, is_unsafe: bool) -> bool {
    diffs.iter().any(|d| {
        d.side == side
            && matches!(&d.record, ShapeRecord::InherentHeader { owner, name, header }
                if owner == "siginfo_t" && name == "si_pid"
                    && aterm_json::from_str::<Value>(header).ok()
                        .and_then(|h| h.get("is_unsafe")?.as_bool()) == Some(is_unsafe))
    })
}

fn diff_verdict<D: fmt::Display>(
    diffs: Result<&[D], &ControlFailure>,
    what: &str,
    caught: impl Fn(&[D]) -> bool,
) -> Result<(), ControlFailure> {
    match diffs {
        Err(failure) => Err(failure.clone()),
        Ok(diffs) if caught(diffs) => Ok(()),
        Ok([]) => Err(ControlFailure::Escaped),
        Ok(diffs) => Err(ControlFailure::Unattributed {
            expected: what.into(),
            detail: diffs
                .iter()
                .take(10)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        }),
    }
}

fn body(name: &str, has_body: bool) -> ShapeRecord {
    ShapeRecord::Body {
        name: name.into(),
        has_body,
    }
}

fn derived(path: &str) -> ShapeRecord {
    ShapeRecord::Trait {
        owner: "timespec".into(),
        kind: "struct".into(),
        path: path.into(),
        args: None,
        negative: false,
    }
}

/// Run every control for `cell` against a copy of the checkout at `root`.
/// # Errors
/// When the copy cannot be made or targo cannot be started.
pub fn run(
    trust: &Trust,
    root: &Path,
    cell: &'static Cell,
    reference: &Reference,
) -> Result<ControlsReport, CouldNotRun> {
    let mut report = ControlsReport::default();
    let sources = siblings::read_sources(&root.join("crates/aterm-libc/src"))
        .map_err(io_failure("reading the shim's cell sources"))?;
    sibling_controls(&sources, &mut report);

    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-libc-oracle-")
        .tempdir()
        .map_err(io_failure("creating the controls' scratch directory"))?;
    copy_oracle(root, scratch.path()).map_err(io_failure("copying the oracle"))?;
    let copy = Scratch {
        ws: Workspace::new(
            trust,
            &scratch.path().join("libc-oracle"),
            &scratch.path().join("target"),
        ),
        src: scratch.path().join("crates/aterm-libc/src"),
        cell,
    };

    let baseline = copy
        .ws
        .build(Build::Test)
        .map_err(io_failure("running targo on the copy"))?;
    if !baseline.success {
        report.record(
            ControlId::Baseline,
            Err(ControlFailure::Baseline {
                detail: baseline.detail(),
            }),
        );
        return Ok(report);
    }
    report.record(ControlId::Baseline, Ok(()));

    let fixture = fixture(cell.os);
    let source = cell.source;

    for (id, cfg, expect) in [
        (
            ControlId::ForeignPrimitive,
            FOREIGN_CFG.to_string(),
            Expect::Green,
        ),
        (
            ControlId::NativePrimitive,
            cell.cfg(),
            Expect::Error {
                code: "E0425",
                names: "c_int",
            },
        ),
    ] {
        let edit = Edit::once(PRIMITIVES, &without_c_int_on(&cfg));
        report.record(id, copy.build("lib.rs", &[edit], Build::Check, expect)?);
    }

    match fixture.alias {
        Some((good, bad, expect)) => {
            let verdict = copy.build(source, &[Edit::once(good, bad)], Build::Check, expect)?;
            report.record(ControlId::NominalAlias, verdict);
        }
        None => report.not_on_this_cell.push(ControlId::NominalAlias),
    }

    let (signature, _) = fixture.signature;
    let inline = copy.surface(
        &[
            Edit::once(STRLEN_DECL, ""),
            Edit::once(
                signature,
                &signature.replacen("pub unsafe fn si_pid", "pub fn si_pid", 1),
            ),
            Edit::once(
                GETPID_DECL,
                &format!("    #[link_name = \"{PLANTED_SYMBOL}\"]\n{GETPID_DECL}"),
            ),
            Edit::Append(INLINE_TAIL),
        ],
        reference,
    )?;
    let inline = inline.as_ref();
    report.record(
        ControlId::ExternToInline,
        diff_verdict(
            inline.map(Surface::shape_diffs),
            "strlen's extern declaration replaced by a body",
            |d| {
                has(d, Side::Missing, &body("strlen", false))
                    && has(d, Side::Unexpected, &body("strlen", true))
            },
        ),
    );
    report.record(
        ControlId::ExtraInherentMethod,
        diff_verdict(
            inline.map(Surface::shape_diffs),
            "an unexpected siginfo_t::aterm_oracle_extra_method",
            |d| {
                let extra = ShapeRecord::Inherent {
                    owner: "siginfo_t".into(),
                    name: "aterm_oracle_extra_method".into(),
                    kind: "function".into(),
                };
                has(d, Side::Unexpected, &extra)
            },
        ),
    );
    report.record(
        ControlId::HeaderSafety,
        diff_verdict(
            inline.map(Surface::shape_diffs),
            "si_pid's unsafe header replaced by a safe one",
            |d| si_pid_header(d, Side::Missing, true) && si_pid_header(d, Side::Unexpected, false),
        ),
    );
    report.record(
        ControlId::LinkName,
        diff_verdict(
            inline.map(Surface::symbol_diffs),
            &format!("getpid bound to `{PLANTED_SYMBOL}`"),
            |d| {
                d.iter().any(|diff| {
                    matches!(diff, SymbolDiff::Mismatch { name, shim, .. }
                        if name == "getpid" && shim == PLANTED_SYMBOL)
                })
            },
        ),
    );

    let traits = copy.surface(
        &[Edit::once(
            TIMESPEC,
            &TIMESPEC.replace("#[derive(Clone, Copy, Debug)]", "#[derive(Debug)]"),
        )],
        reference,
    )?;
    report.record(
        ControlId::TraitSurface,
        diff_verdict(
            traits.as_ref().map(Surface::shape_diffs),
            "timespec's Clone and Copy missing",
            |d| {
                has(d, Side::Missing, &derived("core::clone::Clone"))
                    && has(d, Side::Missing, &derived("core::marker::Copy"))
            },
        ),
    );

    let fd_mapping = Edit::Replace {
        anchor: "    let fd = fd as usize;".into(),
        with: "    let fd = (fd as usize) ^ 1;".into(),
        count: 2,
    };
    let (body_good, body_bad) = fixture.body;
    let (mac_good, mac_bad) = fixture.mac;
    let (sig_good, sig_bad) = fixture.signature;
    for (id, edit, build, expect) in [
        (
            ControlId::FdMapping,
            fd_mapping,
            Build::Test,
            Expect::TestFails("behaviour_fd_set"),
        ),
        (
            ControlId::MethodSet,
            Edit::once(
                "    pub unsafe fn si_status(&self) -> c_int {",
                "    unsafe fn si_status(&self) -> c_int {",
            ),
            Build::Check,
            Expect::Error {
                code: "E0624",
                names: "si_status",
            },
        ),
        (
            ControlId::MethodSignature,
            Edit::once(sig_good, sig_bad),
            Build::Check,
            Expect::Error {
                code: "E0308",
                names: "si_pid",
            },
        ),
        (
            ControlId::MethodBehaviour,
            Edit::once(body_good, body_bad),
            Build::Test,
            Expect::TestFails("behaviour_siginfo_si_value"),
        ),
        (
            ControlId::MacroBehaviour,
            Edit::once(mac_good, mac_bad),
            Build::Check,
            Expect::Error {
                code: "E0080",
                names: "WIFEXITED",
            },
        ),
    ] {
        report.record(id, copy.build(source, &[edit], build, expect)?);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cargo::{Diagnostic, FailedTest};

    fn error(code: &str, message: &str) -> Diagnostic {
        Diagnostic {
            code: Some(code.into()),
            message: message.into(),
            labels: Vec::new(),
            snippets: Vec::new(),
            spans: Vec::new(),
            rendered: format!("error[{code}]: {message}"),
        }
    }

    fn failed(outcome_errors: Vec<Diagnostic>, tests: &[&str]) -> Outcome {
        Outcome {
            success: false,
            code: Some(101),
            errors: outcome_errors,
            failed: tests
                .iter()
                .map(|t| FailedTest {
                    name: format!("cell_mac::runtime::{t}"),
                    stdout: String::new(),
                })
                .collect(),
            ..Outcome::default()
        }
    }

    #[test]
    fn a_control_needs_its_own_evidence_not_just_a_red_build() {
        let expect = Expect::Error {
            code: "E0624",
            names: "si_status",
        };
        let right = failed(vec![error("E0624", "method `si_status` is private")], &[]);
        assert_eq!(expect.judge(&right), Ok(()));
        let unrelated = failed(vec![error("E0433", "failed to resolve: `libc_shim`")], &[]);
        assert!(matches!(
            expect.judge(&unrelated),
            Err(ControlFailure::Unattributed { .. })
        ));
        let green = Outcome {
            success: true,
            ..Outcome::default()
        };
        assert_eq!(expect.judge(&green), Err(ControlFailure::Escaped));
    }

    #[test]
    fn a_test_control_names_its_test_and_a_green_control_must_pass() {
        let fd = Expect::TestFails("behaviour_fd_set");
        assert_eq!(
            fd.judge(&failed(vec![], &["behaviour_fd_isset", "behaviour_fd_set"])),
            Ok(())
        );
        assert!(fd.judge(&failed(vec![], &["behaviour_fd_isset"])).is_err());
        let green = Expect::Green;
        assert!(matches!(
            green.judge(&failed(vec![error("E0425", "c_int")], &[])),
            Err(ControlFailure::Spurious { .. })
        ));
    }

    #[test]
    fn an_anchor_must_occur_exactly_as_often_as_the_control_says() {
        let text = "a\nlet fd = fd as usize;\nb\nlet fd = fd as usize;\n";
        let twice = Edit::Replace {
            anchor: "let fd = fd as usize;".into(),
            with: "x".into(),
            count: 2,
        };
        assert_eq!(apply("f.rs", text, &[twice]).as_deref(), Ok("a\nx\nb\nx\n"));
        assert_eq!(
            apply("f.rs", text, &[Edit::once("let fd = fd as usize;", "x")]),
            Err(ControlFailure::Anchor {
                file: "f.rs".into(),
                anchor: "let fd = fd as usize;".into(),
                found: 2,
                expected: 1,
            })
        );
        assert_eq!(
            apply("f.rs", "x", &[Edit::Append("\ny")]).as_deref(),
            Ok("x\ny")
        );
    }

    #[test]
    fn an_si_pid_header_is_matched_by_its_safety_field() {
        let header = |side, unsafe_| ShapeDiff {
            side,
            record: ShapeRecord::InherentHeader {
                owner: "siginfo_t".into(),
                name: "si_pid".into(),
                header: format!(
                    r#"{{"abi":"Rust","is_async":false,"is_const":false,"is_unsafe":{unsafe_}}}"#
                ),
            },
        };
        let diffs = [header(Side::Missing, true), header(Side::Unexpected, false)];
        assert!(si_pid_header(&diffs, Side::Missing, true));
        assert!(si_pid_header(&diffs, Side::Unexpected, false));
        assert!(!si_pid_header(&diffs, Side::Missing, false));
        assert_eq!(
            diff_verdict::<ShapeDiff>(Ok(&[]), "x", |_| false),
            Err(ControlFailure::Escaped)
        );
    }

    #[test]
    fn a_symbol_control_needs_its_own_mismatch() {
        let caught = |d: &[SymbolDiff]| {
            d.iter().any(|diff| {
                matches!(diff, SymbolDiff::Mismatch { name, shim, .. }
                    if name == "getpid" && shim == PLANTED_SYMBOL)
            })
        };
        let planted = SymbolDiff::Mismatch {
            name: "getpid".into(),
            shim: PLANTED_SYMBOL.into(),
            reference: "getpid".into(),
        };
        let other = SymbolDiff::AbsentFromReference {
            name: "made_up".into(),
        };
        assert_eq!(
            diff_verdict(Ok(&[other.clone(), planted]), "x", caught),
            Ok(())
        );
        assert!(matches!(
            diff_verdict(Ok(&[other]), "x", caught),
            Err(ControlFailure::Unattributed { .. })
        ));
        let anchor = ControlFailure::Anchor {
            file: "darwin_aarch64.rs".into(),
            anchor: GETPID_DECL.into(),
            found: 0,
            expected: 1,
        };
        assert_eq!(
            diff_verdict::<SymbolDiff>(Err(&anchor), "x", caught),
            Err(anchor)
        );
    }

    fn cell_source(body: &str) -> String {
        format!("// generated\n{}\n{body}\n", siblings::BODY_MARKER)
    }

    #[test]
    fn a_sibling_control_proves_nothing_on_a_family_that_already_differs() {
        let body = format!("fn fd_set() {{ {FD_SET_GOOD} }}");
        let mut sources: BTreeMap<&'static str, String> = cells::CELLS
            .iter()
            .map(|cell| (cell.source, cell_source(&body)))
            .collect();
        let mut report = ControlsReport::default();
        sibling_controls(&sources, &mut report);
        assert_eq!(
            report.caught,
            [
                ControlId::SiblingBody { os: Os::Darwin },
                ControlId::SiblingBody { os: Os::GnuLinux }
            ]
        );

        sources.insert(
            "darwin_x86_64.rs",
            cell_source(&format!("{body}\nfn x86_only() {{}}")),
        );
        let mut report = ControlsReport::default();
        sibling_controls(&sources, &mut report);
        assert_eq!(report.caught, [ControlId::SiblingBody { os: Os::GnuLinux }]);
        assert!(matches!(
            report.failed.as_slice(),
            [(
                ControlId::SiblingBody { os: Os::Darwin },
                ControlFailure::Baseline { .. }
            )]
        ));
    }
}
