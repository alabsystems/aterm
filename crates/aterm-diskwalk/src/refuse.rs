// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PATH TEST run before any syscall (design §10.5 rule 7): which roots the
//! walker may touch at all, decided on path strings alone.
//!
//! A `stat` inside the person's Documents folder is itself an access that can raise a macOS
//! privacy prompt, and the prompt holds the calling syscall until a person
//! answers. So nothing here touches the file system: [`refuse`] reads the
//! spelling it is given (the logical form a shell or a person used, and the
//! canonical form the kernel names), the deny set ([`Deny`], built from
//! `aterm_containment::consent::tcc_prompting_roots`, the one list), and the
//! mount table (`aterm_sysprobe::volumes`, read with `MNT_NOWAIT`).
//!
//! * DENIED: every path under a prompting root, every path under another
//!   user's home, and all of `~/Library` but its allowlist ([`LIBRARY_ALLOW`]
//!   plus the person's opt-ins, design O20). On macOS the comparison ignores
//!   ASCII case, because APFS does: a lower-case spelling names the same folder.
//! * TOO BROAD: `/`, `$HOME`, an ancestor of `$HOME`, a volume root, and any
//!   ancestor of a denied root. Such a root gets a one-level listing
//!   ([`crate::list_one_level`]), never a walk (walking the unprotected part
//!   of a home took 222 s and 9.79 M entries).
//! * NOT LOCAL: a network, FUSE, automounted or autofs volume is never asked
//!   anything (a hung one answers a `stat` by blocking), nor is a removable
//!   one wherever it is mounted (its privacy prompt); an incomplete mount
//!   table cannot rule a nested one out.
//!
//! Each form is also tested with the data volume's firmlink prefix
//! (`/System/Volumes/Data`) stripped, and `/tmp`, `/var` and `/etc` are read as
//! the `/private` folders they link to, so no spelling slips past a rule. Every
//! comparison, the volume rules' included, ignores ASCII case where the deny
//! set does: `/users/a/src/NFS` crosses the same share as its true spelling.

use std::path::{Component, Path, PathBuf};

use aterm_sysprobe::volumes::{Mount, Table, mount_of};

/// The folders under `$HOME/Library` the walker may enter: aterm's own cache
/// and state (design §10.4 `own`). The rest of `~/Library` is denied by
/// default; a person may add a root in config (design O20), never a prompt.
pub const LIBRARY_ALLOW: &[&str] = &["Library/Caches/aterm", "Library/Application Support/aterm"];

/// The file system types the walker never enters even when the table calls
/// them local: network shares and FUSE (a hung one blocks a `stat`).
pub const REMOTE_FSTYPES: &[&str] = &[
    "nfs",
    "smbfs",
    "afpfs",
    "webdav",
    "cifs",
    "smb3",
    "fuse",
    "osxfuse",
    "macfuse",
    "fusefs",
    "sshfs",
    "ftp",
    "9p",
    "ceph",
    "glusterfs",
    "lustre",
];

/// macOS's data volume, under which every firmlinked root also lives.
const DATA_VOLUME: &str = "/System/Volumes/Data";

/// Why a root (or a folder inside one) is not measured. Each is a labelled
/// row, never a silent zero (design §10.4 "honest gaps").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// A macOS privacy-protected folder: a `stat` there could prompt.
    Protected,
    /// Another user's home.
    OtherUser,
    /// A network or FUSE volume.
    Remote,
    /// A removable volume.
    Removable,
    /// Mounted by the automounter: asking it may mount it, or block.
    Automounted,
    /// A cloud placeholder (`SF_DATALESS`): entering it would download it.
    Dataless,
    /// `/`, a home, a volume root or an ancestor of a protected folder: one
    /// level is listed, nothing is walked.
    TooBroad,
    /// Deeper than the policy's `max_depth`.
    TooDeep,
    /// Not there any more.
    Gone,
    /// A component is a symbolic link; the walker follows none.
    Link,
    /// Not a directory.
    NotDirectory,
    /// The system refused it (the raw errno).
    Unreadable(i32),
    /// Not an absolute plain path (relative, or with a `..`).
    Relative,
    /// The mount table is incomplete, so a remote mount inside cannot be ruled out.
    MountsUnknown,
    /// This platform has no walker.
    Unsupported,
}

impl Refusal {
    /// The wire word (`why=` of a `sys disk` row).
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Refusal::Protected => "protected",
            Refusal::OtherUser => "other-user",
            Refusal::Remote => "remote",
            Refusal::Removable => "removable",
            Refusal::Automounted => "automounted",
            Refusal::Dataless => "dataless",
            Refusal::TooBroad => "too-broad",
            Refusal::TooDeep => "too-deep",
            Refusal::Gone => "gone",
            Refusal::Link => "link",
            Refusal::NotDirectory => "not-directory",
            Refusal::Unreadable(_) => "unreadable",
            Refusal::Relative => "relative",
            Refusal::MountsUnknown => "mounts-unknown",
            Refusal::Unsupported => "unsupported",
        }
    }

    /// The sentence a row shows.
    #[inline]
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Refusal::Protected => "not measured: macOS privacy-protected folder",
            Refusal::OtherUser => "not measured: another user's folder",
            Refusal::Remote => "not measured: remote volume",
            Refusal::Removable => "not measured: removable volume",
            Refusal::Automounted => "not measured: automounted volume",
            Refusal::Dataless => "not measured: cloud placeholder (dataless)",
            Refusal::TooBroad => "too broad: one level",
            Refusal::TooDeep => "not measured: too deep",
            Refusal::Gone => "gone",
            Refusal::Link => "not measured: a symbolic link (never followed)",
            Refusal::NotDirectory => "not a folder",
            Refusal::Unreadable(_) => "not measured: unreadable",
            Refusal::Relative => "not measured: not an absolute path",
            Refusal::MountsUnknown => "not measured: the mount table is incomplete",
            Refusal::Unsupported => "not measured: no walker on this platform",
        }
    }
}

/// The deny set and the allowlist, resolved once for one home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deny {
    home: PathBuf,
    roots: Vec<PathBuf>,
    other_homes: Option<PathBuf>,
    library: PathBuf,
    allow: Vec<PathBuf>,
    opt_in: Vec<PathBuf>,
    fold_case: bool,
}

impl Deny {
    /// The deny set for `home`: `consent::tcc_prompting_roots(home,
    /// overrides)`, other users' homes, and `~/Library` but [`LIBRARY_ALLOW`].
    /// `opt_in` are roots the person named in config (design O20); one
    /// lifts the `~/Library` default and a protected folder's refusal for
    /// itself and what lies under it, never another user's home or a volume
    /// rule. A relative opt-in is dropped.
    #[must_use]
    pub fn for_home(home: &Path, overrides: &[String], opt_in: &[PathBuf]) -> Self {
        let home = canonical(home).unwrap_or_else(|| home.to_path_buf());
        let roots = aterm_containment::consent::tcc_prompting_roots(&home, overrides)
            .into_iter()
            .filter_map(|r| canonical(&r))
            .collect();
        Self {
            other_homes: aterm_containment::consent::other_homes_root(&home),
            library: home.join("Library"),
            allow: LIBRARY_ALLOW.iter().map(|a| home.join(a)).collect(),
            opt_in: opt_in.iter().filter_map(|p| canonical(p)).collect(),
            roots,
            home,
            fold_case: cfg!(target_vendor = "apple"),
        }
    }

    /// [`Self::for_home`] with the case rule named, for a test of the other
    /// platform's rule.
    #[inline]
    #[must_use]
    pub const fn with_fold_case(mut self, fold_case: bool) -> Self {
        self.fold_case = fold_case;
        self
    }

    /// The home this set was built for, canonical.
    #[inline]
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The denied roots, canonical (for a `not measured` listing).
    #[inline]
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Whether path comparisons ignore ASCII case (macOS, as APFS does).
    #[inline]
    #[must_use]
    pub const fn fold_case(&self) -> bool {
        self.fold_case
    }

    #[inline]
    fn under(&self, path: &Path, root: &Path) -> bool {
        under(path, root, self.fold_case)
    }

    /// Whether `path` (one canonical spelling) lies in a denied place, and why.
    /// Pure.
    #[must_use]
    pub fn denies(&self, path: &Path) -> Option<Refusal> {
        for form in forms(path, self.fold_case) {
            if let Some(why) = self.denies_form(&form) {
                return Some(why);
            }
        }
        None
    }

    fn denies_form(&self, path: &Path) -> Option<Refusal> {
        if let Some(other) = self.other_homes.as_deref()
            && self.under(path, other)
            && !self.under(path, &self.home)
            && !same(path, other, self.fold_case)
        {
            return Some(Refusal::OtherUser);
        }
        let fc = self.fold_case;
        let opted = self
            .opt_in
            .iter()
            .any(|o| forms(o, fc).iter().any(|of| self.under(path, of)));
        if opted {
            return None;
        }
        if self
            .roots
            .iter()
            .any(|r| forms(r, fc).iter().any(|rf| self.under(path, rf)))
        {
            return Some(Refusal::Protected);
        }
        if self.under(path, &self.library) && !self.allow.iter().any(|a| self.under(path, a)) {
            return Some(Refusal::Protected);
        }
        None
    }

    /// Whether `path` is a proper ancestor of a denied root or of the home
    /// (so a walk of it would meet one). Pure.
    #[must_use]
    pub fn above_denied(&self, path: &Path) -> bool {
        let fc = self.fold_case;
        let path_forms = forms(path, fc);
        let mut guarded: Vec<&Path> = self.roots.iter().map(PathBuf::as_path).collect();
        guarded.push(&self.library);
        guarded.push(&self.home);
        if let Some(other) = self.other_homes.as_deref() {
            guarded.push(other);
        }
        guarded.iter().any(|g| {
            forms(g, fc).iter().any(|gf| {
                path_forms
                    .iter()
                    .any(|pf| under(gf, pf, fc) && !same(gf, pf, fc))
            })
        })
    }
}

/// `path` read the way the kernel names it: absolute, no `.` or empty
/// components, and on macOS `/tmp`, `/var` and `/etc` (in any case, as the
/// system volume reads them) read as the `/private` folders they are links
/// to. `None` for a relative path or one with `..` (nobody can place it
/// without resolving links). Pure.
#[must_use]
pub fn canonical(path: &Path) -> Option<PathBuf> {
    canonical_for(path, cfg!(target_vendor = "apple"))
}

/// [`canonical`] with the platform named. Pure.
#[must_use]
pub fn canonical_for(path: &Path, macos: bool) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::from("/");
    let mut first = true;
    for c in path.components() {
        match c {
            Component::RootDir | Component::CurDir => {}
            Component::Prefix(_) | Component::ParentDir => return None,
            Component::Normal(name) => {
                let linked = ["tmp", "var", "etc"]
                    .iter()
                    .any(|l| name.as_encoded_bytes().eq_ignore_ascii_case(l.as_bytes()));
                if first && macos && linked {
                    out.push("private");
                }
                first = false;
                out.push(name);
            }
        }
    }
    Some(out)
}

/// The spellings a canonical path is tested in: itself and, under the data
/// volume (its prefix compared as every other component is), the firmlinked
/// form without that prefix.
fn forms(path: &Path, fold: bool) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    let data = Path::new(DATA_VOLUME);
    if under(path, data, fold) {
        let rest: PathBuf = path.components().skip(data.components().count()).collect();
        out.push(Path::new("/").join(rest));
    }
    out
}

/// Whether `path` lies at or under `root`, in any spelling of either, ASCII
/// case folded when `fold`. Pure.
#[must_use]
pub fn within(path: &Path, root: &Path, fold: bool) -> bool {
    let roots = forms(root, fold);
    forms(path, fold)
        .iter()
        .any(|p| roots.iter().any(|r| under(p, r, fold)))
}

/// Whether `dir` is a mount point of `mounts`, in any spelling. Pure.
#[must_use]
pub fn is_mount_point(dir: &Path, mounts: &Table, fold: bool) -> bool {
    let dirs = forms(dir, fold);
    mounts.mounts.iter().any(|m| {
        mount_forms(m, fold)
            .iter()
            .any(|o| dirs.iter().any(|d| same(d, o, fold)))
    })
}

/// The spellings of a mount point: its canonical path and, for a mount nested
/// in the data volume, the firmlinked one. The data volume itself is only
/// itself: its stripped form would be `/`, the system volume's.
fn mount_forms(m: &Mount, fold: bool) -> Vec<PathBuf> {
    let Some(on) = canonical(&m.on) else {
        return Vec::new();
    };
    if same(&on, Path::new(DATA_VOLUME), fold) {
        vec![on]
    } else {
        forms(&on, fold)
    }
}

/// Component-wise prefix test; ASCII case folded when `fold`.
fn under(path: &Path, root: &Path, fold: bool) -> bool {
    let mut p = path.components();
    for r in root.components() {
        match p.next() {
            Some(pc) if eq_component(pc, r, fold) => {}
            _ => return false,
        }
    }
    true
}

fn same(a: &Path, b: &Path, fold: bool) -> bool {
    under(a, b, fold) && under(b, a, fold)
}

fn eq_component(a: Component<'_>, b: Component<'_>, fold: bool) -> bool {
    if fold {
        a.as_os_str()
            .as_encoded_bytes()
            .eq_ignore_ascii_case(b.as_os_str().as_encoded_bytes())
    } else {
        a == b
    }
}

/// THE PATH TEST, run before any syscall on the logical form (as a shell or a
/// person spelled it) and the canonical form (as the kernel names it). `None`
/// when the walker may open the root. Pure: no path is touched.
#[must_use]
pub fn refuse(
    logical: Option<&Path>,
    canonical_path: &Path,
    mounts: &Table,
    deny: &Deny,
) -> Option<Refusal> {
    let spellings = match spellings(logical, canonical_path) {
        Ok(s) => s,
        Err(why) => return Some(why),
    };
    for s in &spellings {
        if let Some(why) = deny.denies(s) {
            return Some(why);
        }
    }
    for s in &spellings {
        if broad(s, mounts, deny) {
            return Some(Refusal::TooBroad);
        }
    }
    volume_refusal(&spellings[0], mounts, deny.fold_case)
}

/// The path test for a LOOK, not a walk: one directory opened to `stat` a
/// marker name in it (design §10.4's anchor and worktree searches). The deny
/// set and the volume rules as [`refuse`]; a broad directory may be looked
/// in, since nothing under it is listed. Pure.
#[must_use]
pub fn refuse_look(
    logical: Option<&Path>,
    canonical_path: &Path,
    mounts: &Table,
    deny: &Deny,
) -> Option<Refusal> {
    let spellings = match spellings(logical, canonical_path) {
        Ok(s) => s,
        Err(why) => return Some(why),
    };
    for s in &spellings {
        if let Some(why) = deny.denies(s) {
            return Some(why);
        }
    }
    volume_refusal(&spellings[0], mounts, deny.fold_case)
}

/// Every spelling a path test reads, the canonical form first: the canonical
/// path, and with a logical spelling both its canonical form and itself.
fn spellings(logical: Option<&Path>, canonical_path: &Path) -> Result<Vec<PathBuf>, Refusal> {
    let canon = canonical(canonical_path).ok_or(Refusal::Relative)?;
    let mut spellings = vec![canon];
    if let Some(l) = logical {
        spellings.push(canonical(l).ok_or(Refusal::Relative)?);
        spellings.push(l.to_path_buf());
    }
    Ok(spellings)
}

/// `/`, the home, an ancestor of a denied root or of the home, or a volume's
/// mount point.
fn broad(path: &Path, mounts: &Table, deny: &Deny) -> bool {
    let fc = deny.fold_case;
    forms(path, fc).iter().any(|f| {
        f.parent().is_none()
            || same(f, deny.home(), fc)
            || deny.above_denied(f)
            || is_mount_point(f, mounts, fc)
    })
}

/// The refusal the volume holding `path` earns: remote, automounted,
/// removable, or an incomplete table. Every mount `path` lies at or under counts, not only the
/// nearest: a component-wise open reaches a folder through each volume above
/// it, so a local volume mounted inside a share is reached through the share.
/// ASCII case is folded when `fold` ([`Deny::fold_case`]). Pure.
#[must_use]
pub fn volume_refusal(path: &Path, mounts: &Table, fold: bool) -> Option<Refusal> {
    if !mounts.complete {
        return Some(Refusal::MountsUnknown);
    }
    let paths = forms(path, fold);
    let mut holders: Vec<(&Mount, usize)> = mounts
        .mounts
        .iter()
        .filter_map(|m| {
            mount_forms(m, fold)
                .iter()
                .filter(|o| paths.iter().any(|p| under(p, o, fold)))
                .map(|o| o.components().count())
                .max()
                .map(|depth| (m, depth))
        })
        .collect();
    // The firmlink-placed lookup too: a path under a firmlinked root is on the
    // data volume though no spelling of it carries that prefix.
    if let Some(m) = mount_of(path, mounts) {
        holders.push((m, usize::MAX));
    }
    if holders.is_empty() {
        return Some(Refusal::MountsUnknown);
    }
    // The nearest volume's reason first.
    holders.sort_by_key(|&(_, depth)| std::cmp::Reverse(depth));
    holders.iter().find_map(|&(m, _)| unsafe_mount(m))
}

/// What a mount earns on its own: automounted (or autofs), removable (a
/// `stat` there can raise the removable-volumes privacy prompt, wherever it is
/// mounted), or remote (not local, a network or FUSE type).
fn unsafe_mount(m: &Mount) -> Option<Refusal> {
    if m.automounted || m.fstype == "autofs" {
        return Some(Refusal::Automounted);
    }
    if m.removable {
        return Some(Refusal::Removable);
    }
    let fstype = m.fstype.to_ascii_lowercase();
    if !m.local
        || REMOTE_FSTYPES
            .iter()
            .any(|r| fstype == *r || fstype.starts_with("fuse"))
    {
        return Some(Refusal::Remote);
    }
    None
}

/// The mount points `dir` directly holds that the walker must never ask
/// anything (a remote, removable, automounted or autofs mount): the lister skips these
/// names instead of reading their attributes. `dir` and each mount point are
/// compared in every spelling, ASCII case folded when `fold`. Pure.
#[must_use]
pub fn unsafe_mounts_in(dir: &Path, mounts: &Table, fold: bool) -> Vec<std::ffi::OsString> {
    let dirs = forms(dir, fold);
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for m in mounts.mounts.iter().filter(|m| unsafe_mount(m).is_some()) {
        let name = mount_forms(m, fold).into_iter().find_map(|o| {
            let parent = o.parent()?;
            dirs.iter()
                .any(|d| same(parent, d, fold))
                .then(|| o.file_name().map(std::ffi::OsStr::to_os_string))
                .flatten()
        });
        if let Some(name) = name
            && !out.contains(&name)
        {
            out.push(name);
        }
    }
    out
}
