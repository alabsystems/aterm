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
/// the build spelled as the store writes it (digits, no leading zero; `.noindex` after them
/// for a build laid aside, [`aterm_toolchain::layout`]).
fn anchored_store_build(prefix: &Path, exe: &Path) -> Option<(String, u64)> {
    let (program, build) = crate::ops::store_build_of(prefix, exe)?;
    let rest = exe.strip_prefix(prefix.join("store")).ok()?;
    let plain = rest.components().all(|c| matches!(c, Component::Normal(_)));
    let spelled = rest
        .components()
        .nth(1)
        .and_then(|c| c.as_os_str().to_str())
        .map(|name| {
            name.strip_suffix(aterm_toolchain::build_cache::NOINDEX)
                .unwrap_or(name)
        })
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
#[path = "image_tests.rs"]
mod tests;
