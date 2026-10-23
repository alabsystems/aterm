// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Process executable image classification (`Store`, `StoreRecord`, `Native`, `Launcher`, `Unknown`).

use std::path::{Component, Path, PathBuf};

use crate::store::Layout;
use crate::vendor_direct::{VENDOR_BUILD_BASE, Version};

/// Shell interpreter basenames under which an agent shim script's `argv[1]` names the script.
pub const SCRIPT_SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ksh"];

/// The longest program name an [`ImageBuild`] keeps, in bytes.
const PROGRAM_MAX: usize = 64;

/// What Linux appends to `/proc/<pid>/exe` when the image was unlinked while it runs.
const DELETED_SUFFIX: &str = " (deleted)";

/// Where an image's build came from: what a reader of [`ImageBuild::build`] may trust it
/// for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageSource {
    /// Vendor build in the managed store (`<prefix>/store/<program>/<id>/…`).
    Store,
    /// ALab index build in the managed store resolved from aterm's vendor stamp.
    StoreRecord,
    /// Vendor installer build (`…/<tool>/versions/<x.y.z>`).
    Native,
    /// atpkg shim script prior to `exec` (no resolved build).
    Launcher,
    /// Unrecognized executable path (no resolved build).
    Unknown,
}

impl ImageSource {
    /// Wire and log token for this source.
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

/// Validated ASCII lowercase/digit/`-`/`_` program identifier.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProgramTag(Box<str>);

impl ProgramTag {
    /// Validate `name` as a [`ProgramTag`].
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        let ok = !name.is_empty()
            && name.len() <= PROGRAM_MAX
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'));
        ok.then(|| Self(name.into()))
    }

    /// Program name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Classified program tag, vendor build ID, and provenance of a process image.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageBuild {
    /// Program tag, if identified.
    pub program: Option<ProgramTag>,
    /// Vendor build ID (`0` when unknown).
    pub build: u64,
    /// Provenance of `build`.
    pub source: ImageSource,
}

impl ImageBuild {
    /// Decoded [`Version`] for `build`, or `None` when `build == 0`.
    #[must_use]
    pub const fn version(&self) -> Option<Version> {
        Version::from_build_id(self.build)
    }

    /// Construct an [`ImageSource::Unknown`] classification with `build = 0`.
    #[must_use]
    pub const fn unknown(program: Option<ProgramTag>) -> Self {
        Self {
            program,
            build: 0,
            source: ImageSource::Unknown,
        }
    }
}

/// Canonicalized and literal managed-prefix and shim directories for image classification.
#[derive(Clone, Debug)]
pub struct ImageRoots {
    layout: Layout,
    /// `<canonical prefix>`, which [`crate::ops::store_build_of`] anchors `store/` under.
    prefix: PathBuf,
    /// `agents/`, `bin/` and `reroute/`, as the layout spells them and resolved.
    shim_dirs: Vec<PathBuf>,
}

impl ImageRoots {
    /// Resolve and cache the store prefix and shim directories from `layout`.
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

    /// Underlying package manager [`Layout`].
    #[must_use]
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Whether `script` is a file DIRECTLY in `agents/`, `bin/` or `reroute/`, its
    /// directory as spelled or as resolved.
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

/// Classify `exe` (and `argv1` for a script shell) against `roots` and the layout's vendor stamps.
#[must_use]
pub fn image_build(roots: Option<&ImageRoots>, exe: &Path, argv1: Option<&Path>) -> ImageBuild {
    image_build_with(roots, exe, argv1, &|program, build| {
        let layout = roots?.layout();
        crate::vendor_direct::ProgramStamp::read(layout, program)?.legacy_version_of(build)
    })
}

/// Classify `exe` using `record` to resolve `StoreRecord` versions.
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
/// `(program, build)`.
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

/// Strip Linux's `" (deleted)"` suffix from `/proc/<pid>/exe` paths.
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
        // `rest` under `p` with its `..` kept: a join onto a verbatim prefix (`p` canonical
        // on Windows) resolves each `..` as it pushes.
        let dotted = |rest: &str| {
            let mut path = p.clone().into_os_string();
            for name in rest.split('/') {
                path.push(std::path::MAIN_SEPARATOR_STR);
                path.push(name);
            }
            PathBuf::from(path)
        };
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
                dotted("store/claude/1000002000001000304/../../../x/bin/claude"),
                None,
                unknown("claude"),
                "CONTROL: a .. escape out of the store",
            ),
            row(
                dotted("store/../store/claude/1000002000001000304/bin/claude"),
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
    /// the roots resolve it.
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
