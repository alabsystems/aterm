// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WHICH BUILD A RUNNING IMAGE IS (`docs/DESIGN-card-engine-2026-10-19.md` §3.2), from
//! the executable the kernel mapped for it (`proc_pidpath` on macOS, `/proc/<pid>/exe` on
//! Linux, both canonical) and, for a script shell only, its first argument. Never from a
//! file the program writes about itself at its own pace: the terminal names a Claude Code
//! build at its exec, at least 225 ms before the program draws, where the vendor's
//! registry row lands 15-30 ms after its first frame on the inline renderer (measured
//! 2026-10-19).
//!
//! The answer ([`ImageBuild`]) is one of five sources ([`ImageSource`]):
//!
//! * **Store** — a vendor build in the managed store: `<prefix>/store/<program>/<id>/…`
//!   with `<id>` a vendor build id, which IS its version ([`Version::from_build_id`]).
//!   The match is anchored at `<prefix>/store` ([`crate::ops::store_build_of`]) with the
//!   prefix canonicalized once per [`ImageRoots`], and every component past the anchor
//!   must be a plain name: `/x/src/store/claude/<id>/bin/claude` (a dev checkout) and a
//!   `..` escape are not the store, where the unanchored
//!   [`crate::ops::program_build_of_target`] would say they are.
//! * **StoreRecord** — an ALab index build in the managed store (an id below
//!   [`crate::vendor_direct::VENDOR_BUILD_BASE`]): its version is what aterm's own record
//!   says, read once per image. The record is the program's stamp
//!   ([`crate::vendor_direct::ProgramStamp::legacy_version_of`], read from the build's
//!   signed manifest by the vendor lane); a build with no record is Unknown.
//! * **Native** — the vendor's own installer's `…/<tool>/versions/<x.y.z>`, the file named
//!   by its version ([`Version::parse`]). Linux's ` (deleted)` suffix (an image unlinked
//!   while it runs, which a self-updater does) is stripped first.
//! * **Launcher** — atpkg's own shim before its `exec`: an interpreter in
//!   [`SCRIPT_SHELLS`] whose first argument is a script directly in the managed prefix's
//!   `agents/`, `bin/` or `reroute/`. A user's `sh ~/bin/claude` is not one: it stays
//!   the shell it is, as `aterm-gui`'s program naming rules.
//! * **Unknown** — everything else: an npm or bun `node`, a brew cask, an `aterm pkg link`
//!   checkout, an image that could not be read. Its program is the executable's name,
//!   and it names no build.
//!
//! [`image_build`] does the one read a StoreRecord needs; [`image_build_with`] takes it as
//! a parameter. [`ImageRoots`] caches the prefix and known shim-directory spellings. A
//! script spelled through another path to a shim directory (a symlinked prefix) is
//! resolved only for an absolute first argument whose name is an entry of one of those
//! directories. This skips parent resolution for names absent there. A matching name
//! still canonicalizes the supplied parent, including a network path; these filesystem
//! reads can block and describe the paths only when read. Injecting the StoreRecord reader
//! does not replace them.
//!
//! A Store build needs no record cross-check: [`crate::vendor_direct::read_record`] already
//! refuses a record whose version's build id is not its directory's, and the directory's
//! id IS the version here.

use std::path::{Component, Path, PathBuf};

use crate::store::Layout;
use crate::vendor_direct::{VENDOR_BUILD_BASE, Version};

/// The interpreters an agent's shim script runs under (`#!/bin/sh`), whose first argument
/// then names the script: a process whose executable is one of these and whose first
/// argument is atpkg's own shim is that shim, before its `exec` ([`ImageSource::Launcher`];
/// `aterm-gui`'s `session_program::program_from_argv` names it as its agent).
pub const SCRIPT_SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ksh"];

/// The longest program name an [`ImageBuild`] keeps, in bytes.
const PROGRAM_MAX: usize = 64;

/// What Linux appends to `/proc/<pid>/exe` when the image was unlinked while it runs.
const DELETED_SUFFIX: &str = " (deleted)";

/// Where an image's build came from: what a reader of [`ImageBuild::build`] may trust it
/// for. Module docs: one rule per source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageSource {
    /// A vendor build in the managed store; its directory names its version.
    Store,
    /// An ALab index build in the managed store, its version from aterm's own record.
    StoreRecord,
    /// The vendor installer's `…/<tool>/versions/<x.y.z>`.
    Native,
    /// atpkg's own shim script before its `exec`; no build.
    Launcher,
    /// Anything else; no build.
    Unknown,
}

impl ImageSource {
    /// The word a log or timeline row prints.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Store => "store",
            Self::StoreRecord => "store-record",
            Self::Native => "native",
            Self::Launcher => "launcher",
            Self::Unknown => "unknown",
        }
    }
}

/// A program's name as an image spells it: a store directory, a native installer's tool
/// directory, a shim's file name or an executable's: `[A-Za-z0-9._+-]`, at most
/// [`PROGRAM_MAX`] bytes, never empty.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProgramTag(Box<str>);

impl ProgramTag {
    /// `name` as a tag, or `None` when it is empty, too long or holds anything else.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        let ok = !name.is_empty()
            && name.len() <= PROGRAM_MAX
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'));
        ok.then(|| Self(name.into()))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which program and build an image is ([`image_build`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageBuild {
    /// The program, where the image names one.
    pub program: Option<ProgramTag>,
    /// The vendor build id of its version ([`Version::build_id`]); `0` names none.
    pub build: u64,
    /// Where `build` came from.
    pub source: ImageSource,
}

impl ImageBuild {
    /// The version `build` names, or `None` when it names none.
    #[must_use]
    pub const fn version(&self) -> Option<Version> {
        Version::from_build_id(self.build)
    }

    /// An Unknown image: `program` (or nothing, for an image that could not be read) and
    /// no build.
    #[must_use]
    pub fn unknown(program: Option<ProgramTag>) -> Self {
        Self {
            program,
            build: 0,
            source: ImageSource::Unknown,
        }
    }
}

/// The managed prefix's directories an image is classified against, each spelled as the
/// layout names it and as the file system resolves it, canonicalized ONCE here: a process
/// that reads many images (the terminal's PTY readers, one per foreground edge) keeps one
/// of these per layout instead of resolving the prefix per image.
#[derive(Clone, Debug)]
pub struct ImageRoots {
    layout: Layout,
    /// `<canonical prefix>`, which [`crate::ops::store_build_of`] anchors `store/` under.
    prefix: PathBuf,
    /// `agents/`, `bin/` and `reroute/`, as the layout spells them and resolved.
    shim_dirs: Vec<PathBuf>,
}

impl ImageRoots {
    /// The roots of `layout`. A prefix that does not resolve (not laid yet) is kept as
    /// spelled, and only an image under that spelling can be its store's.
    #[must_use]
    pub fn new(layout: &Layout) -> Self {
        let prefix =
            std::fs::canonicalize(&layout.prefix).unwrap_or_else(|_| layout.prefix.clone());
        let resolved = Layout {
            prefix: prefix.clone(),
        };
        let mut shim_dirs = Vec::with_capacity(6);
        for l in [layout, &resolved] {
            for dir in [l.agents_dir(), l.bin_dir(), l.reroute_dir()] {
                let real = std::fs::canonicalize(&dir).ok();
                for d in std::iter::once(dir).chain(real) {
                    if !shim_dirs.contains(&d) {
                        shim_dirs.push(d);
                    }
                }
            }
        }
        Self {
            layout: layout.clone(),
            prefix,
            shim_dirs,
        }
    }

    /// The layout these roots were made of.
    #[must_use]
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Whether `script` is a file DIRECTLY in `agents/`, `bin/` or `reroute/`, its
    /// directory as spelled or as resolved. A deeper directory or a sibling never counts.
    ///
    /// Only an absolute path names the file the shell ran: a relative one would resolve
    /// against this process's directory, not the shell's. A parent spelled as none of the
    /// roots is resolved only when a shim directory holds an entry of the script's name
    /// (one `lstat` inside the prefix per directory). An absent name skips resolution;
    /// a matching name still canonicalizes that parent, including a network path. This
    /// prefilter does not bound the filesystem calls or guard concurrent path changes.
    fn holds_shim(&self, script: &Path) -> bool {
        if !script.is_absolute() {
            return false;
        }
        let (Some(parent), Some(name)) = (script.parent(), script.file_name()) else {
            return false;
        };
        if self.shim_dirs.iter().any(|d| d == parent) {
            return true;
        }
        self.shim_dirs
            .iter()
            .any(|d| d.join(name).symlink_metadata().is_ok())
            && std::fs::canonicalize(parent)
                .is_ok_and(|resolved| self.shim_dirs.contains(&resolved))
    }
}

/// THE CLASSIFIER, with the one file it may read for a StoreRecord: `program`'s stamp in
/// `roots`' layout ([`crate::vendor_direct::ProgramStamp::legacy_version_of`]). `exe` is
/// the image the kernel mapped (canonical), `argv1` the first argument, read only for a
/// script shell (`None` otherwise). `roots` is `None` where no managed prefix resolves:
/// then nothing is Store, StoreRecord or Launcher.
#[must_use]
pub fn image_build(roots: Option<&ImageRoots>, exe: &Path, argv1: Option<&Path>) -> ImageBuild {
    image_build_with(roots, exe, argv1, &|program, build| {
        let layout = roots?.layout();
        crate::vendor_direct::ProgramStamp::read(layout, program)?.legacy_version_of(build)
    })
}

/// [`image_build`] with the StoreRecord read as a parameter: `record(program, build)` is
/// the version aterm's record gives index build `build` of `program`, asked at most once.
/// This replaces the stamp read, not the shim-parent resolution described in the module
/// documentation.
#[must_use]
pub fn image_build_with(
    roots: Option<&ImageRoots>,
    exe: &Path,
    argv1: Option<&Path>,
    record: &dyn Fn(&str, u64) -> Option<Version>,
) -> ImageBuild {
    classify(roots, exe, argv1, record, anchored_store_build)
}

/// What decides that a path is a managed store's build: `(prefix, exe)` to
/// `(program, build)`. [`anchored_store_build`] in the product; the tests' control
/// swaps in the unanchored parser to show the anchor is load-bearing.
type StoreMatch = fn(&Path, &Path) -> Option<(String, u64)>;

fn classify(
    roots: Option<&ImageRoots>,
    exe: &Path,
    argv1: Option<&Path>,
    record: &dyn Fn(&str, u64) -> Option<Version>,
    store_of: StoreMatch,
) -> ImageBuild {
    let exe = strip_deleted(exe);
    let name = exe.file_name().and_then(|n| n.to_str());
    // A script shell: atpkg's own shim, or the shell it is.
    if let Some(shell) = name.filter(|n| SCRIPT_SHELLS.contains(n)) {
        let shim = roots.zip(argv1).and_then(|(roots, script)| {
            let text = script.to_str()?;
            if text.starts_with('-') || !roots.holds_shim(script) {
                return None;
            }
            ProgramTag::new(script.file_name()?.to_str()?)
        });
        return match shim {
            Some(program) => ImageBuild {
                program: Some(program),
                build: 0,
                source: ImageSource::Launcher,
            },
            None => ImageBuild::unknown(ProgramTag::new(shell)),
        };
    }
    if let Some((program, build)) = roots.and_then(|r| store_of(&r.prefix, exe)) {
        let tag = ProgramTag::new(&program);
        if build >= VENDOR_BUILD_BASE {
            return match Version::from_build_id(build) {
                Some(version) => ImageBuild {
                    program: tag,
                    build: version.build_id(),
                    source: ImageSource::Store,
                },
                None => ImageBuild::unknown(tag),
            };
        }
        return match record(&program, build) {
            Some(version) => ImageBuild {
                program: tag,
                build: version.build_id(),
                source: ImageSource::StoreRecord,
            },
            None => ImageBuild::unknown(tag),
        };
    }
    if let Some((tool, version)) = native_build(exe) {
        return ImageBuild {
            program: ProgramTag::new(tool),
            build: version.build_id(),
            source: ImageSource::Native,
        };
    }
    ImageBuild::unknown(name.and_then(ProgramTag::new))
}

/// `exe` without Linux's ` (deleted)` suffix: the image the process runs, which a
/// self-updater unlinking it does not change.
#[must_use]
pub fn strip_deleted(exe: &Path) -> &Path {
    exe.to_str()
        .and_then(|s| s.strip_suffix(DELETED_SUFFIX))
        .map_or(exe, Path::new)
}

/// `(program, build)` of a path INSIDE `prefix`'s store: [`crate::ops::store_build_of`],
/// with every component past `<prefix>/store` a plain name (no `..` walks back out) and
/// the build spelled as the store writes it (digits, no leading zero).
fn anchored_store_build(prefix: &Path, exe: &Path) -> Option<(String, u64)> {
    let (program, build) = crate::ops::store_build_of(prefix, exe)?;
    let rest = exe.strip_prefix(prefix.join("store")).ok()?;
    let plain = rest.components().all(|c| matches!(c, Component::Normal(_)));
    let spelled = rest
        .components()
        .nth(1)
        .and_then(|c| c.as_os_str().to_str())
        .and_then(crate::store::parse_build_name);
    (plain && spelled == Some(build)).then_some((program, build))
}

/// `(tool, version)` of a native installer's build: `…/<tool>/versions/<version>`.
fn native_build(exe: &Path) -> Option<(&str, Version)> {
    let version = Version::parse(exe.file_name()?.to_str()?)?;
    let versions = exe.parent()?;
    if versions.file_name()? != "versions" {
        return None;
    }
    let tool = versions.parent()?.file_name()?.to_str()?;
    Some((tool, version))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store ids the design's table names.
    const CLAUDE_304: u64 = 1_000_002_000_001_000_304;
    const AGY_142: u64 = 1_000_001_000_004_000_002;
    const MUSE_R: u64 = 2_001_004_011_006_667_002;
    const ALAB: u64 = 2_026_092_201;

    struct Row {
        exe: PathBuf,
        argv1: Option<PathBuf>,
        want: ImageBuild,
        why: &'static str,
    }

    fn build(program: &str, build: u64, source: ImageSource) -> ImageBuild {
        ImageBuild {
            program: ProgramTag::new(program),
            build,
            source,
        }
    }

    fn unknown(program: &str) -> ImageBuild {
        build(program, 0, ImageSource::Unknown)
    }

    /// The design's table (§7, "atpkg::image table") over a scratch prefix.
    fn table(l: &Layout) -> Vec<Row> {
        let p = std::fs::canonicalize(&l.prefix).expect("the scratch prefix resolves");
        let store = |program: &str, id: u64| p.join(format!("store/{program}/{id}/bin/{program}"));
        let row = |exe: PathBuf, argv1: Option<PathBuf>, want, why| Row {
            exe,
            argv1,
            want,
            why,
        };
        vec![
            row(
                store("claude", CLAUDE_304),
                None,
                build("claude", CLAUDE_304, ImageSource::Store),
                "a vendor store build",
            ),
            row(
                store("agy", AGY_142),
                None,
                build("agy", AGY_142, ImageSource::Store),
                "agy 1.4.2",
            ),
            row(
                store("muse", MUSE_R),
                None,
                build("muse", MUSE_R, ImageSource::Store),
                "the revision tier",
            ),
            row(
                store("claude", ALAB),
                None,
                build("claude", CLAUDE_304, ImageSource::StoreRecord),
                "an ALab index build with a record",
            ),
            row(
                store("codex", ALAB),
                None,
                unknown("codex"),
                "an ALab index build with no record",
            ),
            row(
                PathBuf::from("/Users//u/.local/share/claude/versions/2.1.304"),
                None,
                build("claude", CLAUDE_304, ImageSource::Native),
                "the native installer's build",
            ),
            row(
                PathBuf::from("/home/u/.local/share/claude/versions/2.1.304 (deleted)"),
                None,
                build("claude", CLAUDE_304, ImageSource::Native),
                "Linux's (deleted) is stripped",
            ),
            row(
                PathBuf::from("/bin/sh"),
                Some(l.agents_dir().join("claude")),
                build("claude", 0, ImageSource::Launcher),
                "the twin before its exec",
            ),
            row(
                PathBuf::from("/bin/sh"),
                Some(l.reroute_dir().join("claude")),
                build("claude", 0, ImageSource::Launcher),
                "the reroute stub before its exec",
            ),
            row(
                PathBuf::from("/usr/bin/dash"),
                Some(l.bin_dir().join("claude")),
                build("claude", 0, ImageSource::Launcher),
                "a bin/ shim under dash",
            ),
            row(
                PathBuf::from("/bin/sh"),
                Some(PathBuf::from("/Users//u/bin/claude")),
                unknown("sh"),
                "a user's own wrapper is the shell",
            ),
            row(
                PathBuf::from("/bin/sh"),
                Some(l.agents_dir().join("sub/claude")),
                unknown("sh"),
                "deeper than agents/ is not a shim",
            ),
            row(
                PathBuf::from("/bin/sh"),
                Some(PathBuf::from("-c")),
                unknown("sh"),
                "an option is no script",
            ),
            row(
                PathBuf::from("/opt/homebrew/bin/node"),
                Some(PathBuf::from("/opt/homebrew/bin/claude")),
                unknown("node"),
                "npm's node",
            ),
            row(
                PathBuf::from("/x/src/store/claude/1000002000001000304/bin/claude"),
                None,
                unknown("claude"),
                "CONTROL: a dev checkout's store-shaped path is not the store",
            ),
            row(
                p.join("store/claude/1000002000001000304/../../../x/bin/claude"),
                None,
                unknown("claude"),
                "CONTROL: a .. escape out of the store",
            ),
            row(
                p.join("store/../store/claude/1000002000001000304/bin/claude"),
                None,
                unknown("claude"),
                "CONTROL: a .. at the anchor",
            ),
            row(
                p.join("store/claude/01000002000001000304/bin/claude"),
                None,
                unknown("claude"),
                "a build the store never spells",
            ),
            row(
                p.join("store/claude/3000000000000000000/bin/claude"),
                None,
                unknown("claude"),
                "an id past the revision tier",
            ),
        ]
    }

    /// The stamp record of the ALab row: claude's index build `ALAB` is 2.1.304.
    fn record(program: &str, b: u64) -> Option<Version> {
        (program == "claude" && b == ALAB).then(|| Version::parse("2.1.304").expect("canonical"))
    }

    fn misses(roots: &ImageRoots, store_of: StoreMatch) -> Vec<String> {
        table(roots.layout())
            .into_iter()
            .filter_map(|row| {
                let got = classify(
                    Some(roots),
                    &row.exe,
                    row.argv1.as_deref(),
                    &record,
                    store_of,
                );
                (got != row.want).then(|| format!("{}: {got:?} != {:?}", row.why, row.want))
            })
            .collect()
    }

    /// THE TABLE (§7). NEGATIVE CONTROL: the same table through the UNANCHORED store
    /// parser ([`crate::ops::program_build_of_target`], the mutant the anchor exists to
    /// refuse) misclassifies the dev checkout's path and the `..` escape as store builds.
    #[test]
    fn the_image_table_names_each_build_by_its_source() {
        let l = crate::testdir::layout("atpkg-image-table-");
        for dir in [l.agents_dir(), l.bin_dir(), l.reroute_dir()] {
            std::fs::create_dir_all(dir).expect("shim dirs");
        }
        let roots = ImageRoots::new(&l);
        assert_eq!(misses(&roots, anchored_store_build), Vec::<String>::new());
        let unanchored: StoreMatch = |_prefix, exe| crate::ops::program_build_of_target(exe);
        let mutant = misses(&roots, unanchored);
        assert!(
            mutant
                .iter()
                .any(|m| m.starts_with("CONTROL: a dev checkout"))
                && mutant.iter().any(|m| m.starts_with("CONTROL: a .. escape")),
            "the unanchored matcher must fail the controls: {mutant:#?}"
        );
    }

    /// No managed prefix: nothing is Store, StoreRecord or Launcher, and a native
    /// build is still itself.
    #[test]
    fn with_no_prefix_only_a_native_build_names_a_version() {
        let l = crate::testdir::layout("atpkg-image-noprefix-");
        let rows = table(&l);
        for row in rows {
            let got = image_build_with(None, &row.exe, row.argv1.as_deref(), &record);
            match row.want.source {
                ImageSource::Native => assert_eq!(got, row.want, "{}", row.why),
                _ => assert_eq!(got.build, 0, "{}: {got:?}", row.why),
            }
        }
    }

    /// A shim directory reached through a symlinked prefix (macOS's `/tmp` →
    /// `/private/tmp`) is the same directory: the script is spelled through the link,
    /// the roots resolve it. NEGATIVE CONTROLS: a name no shim directory holds is never
    /// resolved, however its parent resolves, and a relative script names no file.
    #[cfg(unix)]
    #[test]
    fn a_shim_spelled_through_a_link_is_a_launcher() {
        let real = crate::testdir::dir("atpkg-image-real-");
        let links = crate::testdir::dir("atpkg-image-link-");
        let prefix = real.join("pkg");
        std::fs::create_dir_all(prefix.join("agents")).expect("agents/");
        std::fs::write(prefix.join("agents/claude"), "#!/bin/sh\n").expect("the shim");
        let link = links.join("pkg");
        std::os::unix::fs::symlink(&prefix, &link).expect("link the prefix");
        let roots = ImageRoots::new(&Layout {
            prefix: prefix.clone(),
        });
        let classify = |script: &Path| {
            image_build_with(Some(&roots), Path::new("/bin/sh"), Some(script), &record)
        };
        assert_eq!(
            classify(&link.join("agents/claude")),
            build("claude", 0, ImageSource::Launcher)
        );
        assert_eq!(
            classify(&link.join("agents/codex")),
            unknown("sh"),
            "no shim of that name: the parent is never resolved"
        );
        // A relative script names no file the shell ran, even spelled as a
        // (relative) layout's shim directory is.
        let relative = ImageRoots::new(&Layout {
            prefix: PathBuf::from("rel/pkg"),
        });
        assert_eq!(
            image_build_with(
                Some(&relative),
                Path::new("/bin/sh"),
                Some(Path::new("rel/pkg/agents/claude")),
                &record,
            ),
            unknown("sh"),
            "a relative script resolves against no shell's directory"
        );
    }

    /// The StoreRecord read is the program's stamp: none recorded is Unknown, and a
    /// recorded legacy version names the build ([`image_build`], the product's reader).
    #[test]
    fn a_store_record_is_the_programs_stamp() {
        let l = crate::testdir::layout("atpkg-image-stamp-");
        let roots = ImageRoots::new(&l);
        let p = std::fs::canonicalize(&l.prefix).expect("resolves");
        let exe = p.join(format!("store/claude/{ALAB}/bin/claude"));
        assert_eq!(image_build(Some(&roots), &exe, None), unknown("claude"));
        let v = Version::parse("2.1.304").expect("canonical");
        crate::vendor_direct::ProgramStamp::record_legacy_version(&l, "claude", ALAB, v)
            .expect("record");
        assert_eq!(
            image_build(Some(&roots), &exe, None),
            build("claude", CLAUDE_304, ImageSource::StoreRecord)
        );
    }

    #[test]
    fn a_program_tag_is_a_name() {
        assert!(ProgramTag::new("muse-bin-1.4.10-R6427.1").is_some());
        assert!(ProgramTag::new("").is_none());
        assert!(ProgramTag::new("a b").is_none());
        assert!(ProgramTag::new(&"x".repeat(PROGRAM_MAX + 1)).is_none());
    }
}
