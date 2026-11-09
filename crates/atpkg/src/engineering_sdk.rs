// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit local engineering composition, separate from signed installation and build completion.
//! Only `produce` constructs a candidate, from actual leased inputs and verified copied bytes.
//! It never changes an existing prefix, current link, package pin, or release record.

use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_dirfd::Mode;
use trust_resource_provenance::program::HeldProgram;

mod command;
mod fs;
pub(crate) use command::USAGE;
pub(crate) use command::command;
#[cfg(test)]
mod tests;

pub(crate) const SUFFIX: &str = ".engineering-sdk";
const ORIGINAL_RECORDS: &[&str] = &[
    "LINEAGE",
    "PROVENANCE",
    "PROVENANCE.build-status",
    "SEED-MINT",
];

fn invalid(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}
fn time(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}

/// Explicit input expectations, not a signature, source proof, ABI claim, or completed build.
pub struct Request {
    /// Existing base SDK, independently selected by its real package layout/build.
    pub base: crate::store::Layout,
    /// Numeric base SDK build (its compiler/driver/runtime origins remain unchanged).
    /// Bundled source namespaces are explicitly omitted, never supplied as source custody.
    pub base_build: u64,
    /// Exact independently built Targo image. Opened once; never executed by the producer.
    pub targo: PathBuf,
    /// Expected SHA-256 of that actual file, lowercase hexadecimal.
    pub targo_sha256: String,
    /// Explicit caller-supplied source expectation, separately labeled in the observation record.
    pub expected_targo_source: String,
    /// Fresh private prefix. Existing destinations refuse, including an existing canonical store.
    pub destination: PathBuf,
    /// Numeric private build address; no current link is created.
    pub build: u64,
}

/// A live constructed engineering candidate. Not a signed artifact, `CompletedRuntime`, SDK
/// Keeper, resource owner, or proof. The unchanged Host must capture its own genuine Keeper.
pub struct Candidate {
    layout: crate::store::Layout,
    build: u64,
    root: fs::Pin,
    shim_parent: fs::Pin,
    inventory: fs::Snapshot,
    ready: File,
    ready_stamp: fs::Stamp,
    programs: Vec<HeldProgram>,
    lease: crate::lease::Lease,
}
impl Candidate {
    /// The new private layout, for independent ordinary Host qualification.
    pub fn layout(&self) -> &crate::store::Layout {
        &self.layout
    }
    /// The exact independent payload directory, not a selection or execution capability.
    pub fn directory(&self) -> &Path {
        self.root.path()
    }
    /// Recheck actual original candidate and lease/input custody. No data constructs this owner.
    pub fn check_current(&self, deadline: Instant) -> io::Result<()> {
        time(deadline)?;
        self.lease.check_current()?;
        self.root.check()?;
        self.shim_parent.check_original_root()?;
        self.inventory.recheck(self.root.held(), deadline)?;
        if self.ready_stamp != fs::Stamp::of(&self.ready.metadata()?) {
            return Err(invalid("original engineering Ready changed"));
        }
        let marker = crate::store::ready_marker_path(&self.layout.build_dir("trust", self.build))
            .ok_or(io::ErrorKind::InvalidInput)?;
        let (named, _) = aterm_uds::regular_file::open(&marker)?;
        if self.ready_stamp != fs::Stamp::of(&named.metadata()?) {
            return Err(invalid("engineering Ready name changed"));
        }
        for program in &self.programs {
            program.validate(deadline)?;
        }
        Ok(())
    }
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(text: &str) -> io::Result<[u8; 32]> {
    if !hex(text, 64) {
        return Err(invalid("expected image digest is not canonical SHA-256"));
    }
    let mut out = [0; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16)
            .map_err(|_| invalid("invalid image digest"))?;
    }
    Ok(out)
}
fn hold(
    layout: &crate::store::Layout,
    build: u64,
    deadline: Instant,
) -> io::Result<crate::lease::Lease> {
    time(deadline)?;
    match crate::lease::hold_for(
        &layout.prefix,
        &layout.build_dir("trust", build),
        "engineering-sdk-construction",
        deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(30)),
    )
    .map_err(|why| invalid(format!("SDK input unavailable: {why:?}")))?
    {
        crate::lease::Hold::Held(lease) => {
            lease.check_current()?;
            Ok(lease)
        }
        other => Err(invalid(format!("actual SDK lease unavailable: {other:?}"))),
    }
}
fn mapped(path: &str) -> io::Result<String> {
    let root = path.split('/').next().unwrap_or("");
    if matches!(root, "bin" | "lib" | "libexec") {
        return Ok(path.into());
    }
    if ORIGINAL_RECORDS.contains(&path) {
        return Ok(format!("base-origin/{path}"));
    }
    Err(invalid(format!("unclassified base SDK entry {path}")))
}
fn executable<'a>(snapshot: &'a fs::Snapshot, path: &str) -> io::Result<&'a str> {
    match snapshot.entries.iter().find(|e| e.path() == path) {
        Some(fs::Entry::File { stamp, sha256, .. }) if stamp.mode() & 0o111 != 0 => Ok(sha256),
        _ => Err(invalid(format!(
            "base SDK lacks original regular executable {path}"
        ))),
    }
}
fn ready_input(layout: &crate::store::Layout, build: u64) -> io::Result<(File, fs::Stamp)> {
    let logical = layout.build_dir("trust", build);
    let marker = crate::store::ready_marker_path(&logical).ok_or(io::ErrorKind::InvalidInput)?;
    let (file, metadata) = aterm_uds::regular_file::open(&marker)?;
    crate::local_harness::fs::owned_file(&file)?;
    if metadata.len() > 4 << 20 {
        return Err(invalid("base Ready exceeds bound"));
    }
    let stamp = fs::Stamp::of(&metadata);
    let mut bytes = vec![0; metadata.len() as usize];
    file.read_exact_at(&mut bytes, 0)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid("base Ready is not UTF8"))?;
    // Positive observation, never build_is_complete's GC keep-on-unreadable semantics.
    if !text.starts_with("ok\n")
        || !text.lines().any(|s| {
            s == format!(
                "platform={}-{}",
                std::env::consts::ARCH,
                std::env::consts::OS
            )
        })
        || !crate::store::build_is_complete(&logical)
        || stamp != fs::Stamp::of(&file.metadata()?)
    {
        return Err(invalid(
            "base SDK has no positively readable native complete record",
        ));
    }
    Ok((file, stamp))
}

/// Construct a fresh private engineering SDK using native original-file COW clones.
///
/// The complete base inventory is captured and rechecked, internal aliases stay internal,
/// all materialized regular files have independent inodes, and compiler/driver load closures
/// are checked from actual originals before and after relocation. Targo must be a native
/// system-only image: this path does not waive compiler/frontend ABI or commit checks.
///
/// Failures before Ready retain an unready private attempt; there is no path-based cleanup
/// or retry into an existing destination. A post-Ready I/O/check failure may leave a complete
/// candidate, which must be independently inspected rather than retried into. A final Ready
/// is written only after construction and validation.
pub fn produce(request: Request, deadline: Instant) -> io::Result<Candidate> {
    time(deadline)?;
    let targo_expected = digest(&request.targo_sha256)?;
    if !hex(&request.expected_targo_source, 40)
        || crate::lease::Subject::build("trust", request.base_build).is_none()
        || crate::lease::Subject::build("trust", request.build).is_none()
        || deadline.saturating_duration_since(Instant::now()) > Duration::from_secs(3600)
    {
        return Err(invalid("invalid engineering construction scope/deadline"));
    }
    // A lexical prefix alias is not accepted as another way to address an immutable input:
    // disjointness below is against this independently held canonical component chain.
    let base_prefix = fs::Pin::open(&request.base.prefix)?;
    let base_lease = hold(&request.base, request.base_build, deadline)?;
    base_prefix.check()?;
    let (base_ready, base_ready_stamp) = ready_input(&request.base, request.base_build)?;
    let base_path = crate::lookup::payload_root(&request.base, "trust", request.base_build)
        .ok_or_else(|| invalid("base SDK has no exact managed physical tree"))?;
    let base = fs::Pin::open(&base_path)?;
    let before = fs::Snapshot::capture_runtime(base.held(), deadline)?;
    for entry in &before.entries {
        mapped(entry.path())?;
        if let Some(leaf) = entry.path().strip_prefix("bin/")
            && !leaf.contains('/')
            && leaf.contains(',')
        {
            return Err(invalid(
                "bin entry cannot be represented by the canonical Ready inventory",
            ));
        }
        if ORIGINAL_RECORDS.contains(&entry.path()) && !matches!(entry, fs::Entry::File { .. }) {
            return Err(invalid(
                "base SDK origin record must be a regular original file",
            ));
        }
    }
    let compiler_hash = executable(&before, "bin/trustc")?;
    let driver_hash = executable(&before, "bin/tippy-driver")?;
    executable(&before, "bin/targo")?;
    let base_programs = [
        HeldProgram::open(
            &base_path,
            &base_path.join("bin/trustc"),
            &digest(compiler_hash)?,
            deadline,
        )?,
        HeldProgram::open(
            &base_path,
            &base_path.join("bin/tippy-driver"),
            &digest(driver_hash)?,
            deadline,
        )?,
    ];
    let targo_parent = fs::Pin::open(request.targo.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
    let targo_name = request
        .targo
        .file_name()
        .ok_or(io::ErrorKind::InvalidInput)?;
    let targo = fs::regular(targo_parent.held(), targo_name)?;
    let targo_stamp = fs::Stamp::of(&targo.metadata()?);
    let (targo_bytes, targo_hash) = fs::hash(&targo, deadline)?;
    if targo_hash != request.targo_sha256 || targo_stamp.mode() & 0o111 == 0 {
        return Err(invalid(
            "replacement Targo bytes/executable expectation differs",
        ));
    }
    trust_resource_provenance::require_no_acl_grants(&targo)?;
    // One bounded actual-byte parse, no program execution/version echo or marker synthesis.
    if targo_bytes > 512 << 20 {
        return Err(invalid("Targo exceeds native image parser bound"));
    }
    let mut image = vec![0; targo_bytes as usize];
    targo.read_exact_at(&mut image, 0)?;
    let names = crate::macho::load_names(&image).map_err(|e| invalid(e.to_string()))?;
    if !crate::macho::is_native_executable(&image)
        || !names.rpaths.is_empty()
        || names
            .loads
            .iter()
            .any(|name| !crate::macho::is_system_dylib(name))
    {
        return Err(invalid(
            "replacement Targo is not a native system-only executable",
        ));
    }
    drop(image);

    // Fresh prefix is the API boundary: no operation accepts an existing canonical prefix.
    let destination_parent = fs::Pin::open(
        request
            .destination
            .parent()
            .ok_or(io::ErrorKind::InvalidInput)?,
    )?;
    disjoint_destination(
        &request.destination,
        base_prefix.path(),
        &base_path,
        targo_parent.path(),
    )?;
    let prefix_name = request
        .destination
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or(io::ErrorKind::InvalidInput)?;
    if prefix_name.is_empty() || prefix_name == "." || prefix_name == ".." {
        return Err(invalid("invalid fresh prefix name"));
    }
    destination_parent.check()?;
    let prefix = fs::mkdir(destination_parent.held(), prefix_name)?;
    let layout = crate::store::Layout {
        prefix: request.destination.clone(),
    };
    let prefix_pin = fs::Pin::open(&layout.prefix)?;
    if fs::Stamp::of(&prefix.metadata()?).identity()
        != fs::Stamp::of(&prefix_pin.held().metadata()?).identity()
    {
        return Err(invalid("fresh prefix replaced after creation"));
    }
    let lock = crate::lock::StoreLock::create_in_original(&layout, &prefix)?;
    let parent = crate::local_harness::fs::Parent::open_original(&lock, "trust", &prefix)?;
    let name = format!("{}.noindex", request.build);
    let incoming = format!(".{name}.engineering-{}", std::process::id());
    let stage = fs::mkdir(parent.held(), &incoming)?;
    fs::mkdir(&stage, "base-origin")?;
    for entry in &before.entries {
        time(deadline)?;
        base.check()?;
        parent.reprove()?;
        prefix_pin.check()?;
        let path = mapped(entry.path())?;
        let (output_dir, output_name) = fs::parent(&stage, &path)?;
        match entry {
            fs::Entry::Directory { .. } => {
                fs::mkdir(
                    &output_dir,
                    output_name.to_str().ok_or(io::ErrorKind::InvalidInput)?,
                )?;
            }
            fs::Entry::File {
                path: source_path,
                stamp,
                bytes,
                sha256,
            } => {
                let (source_dir, source_name) = fs::parent(base.held(), source_path)?;
                let original = fs::regular(&source_dir, &source_name)?;
                if *stamp != fs::Stamp::of(&original.metadata()?) {
                    return Err(invalid("base file changed before clone"));
                }
                let (selected, expected_hash, expected_size) = if source_path == "bin/targo" {
                    (&targo, &targo_hash, targo_bytes)
                } else {
                    (&original, sha256, *bytes)
                };
                trust_resource_provenance::require_no_acl_grants(selected)?;
                fs::clone_file(selected, &output_dir, &output_name)?;
                let copied = fs::regular(&output_dir, &output_name)?;
                aterm_dirfd::fchmod(
                    &copied,
                    Mode::from_bits(if source_path == "bin/targo" {
                        0o555
                    } else {
                        // The permission mask fits the native Mode's u16.
                        (stamp.mode() & 0o555) as u16
                    }),
                )
                .map_err(io::Error::from)?;
                let copied_stamp = fs::Stamp::of(&copied.metadata()?);
                if copied_stamp.identity() == fs::Stamp::of(&selected.metadata()?).identity()
                    || copied.metadata()?.nlink() != 1
                    || fs::hash(&copied, deadline)? != (expected_size, expected_hash.clone())
                {
                    return Err(invalid(
                        "materialized file is not an independent exact clone",
                    ));
                }
                crate::store::sync_contents_or_accept_refusal(&copied)?;
                fs::sync(&output_dir)?;
            }
            fs::Entry::Link { target, .. } => fs::link(
                &output_dir,
                output_name.to_str().ok_or(io::ErrorKind::InvalidInput)?,
                target,
            )?,
        }
    }
    base_lease.check_current()?;
    base.check_original_root()?;
    before.recheck(base.held(), deadline)?;
    for program in &base_programs {
        program.validate(deadline)?;
    }
    targo_parent.check_original_root()?;
    let (named_ready, named_ready_stamp) = ready_input(&request.base, request.base_build)?;
    let _named_ready = named_ready;
    if targo_stamp != fs::Stamp::of(&targo.metadata()?)
        || targo_stamp != fs::Stamp::of(&fs::regular(targo_parent.held(), targo_name)?.metadata()?)
        || base_ready_stamp != fs::Stamp::of(&base_ready.metadata()?)
        || base_ready_stamp != named_ready_stamp
        || fs::hash(&targo, deadline)? != (targo_bytes, targo_hash.clone())
    {
        return Err(invalid(
            "original engineering inputs changed during construction",
        ));
    }

    // Root metadata describing the old homogeneous SDK is preserved under base-origin,
    // never copied as a claim about this newly mixed-component engineering tree.
    let record = serde_record(
        &request,
        &base_path,
        &base_ready,
        &before,
        &targo_stamp,
        deadline,
    )?;
    fs::write_new(&stage, "ENGINEERING-SDK.json", &record)?;
    seal_directories(&stage, &before)?;
    let staged_inventory = fs::Snapshot::capture(&stage, deadline)?;
    check_materialized(
        &before,
        &staged_inventory,
        &targo_hash,
        targo_bytes,
        &record,
    )?;
    parent.reprove()?;
    prefix_pin.check()?;
    destination_parent.check()?;
    aterm_dirfd::renameat_with(
        parent.held(),
        &incoming,
        parent.held(),
        &name,
        aterm_dirfd::RenameFlags::NOREPLACE,
    )
    .map_err(io::Error::from)?;
    fs::link(parent.held(), &request.build.to_string(), &name)?;
    parent.sync()?;
    let physical = layout.prefix.join("store/trust").join(&name);
    let root = fs::Pin::open(&physical)?;
    if fs::Stamp::of(&stage.metadata()?).identity()
        != fs::Stamp::of(&root.held().metadata()?).identity()
    {
        return Err(invalid(
            "published engineering root is not the same original stage",
        ));
    }
    let published_inventory = fs::Snapshot::capture(root.held(), deadline)?;
    if staged_inventory.entries != published_inventory.entries {
        return Err(invalid("engineering contents changed across publication"));
    }
    check_materialized(
        &before,
        &published_inventory,
        &targo_hash,
        targo_bytes,
        &record,
    )?;
    let programs = vec![
        HeldProgram::open(
            &physical,
            &physical.join("bin/targo"),
            &targo_expected,
            deadline,
        )?,
        HeldProgram::open(
            &physical,
            &physical.join("bin/trustc"),
            &digest(compiler_hash)?,
            deadline,
        )?,
        HeldProgram::open(
            &physical,
            &physical.join("bin/tippy-driver"),
            &digest(driver_hash)?,
            deadline,
        )?,
    ];
    // Complete actual runtime validation precedes metadata publication. No recorded source
    // expectation, version label, marker or JSON record can substitute for these owners.
    for program in &programs {
        program.validate(deadline)?;
    }
    parent.reprove()?;
    fs::write_new(
        parent.held(),
        &format!("{}{SUFFIX}", request.build),
        &record,
    )?;
    let bin = fs::mkdir(&prefix, "bin")?;
    fs::link(
        &bin,
        "targo",
        &format!("../store/trust/{}/bin/targo", request.build),
    )?;
    fs::sync(&bin)?;
    let mut names = before
        .entries
        .iter()
        .filter_map(|entry| {
            let path = entry.path();
            let leaf = path.strip_prefix("bin/")?;
            (!leaf.contains('/')).then_some(leaf)
        })
        .collect::<Vec<_>>();
    names.sort();
    let names = names.join(",");
    if names.is_empty() {
        return Err(invalid("SDK has no bin inventory"));
    }
    let ready_body = crate::store::ready_body_with_inventory(Some(&format!("bin:{names}")));
    time(deadline)?;
    parent.reprove()?;
    prefix_pin.check()?;
    for program in &programs {
        program.validate(deadline)?;
    }
    published_inventory.recheck(root.held(), deadline)?;
    base_lease.check_current()?;
    base.check_original_root()?;
    before.recheck(base.held(), deadline)?;
    targo_parent.check_original_root()?;
    if targo_stamp != fs::Stamp::of(&targo.metadata()?) {
        return Err(invalid("replacement Targo drift before Ready"));
    }
    // Ready lives beside, outside, the payload snapshot. Retain its actual completion
    // observation through the final relocation/validation bookend too.
    let (_named_ready, final_ready_stamp) = ready_input(&request.base, request.base_build)?;
    if base_ready_stamp != final_ready_stamp
        || base_ready_stamp != fs::Stamp::of(&base_ready.metadata()?)
    {
        return Err(invalid(
            "original base Ready changed before engineering publication",
        ));
    }
    fs::write_new(
        parent.held(),
        &format!("{}.ready", request.build),
        ready_body.as_bytes(),
    )?;
    parent.sync()?;
    let lease = hold(&layout, request.build, deadline)?;
    let (ready, ready_stamp) = ready_input(&layout, request.build)?;
    let shim_parent = fs::Pin::open(&layout.bin_dir())?;
    let candidate = Candidate {
        layout,
        build: request.build,
        root,
        shim_parent,
        inventory: published_inventory,
        ready,
        ready_stamp,
        programs,
        lease,
    };
    candidate.check_current(deadline)?;
    Ok(candidate)
}

fn disjoint_destination(
    destination: &Path,
    base_prefix: &Path,
    base_tree: &Path,
    image_parent: &Path,
) -> io::Result<()> {
    for input in [base_prefix, base_tree, image_parent] {
        if destination.starts_with(input) || input.starts_with(destination) {
            return Err(invalid(
                "engineering destination overlaps an original input/store namespace",
            ));
        }
    }
    Ok(())
}

fn check_materialized(
    base: &fs::Snapshot,
    actual: &fs::Snapshot,
    targo_hash: &str,
    targo_bytes: u64,
    record: &[u8],
) -> io::Result<()> {
    if actual.entries.len() != base.entries.len() + 2 {
        return Err(invalid(
            "materialized inventory has extra or missing entries",
        ));
    }
    let by_path = actual
        .entries
        .iter()
        .map(|entry| (entry.path(), entry))
        .collect::<std::collections::BTreeMap<_, _>>();
    for source in &base.entries {
        let path = mapped(source.path())?;
        let copied = by_path
            .get(path.as_str())
            .copied()
            .ok_or_else(|| invalid("materialized entry missing"))?;
        let same = match (source, copied) {
            (fs::Entry::Directory { .. }, fs::Entry::Directory { stamp, .. }) => {
                stamp.mode() == 0o555
            }
            (fs::Entry::Link { target, .. }, fs::Entry::Link { target: copied, .. }) => {
                target == copied
            }
            (
                fs::Entry::File {
                    path: source,
                    stamp,
                    bytes,
                    sha256,
                    ..
                },
                fs::Entry::File {
                    stamp: copy,
                    bytes: n,
                    sha256: hash,
                    ..
                },
            ) => {
                let replacement = source == "bin/targo";
                copy.identity() != stamp.identity()
                    && *n == if replacement { targo_bytes } else { *bytes }
                    && hash == if replacement { targo_hash } else { sha256 }
                    && copy.mode()
                        == if replacement {
                            0o555
                        } else {
                            stamp.mode() & 0o555
                        }
            }
            _ => false,
        };
        if !same {
            return Err(invalid(format!("materialized entry differs: {path}")));
        }
    }
    if !actual.entries.iter().any(|entry|matches!(entry,fs::Entry::Directory{path,stamp} if path=="base-origin" && stamp.mode()==0o555))
        || !actual.entries.iter().any(|entry|matches!(entry,fs::Entry::File{path,bytes,sha256,..} if path=="ENGINEERING-SDK.json" && *bytes==record.len() as u64 && *sha256==crate::tree::sha256_bytes(record)))
    { return Err(invalid("materialized engineering origin record differs")); }
    Ok(())
}

fn seal_directories(root: &File, snapshot: &fs::Snapshot) -> io::Result<()> {
    for entry in snapshot.entries.iter().rev() {
        if matches!(entry, fs::Entry::Directory { .. }) {
            let (parent, name) = fs::parent(root, &mapped(entry.path())?)?;
            let directory = fs::directory(&parent, &name)?;
            aterm_dirfd::fchmod(&directory, Mode::from_bits(0o555)).map_err(io::Error::from)?;
            fs::sync(&directory)?;
        }
    }
    let origin = fs::directory(root, std::ffi::OsStr::new("base-origin"))?;
    aterm_dirfd::fchmod(&origin, Mode::from_bits(0o555)).map_err(io::Error::from)?;
    fs::sync(&origin)?;
    aterm_dirfd::fchmod(root, Mode::from_bits(0o555)).map_err(io::Error::from)?;
    fs::sync(root)
}
fn serde_record(
    request: &Request,
    base: &Path,
    ready: &File,
    snapshot: &fs::Snapshot,
    targo_stamp: &fs::Stamp,
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    let record = aterm_json::json!({
        "schema":"atpkg.engineering-sdk.v1",
        "scope":"local runtime-only engineering construction observations; no bundled source checkout, signed release, homogeneous source, build completion, ABI, resource or proof claim",
        "base":{"prefix":request.base.prefix,"build":request.base_build,"physical_root":base,
            "ready_sha256_observation":fs::hash(ready,deadline)?.1,"original_entries":snapshot.entries,
            "omitted_source_namespace_entry_observations":snapshot.omitted_source_namespaces},
        "replacement":{"path":"bin/targo","input":request.targo,"sha256":request.targo_sha256,
            "expected_source_commit_supplied_by_caller":request.expected_targo_source,"original_stamp":targo_stamp},
        "component_origins":"bin/targo alone uses replacement; all other files/aliases retain base byte origins; original root records relocated into base-origin/",
        "build":request.build,
    });
    let bytes = aterm_json::to_vec(&record).map_err(|why| invalid(why.to_string()))?;
    if bytes.len() > 16 << 20 {
        return Err(invalid("engineering inventory record exceeds 16MiB"));
    }
    Ok(bytes)
}
