// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-protocol row admission, the system-satisfaction probe, and the PATH-shadow probe.
//!
//! A `protocol = "https"` artifact row is a roster-signed manifest row that pins a
//! VENDOR's own download (`url`) by `sha256` + `size` + `tree_root`. The client downloads
//! straight from the vendor, verifies against the signed digests, and shims the result
//! like a plain binary ([`crate::dispatch::ApplyStrategy::Shim`]) or lays its `.app`
//! ([`crate::dispatch::ApplyStrategy::VendorApp`]). Nothing third-party is ever mirrored
//! onto the index owner's hosting — that is the whole point of the lane (Claude Code's
//! license forbids redistribution; Codex's does not need it).
//!
//! A `protocol = "pkg"` row pins a Developer-ID-signed macOS installer package the OS
//! applies with elevation ([`crate::installer_pkg`]); a `protocol = "softwareupdate"` row
//! names an Apple `softwareupdate` label (the Command Line Tools,
//! [`crate::softwareupdate`]); a `protocol = "system-pm"` row names a package one of the
//! managers in [`MANAGER_TABLE`] resolves ([`crate::system_pm`]). None lands bytes in
//! the store; all prove themselves by the row's `provides`.
//!
//! Trust stays where it is today: the signed row is the authority, and the vendor host is
//! a transport exactly as `alabsystems` is (§8 "the host is never an authenticity input").
//! [`check_row`] is DEFENSE IN DEPTH over that signed data — it narrows where a signed row
//! may point ([`VENDOR_HOSTS`]) and refuses shapes the stage lanes could mis-stage — so a
//! compromised publisher key still cannot aim clients at an arbitrary host, and a
//! malformed row fails BEFORE any byte moves rather than after a multi-hundred-MB download.
//!
//! [`system_satisfied`] is the other half of the owner's decision for `gh`/`emacs`: a
//! default-set member declaring `system = "<bin>"` is SATISFIED by a binary of that name
//! already on the user's `PATH` — outside the managed `bin/` and the store — and is
//! fetched only when no such install exists. [`shadowing_binary_on_path`] is its mirror
//! for MANAGED members: a binary of an exposed name that precedes the managed `bin/` on
//! `PATH` is what actually runs, and the canonical `managed <build> — SHADOWED by <path>`
//! state says so (a warning, never a fault, never "fixed" — the user owns `PATH`).
//! [`executable_on_path`] is the raw walk both share, for the one caller that must find a
//! name the shim deny-list refuses: the `system-pm` lane looking for `cargo` itself.
//!
//! The walk is CROSS-PLATFORM: `PATH` splits on the platform separator
//! (`std::env::split_paths` — `:` on Unix, `;` on Windows), only ABSOLUTE entries count
//! (a Windows entry needs its drive), and on Windows a bare name resolves through
//! `PATHEXT` exactly as `cmd.exe` resolves it ([`windows_lookup_names`]: `gh` tries
//! `gh.COM`, `gh.EXE`, `gh.BAT`, `gh.CMD`, …; `gh.exe` is tried as spelled).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::flow::FlowError;
use crate::manifest::{Artifact, Program};
use crate::store::ToolName;

/// The hosts a signed `https`/`pkg` row may download from. EXACT host match — no ports,
/// no userinfo, no subdomain wildcards — over an `https://` URL.
///
/// Defense in depth, not the trust root: the signed row is the authority; this list only
/// narrows where a signed row may point, so a publisher-key compromise cannot redirect
/// clients to an arbitrary host. Bump it in a client release when a vendor moves.
pub const VENDOR_HOSTS: &[&str] = &[
    "downloads.claude.ai",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "releases.openai.com",
    "emacsformacosx.com",
];

/// The staging lanes an `https` row may name in `payload`. The stage side
/// (`install::verify_and_stage`) implements each; a row naming anything else is refused
/// here, before download. `dmg` is the `kind = "app-bundle"` lane; the other four are
/// `kind = "binary"`.
pub const PAYLOADS: &[&str] = &["raw-binary", "tar-gz", "tar-zst", "zip", "dmg"];

/// The archive payloads — the only ones `strip_components` applies to.
const ARCHIVE_PAYLOADS: &[&str] = &["tar-gz", "tar-zst", "zip"];

/// One row of the EXTENSIBLE manager table a `system-pm` row may name in `manager`.
/// atpkg is a META package manager: adding `npm`, `uv`, … later is ONE more row here —
/// the admission rule, the install argv, the binary the lane looks for on `PATH` and the
/// elevation default all read the table ([`crate::system_pm`] is the lane).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Manager {
    /// The spelling a signed row uses (`manager = "apt"`).
    pub name: &'static str,
    /// The install argv template. Its FIRST element is the manager's binary as looked up
    /// on `PATH` ([`Manager::binary`]: `apt-get` for `apt`); the ONE `{}` element is
    /// replaced by the row's `package`, as a single argument, never a shell word; an
    /// optional `{scope}` element becomes `user` or `machine` from the row's `elevated`
    /// ([`Manager::install_argv`]) — winget's install scope, the one manager that spells
    /// the user/system split as a flag rather than as `sudo`.
    pub install: &'static [&'static str],
    /// Whether installs are system-wide and therefore need elevation — a row naming such
    /// a manager must declare `elevated = true`, so the unattended pass knows to wait for
    /// the explicit door instead of failing every six hours. `false` ⇒ user-scoped.
    pub elevated: bool,
    /// The non-alphanumeric ASCII bytes a package id may carry in this manager's naming
    /// (alphanumerics are always admitted); everything else is refused at admission.
    /// The SHAPE rules every manager shares sit in [`Manager::id_ok`].
    pub id_chars: &'static str,
    /// The file suffixes this manager reads as "install THIS LOCAL FILE (or URL)" rather
    /// than as a package name — `brew install evil.rb`, `scoop install evil.json`,
    /// `dnf install evil.rpm`, `pipx install evil.whl` — compared case-insensitively
    /// against the id's tail and refused at admission: a signed row names a package the
    /// manager RESOLVES, never a file the pass's working directory happens to hold.
    pub local_file: &'static [&'static str],
    /// How the row proves the install, for the authoring side and the docs: what a
    /// `provides` entry conventionally names.
    pub provides: &'static str,
}

impl Manager {
    /// The binary the lane looks for on `PATH` — the template's first element (`apt-get`,
    /// `brew`, `winget`, …). Absent ⇒ the member is `unavailable on <target>` here;
    /// atpkg never installs a manager.
    #[must_use]
    pub fn binary(&self) -> &'static str {
        self.install.first().copied().unwrap_or("")
    }

    /// `install` with `{}` replaced by `package` and `{scope}` by `machine` when the row
    /// declares `elevated = true`, else `user` — the argv [`crate::system_pm`] runs (its
    /// first element re-spelled as the manager's RESOLVED absolute path there).
    #[must_use]
    pub fn install_argv(&self, package: &str, elevated: bool) -> Vec<String> {
        self.install
            .iter()
            .map(|a| match *a {
                "{}" => package.to_string(),
                "{scope}" => String::from(if elevated { "machine" } else { "user" }),
                other => other.to_string(),
            })
            .collect()
    }

    /// Whether `package` is spelled in this manager's own charset AND in the shape a
    /// package NAME has, as opposed to the other things a manager's `install` verb
    /// accepts and a charset alone lets through:
    ///
    /// * every byte alphanumeric or in [`Manager::id_chars`];
    /// * the FIRST byte alphanumeric and the LAST alphanumeric or `+` — `apt-get install
    ///   emacs-` REMOVES emacs (a trailing `-` names nothing in any manager), a leading
    ///   `.` or `/` is a path; `+` may close a name because `g++` and `gcc-c++` are
    ///   packages, and apt's `foo+` install-marker spelling installs `foo` either way;
    /// * no `/`-separated segment empty, `.` or `..`, and each starting alphanumeric —
    ///   `../evil.rb` through brew's tap-path charset is a file, not a formula;
    /// * not ending in one of [`Manager::local_file`]'s suffixes — the manager would
    ///   load a local file (or fetch a URL) instead of resolving a name.
    ///
    /// [`package_id_ok`] (non-empty, printable, not `-`-led) runs before this and is the
    /// one-argument rule; this is the per-manager rule.
    #[must_use]
    pub fn id_ok(&self, package: &str) -> bool {
        let bytes = package.as_bytes();
        let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else {
            return false;
        };
        if !first.is_ascii_alphanumeric() || !(last.is_ascii_alphanumeric() || *last == b'+') {
            return false;
        }
        if !bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || self.id_chars.as_bytes().contains(b))
        {
            return false;
        }
        if !package.split('/').all(|seg| {
            seg.as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
                && seg != "."
                && seg != ".."
        }) {
            return false;
        }
        let lower = package.to_ascii_lowercase();
        !self.local_file.iter().any(|suffix| lower.ends_with(suffix))
    }
}

/// The manager table: the OS managers, plus the first non-OS entries (`cargo`, `pipx`),
/// which are user-scoped. [`MANAGERS`] is its name column, kept in step by a test.
pub const MANAGER_TABLE: &[Manager] = &[
    Manager {
        name: "apt",
        install: &["apt-get", "install", "-y", "{}"],
        elevated: true,
        id_chars: ".+-",
        local_file: &[".deb"],
        provides: "the binary the .deb installs, by bare name or /usr/bin path",
    },
    Manager {
        name: "dnf",
        install: &["dnf", "install", "-y", "{}"],
        elevated: true,
        id_chars: ".+-_",
        local_file: &[".rpm"],
        provides: "the binary the .rpm installs, by bare name or /usr/bin path",
    },
    Manager {
        name: "brew",
        install: &["brew", "install", "{}"],
        elevated: false,
        id_chars: "._@/+-",
        local_file: &[".rb"],
        provides: "the formula's or cask's binary, by bare name",
    },
    Manager {
        name: "winget",
        // `--scope user` by default (no UAC: portable/user installers land under
        // %LOCALAPPDATA%); a row that declares `elevated = true` names a package whose
        // only installer is machine-scoped (GNU.Emacs' nullsoft installer) and asks for
        // `--scope machine` — winget raises the UAC prompt itself, so the unattended
        // pass defers such a row exactly as it defers apt.
        install: &[
            "winget",
            "install",
            "--exact",
            "--id",
            "{}",
            "--accept-package-agreements",
            "--accept-source-agreements",
            "--scope",
            "{scope}",
        ],
        elevated: false,
        id_chars: "._-",
        local_file: &[],
        provides: "the installed binary, by bare name",
    },
    Manager {
        name: "scoop",
        install: &["scoop", "install", "{}"],
        elevated: false,
        id_chars: "._/-",
        local_file: &[".json"],
        provides: "the manifest's bin, by bare name",
    },
    Manager {
        name: "cargo",
        install: &["cargo", "install", "{}"],
        elevated: false,
        id_chars: "_-",
        local_file: &[],
        provides: "the crate's binary under ~/.cargo/bin, by bare name",
    },
    Manager {
        name: "pipx",
        install: &["pipx", "install", "{}"],
        elevated: false,
        id_chars: "._-",
        local_file: &[".whl", ".zip", ".tar.gz", ".tgz"],
        provides: "the package's console script under ~/.local/bin, by bare name",
    },
];

/// The package managers a `system-pm` row may name in `manager` — the name column of
/// [`MANAGER_TABLE`], spelled out so the docs and the tooling can pin it.
pub const MANAGERS: &[&str] = &["apt", "dnf", "brew", "winget", "scoop", "cargo", "pipx"];

/// The table row for `name`, if the table carries one.
#[must_use]
pub fn manager(name: &str) -> Option<&'static Manager> {
    MANAGER_TABLE.iter().find(|m| m.name == name)
}

/// `a | b | c` over the table's names — the admission message's spelling.
fn manager_names() -> String {
    MANAGER_TABLE
        .iter()
        .map(|m| m.name)
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Whether `payload` names an archive lane (see [`ARCHIVE_PAYLOADS`]).
#[must_use]
pub fn is_archive_payload(payload: &str) -> bool {
    ARCHIVE_PAYLOADS.contains(&payload)
}

/// The host of an `https://` URL, or `None` when the URL is not admissible at all: a
/// non-`https` scheme, an empty host, a port, userinfo (`user@host`), or any whitespace /
/// control byte anywhere in the URL (curl gets the URL after a literal `--`, but a row
/// that needs quoting has no business being signed).
#[must_use]
pub fn https_host(url: &str) -> Option<&str> {
    if url.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return None;
    }
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    if host.is_empty()
        || host.contains('@')
        || host.contains(':')
        || host.contains('[')
        || host.contains(']')
    {
        return None;
    }
    Some(host)
}

/// Whether `url` is `https://` on an exactly-allow-listed vendor host.
#[must_use]
pub fn url_allowed(url: &str) -> bool {
    https_host(url).is_some_and(|h| VENDOR_HOSTS.contains(&h))
}

/// Whether `s` is a 64-character lowercase-or-uppercase hex SHA-256 spelling.
fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether `s` is ONE bare file-name component: non-empty, not `.`/`..`, no `/`, no `\`
/// (the Windows separator), no control byte (NUL, newline, DEL, …). The row's `asset` is
/// joined onto the program's staging directory as the download's local name, so it must
/// never be a path — and never carry a newline: `pkgutil --check-signature` echoes the
/// file's path VERBATIM in its `Package "<path>":` header, so a name with a newline in
/// it could forge a `Certificate Chain:` for the parser to read. Refused here, before
/// any byte moves; the lane refuses such a path a second time at spawn.
fn bare_file_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains('/')
        && !s.contains('\\')
        && !s.bytes().any(|b| b < b' ' || b == 0x7f)
}

/// The roots an absolute `provides` path may NEVER lie under: a path that exists on
/// every machine (`/etc/passwd`, `/dev/null`, the kernel's `/proc`, `/sys`) or one under
/// a user's home (a dotfile, a store copy, the managed prefix itself) proves no OS
/// install — a signed row naming one would mark an uninstalled member `installed via
/// <protocol>` and the door would never install it. An OS installer (`pkg`,
/// `softwareupdate`) or a platform manager lays what it installs under `/opt`, `/usr`,
/// `/Library`, `/Applications`, … — none of these. (The managed prefix is excluded a
/// second time at probe time, wherever it lives: [`under_managed_prefix`].)
///
/// The world-writable temporary roots (`/tmp`, `/var/tmp`, macOS's `/var/folders`) are
/// deliberately NOT listed: the flow's end-to-end tests prove real installs under the
/// process temp dir (`/var/folders/…` on macOS, `/tmp` on Linux), and a signed row is
/// already the authority that decides what runs — a `provides` under `/tmp` could only
/// make an uninstalled member READ as installed, never run anything. Refusing them at
/// admission is a follow-up that needs a test seam first (see the stage-D notes).
pub const PROVIDES_NEVER: &[&str] = &[
    "/etc/",
    "/private/etc/",
    "/dev/",
    "/proc/",
    "/sys/",
    "/home/",
    "/Users//",
    "/root/",
];

/// Whether an absolute `provides` entry is admissible: [`absolute_path_ok`] AND not
/// under one of [`PROVIDES_NEVER`].
fn provided_path_ok(s: &str) -> bool {
    absolute_path_ok(s) && !PROVIDES_NEVER.iter().any(|r| s.starts_with(r))
}

/// Whether `path` lies under the managed `prefix`, as spelled or as the filesystem
/// resolves either of them — the `provides` probe's exclusion: nothing under atpkg's own
/// prefix (a shim, a store copy) ever proves an OS install.
#[must_use]
pub fn under_managed_prefix(prefix: &Path, path: &Path) -> bool {
    let prefix_real = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    under_prefix(prefix, &prefix_real, path)
        || std::fs::canonicalize(path).is_ok_and(|real| under_prefix(prefix, &prefix_real, &real))
}

/// Whether a `links` TARGET is admissible: a relative path inside the staged tree —
/// non-empty, no leading `/`, no `..`, no `.`/empty components, no NUL, no `\` (the
/// Windows separator would let `..\` dodge the component check), only `Normal`
/// components.
fn link_target_ok(target: &str) -> bool {
    if target.is_empty()
        || target.starts_with('/')
        || target.contains('\0')
        || target.contains('\\')
        || target.ends_with('/')
    {
        return false;
    }
    // Segment by segment on the RAW string, not `Path::components()`: that iterator
    // normalizes an interior `.` away (`a/./b` reads as `a/b`), and a target the client
    // would create verbatim must be judged verbatim.
    target
        .split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        && Path::new(target)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Whether `s` is an Apple Developer ID team identifier: exactly ten ASCII
/// uppercase-alphanumeric characters (`927JGANW46`).
fn is_team_id(s: &str) -> bool {
    s.len() == 10
        && s.bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
}

/// Whether `s` is a package id a manager can be handed as ONE argument: non-empty, no
/// whitespace or control bytes, and not beginning with `-` (which every manager would read
/// as a flag).
fn package_id_ok(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && !s.bytes().any(|b| b <= b' ' || b == 0x7f)
}

/// Whether a `provides` entry is admissible as an absolute path: `/`-rooted, no NUL, no
/// `..` component, no trailing `/`.
fn absolute_path_ok(s: &str) -> bool {
    s.starts_with('/')
        && !s.contains('\0')
        && !s.ends_with('/')
        && s.split('/')
            .skip(1)
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// Refuse `msg` as a [`FlowError::VendorRefused`].
fn refuse(msg: &str) -> FlowError {
    FlowError::VendorRefused(msg.to_string())
}

/// Refuse with a `<head><detail>` message (manual concat — see `lib.rs` on `format!`).
fn refuse2(head: &str, detail: &str) -> FlowError {
    let mut m = String::from(head);
    m.push_str(detail);
    FlowError::VendorRefused(m)
}

/// Admit an artifact row BEFORE any byte moves, by its `protocol`:
///
/// * `github-release` — unchanged: nothing to check here (the release lane's gates are
///   the signed `sha256`/`tree_root` at stage time, exactly as before the split);
/// * `https` — every refusal the vendor lane has always had: https on an allow-listed
///   bare host, a known `payload` that matches the `kind` (`dmg` ⇔ `app-bundle`), a
///   shimmable exposed `entry` for `raw-binary`, `strip_components` on archives only,
///   `links` keys exposed and targets in-tree, a bare `asset` name, `size > 0`, a 64-hex
///   `sha256`, a non-empty `tree_root`;
/// * `pkg` — [`check_pkg_row`]: https on an allow-listed host, a 64-hex `sha256`,
///   `size > 0`, a ten-character `signer_team`, `elevated = true`, absolute `provides`,
///   and NONE of the store-shaped keys (`tree_root`, `payload`, `entry`, `links`,
///   `strip_components`) — nothing lands in the store;
/// * `system-pm` — [`check_system_pm_row`]: a `manager` the table carries, a one-argument
///   `package` in that manager's charset, `provides` as tool names or absolute paths,
///   `elevated = true` for a system-wide manager, and NO byte-shaped keys (`url`,
///   `sha256`, `size`, `tree_root`, `asset`, `payload`);
/// * `softwareupdate` — [`check_softwareupdate_row`]: `kind = "system-package"`, a
///   printable non-empty `label_prefix`, `elevated = true`, absolute `provides`, and NO
///   byte-shaped keys;
/// * anything else — refused by name (dispatch would call it Unknown anyway; this names
///   the field).
///
/// `exposes` is the manifest's signed `exposes` list — the raw-binary `entry` and every
/// `links` key must be in it, because those are the names the stage will put under
/// `bin/` and the shims will resolve.
///
/// Every refusal is a [`FlowError::VendorRefused`] naming the field, so a mis-authored row
/// fails fast on the authoring machine's own `atpkg install` and never on a user's pass.
///
/// # Errors
/// The first field that fails admission, in the order listed on the struct.
pub fn check_row(artifact: &Artifact, exposes: &[String]) -> Result<(), FlowError> {
    match artifact.protocol.as_str() {
        "github-release" => Ok(()),
        "https" => check_https_row(artifact, exposes),
        "pkg" => check_pkg_row(artifact),
        "system-pm" => check_system_pm_row(artifact),
        "softwareupdate" => check_softwareupdate_row(artifact),
        other => Err(refuse2(
            "protocol must be github-release | https | pkg | system-pm | softwareupdate, got ",
            other,
        )),
    }
}

/// The `https` half of [`check_row`] — every refusal the vendor lane has always carried.
fn check_https_row(artifact: &Artifact, exposes: &[String]) -> Result<(), FlowError> {
    // 0. The kind this protocol can carry: a binary (four payload lanes) or a vendor
    //    `.app` (the dmg lane). Anything else is a shape the stage cannot lay down.
    match artifact.kind.as_str() {
        "binary" | "app-bundle" => {}
        other => {
            return Err(refuse2(
                "an https row's kind must be binary | app-bundle, got ",
                other,
            ));
        }
    }
    // 1. URL: https, exact allow-listed host, no port/userinfo/control bytes.
    let Some(host) = https_host(&artifact.url) else {
        return Err(refuse2(
            "url must be https:// on a bare vendor host (no port, no userinfo): ",
            &artifact.url,
        ));
    };
    if !VENDOR_HOSTS.contains(&host) {
        return Err(refuse2(
            "url host is not an allow-listed vendor host: ",
            host,
        ));
    }
    // 2. Payload lane, and it must agree with the kind: `dmg` is how an app-bundle
    //    arrives and the only way one may; a binary never arrives as a disk image.
    if !PAYLOADS.contains(&artifact.payload.as_str()) {
        return Err(refuse2(
            "payload must be raw-binary | tar-gz | tar-zst | zip | dmg, got ",
            &artifact.payload,
        ));
    }
    if artifact.kind == "app-bundle" && artifact.payload != "dmg" {
        return Err(refuse2(
            "kind = app-bundle over https needs payload = dmg, got ",
            &artifact.payload,
        ));
    }
    if artifact.kind == "binary" && artifact.payload == "dmg" {
        return Err(refuse(
            "payload = dmg is the app-bundle lane; a binary row cannot name it",
        ));
    }
    // 2b. `asset` is the LOCAL staging file name the download lands under
    //     (`staging_dir(program).join(asset)`), never a release asset and never a path.
    if !bare_file_name(&artifact.asset) {
        return Err(refuse2(
            "asset must be a bare local file name (no separators, not `.`/`..`): ",
            &artifact.asset,
        ));
    }
    // 3. raw-binary needs an entry that is a shimmable name AND an exposed one; the
    //    other lanes must not carry one (a stray entry would be a silent authoring slip).
    if artifact.payload == "raw-binary" {
        if artifact.entry.is_empty() {
            return Err(refuse("raw-binary payload needs a non-empty entry"));
        }
        if ToolName::new(&artifact.entry).is_none() {
            return Err(refuse2(
                "entry is not an admissible tool name: ",
                &artifact.entry,
            ));
        }
        if !exposes.iter().any(|e| e == &artifact.entry) {
            return Err(refuse2("entry is not in exposes: ", &artifact.entry));
        }
    } else if !artifact.entry.is_empty() {
        return Err(refuse2(
            "entry is only meaningful for raw-binary, but payload is ",
            &artifact.payload,
        ));
    }
    // 4. strip_components only for archives.
    if artifact.strip_components != 0 && !is_archive_payload(&artifact.payload) {
        return Err(refuse2(
            "strip_components applies to archive payloads only, not ",
            &artifact.payload,
        ));
    }
    // 5. links: every key exposed + shimmable, every target relative and `..`-free.
    check_links(&artifact.links, exposes)?;
    // 6. The signed digests must be present and well-formed — the extracted tree is the
    //    only thing the client can re-verify at apply time, so an empty root is a refusal
    //    for THIS protocol even though a loose release manifest tolerates one.
    if artifact.size == 0 {
        return Err(refuse("size must be > 0 (it is the exact download cap)"));
    }
    if !is_sha256_hex(&artifact.sha256) {
        return Err(refuse2(
            "sha256 must be 64 hex characters, got ",
            &artifact.sha256,
        ));
    }
    if artifact.tree_root.is_empty() {
        return Err(refuse("tree_root is required for an https row"));
    }
    // 7. The other protocols' keys have no meaning here and are refused rather than
    //    silently ignored — a `manager` on an https row is an authoring slip.
    if !artifact.signer_team.is_empty()
        || !artifact.manager.is_empty()
        || !artifact.package.is_empty()
        || !artifact.label_prefix.is_empty()
    {
        return Err(refuse(
            "signer_team / manager / package / label_prefix belong to the pkg, system-pm and \
             softwareupdate protocols, not https",
        ));
    }
    Ok(())
}

/// The `pkg` half of [`check_row`]: a Developer-ID-signed macOS installer package.
fn check_pkg_row(artifact: &Artifact) -> Result<(), FlowError> {
    if artifact.kind != "installer-pkg" {
        return Err(refuse2(
            "a pkg row's kind must be installer-pkg, got ",
            &artifact.kind,
        ));
    }
    let Some(host) = https_host(&artifact.url) else {
        return Err(refuse2(
            "url must be https:// on a bare vendor host (no port, no userinfo): ",
            &artifact.url,
        ));
    };
    if !VENDOR_HOSTS.contains(&host) {
        return Err(refuse2(
            "url host is not an allow-listed vendor host: ",
            host,
        ));
    }
    if !artifact.asset.is_empty() && !bare_file_name(&artifact.asset) {
        return Err(refuse2(
            "asset must be a bare local file name (no separators, not `.`/`..`): ",
            &artifact.asset,
        ));
    }
    if artifact.size == 0 {
        return Err(refuse("size must be > 0 (it is the exact download cap)"));
    }
    if !is_sha256_hex(&artifact.sha256) {
        return Err(refuse2(
            "sha256 must be 64 hex characters, got ",
            &artifact.sha256,
        ));
    }
    if !is_team_id(&artifact.signer_team) {
        return Err(refuse2(
            "signer_team must be the signer's ten-character Apple Developer ID team, got ",
            &artifact.signer_team,
        ));
    }
    if !artifact.elevated {
        return Err(refuse(
            "a pkg row must declare elevated = true (the installer runs as root)",
        ));
    }
    if artifact.provides.is_empty() {
        return Err(refuse(
            "a pkg row needs provides = [<absolute path>, …] to prove the install",
        ));
    }
    for p in &artifact.provides {
        if !provided_path_ok(p) {
            return Err(refuse2(
                "a pkg row's provides entries must be absolute, `..`-free paths outside \
                 /etc, /dev, /proc, /sys and every home directory, got ",
                p,
            ));
        }
    }
    if !artifact.tree_root.is_empty()
        || !artifact.payload.is_empty()
        || !artifact.entry.is_empty()
        || !artifact.links.is_empty()
        || artifact.strip_components != 0
    {
        return Err(refuse(
            "tree_root / payload / entry / links / strip_components have no meaning for a \
             pkg row (nothing lands in the store)",
        ));
    }
    if !artifact.manager.is_empty()
        || !artifact.package.is_empty()
        || !artifact.label_prefix.is_empty()
    {
        return Err(refuse(
            "manager / package / label_prefix belong to the system-pm and softwareupdate \
             protocols, not pkg",
        ));
    }
    Ok(())
}

/// The `system-pm` half of [`check_row`]: a package the platform's own manager resolves.
fn check_system_pm_row(artifact: &Artifact) -> Result<(), FlowError> {
    if artifact.kind != "system-package" {
        return Err(refuse2(
            "a system-pm row's kind must be system-package, got ",
            &artifact.kind,
        ));
    }
    let Some(mgr) = manager(&artifact.manager) else {
        let mut head = String::from("manager must be ");
        head.push_str(&manager_names());
        head.push_str(", got ");
        return Err(refuse2(&head, &artifact.manager));
    };
    if !package_id_ok(&artifact.package) {
        return Err(refuse2(
            "package must be one bare manager argument (no whitespace, not `-`-led), got ",
            &artifact.package,
        ));
    }
    if !mgr.id_ok(&artifact.package) {
        let mut head = String::from("package is not spelled in ");
        head.push_str(mgr.name);
        head.push_str("'s package-id charset (alphanumerics and `");
        head.push_str(mgr.id_chars);
        head.push_str("`, alphanumeric first and last, no `.`/`..` path segment, not a local file");
        for suffix in mgr.local_file {
            head.push(' ');
            head.push_str(suffix);
        }
        head.push_str("): ");
        return Err(refuse2(&head, &artifact.package));
    }
    if artifact.provides.is_empty() {
        return Err(refuse(
            "a system-pm row needs provides = [<tool name or absolute path>, …] to prove the install",
        ));
    }
    for p in &artifact.provides {
        if ToolName::new(p).is_none() && !provided_path_ok(p) {
            return Err(refuse2(
                "a system-pm row's provides entries must be tool names or absolute paths \
                 outside /etc, /dev, /proc, /sys and every home directory, got ",
                p,
            ));
        }
    }
    if mgr.elevated && !artifact.elevated {
        return Err(refuse2(
            "elevated = true is required for a system-wide manager: ",
            &artifact.manager,
        ));
    }
    if carries_bytes(artifact) {
        return Err(refuse(
            "a system-pm row moves no bytes: url / sha256 / size / tree_root / asset / \
             payload / entry / links / strip_components must be absent",
        ));
    }
    if !artifact.signer_team.is_empty() || !artifact.label_prefix.is_empty() {
        return Err(refuse(
            "signer_team / label_prefix belong to the pkg and softwareupdate protocols, not \
             system-pm",
        ));
    }
    Ok(())
}

/// The `softwareupdate` half of [`check_row`]: an Apple `softwareupdate` label — the
/// Command Line Tools — installed by the OS with elevation and proven by absolute paths.
fn check_softwareupdate_row(artifact: &Artifact) -> Result<(), FlowError> {
    if artifact.kind != "system-package" {
        return Err(refuse2(
            "a softwareupdate row's kind must be system-package, got ",
            &artifact.kind,
        ));
    }
    if artifact.label_prefix.is_empty()
        || artifact.label_prefix.bytes().any(|b| b < b' ' || b == 0x7f)
        || artifact.label_prefix.starts_with('-')
    {
        return Err(refuse2(
            "label_prefix must be a non-empty printable label head (`Command Line Tools for \
             Xcode`), got ",
            &artifact.label_prefix,
        ));
    }
    if !artifact.elevated {
        return Err(refuse(
            "a softwareupdate row must declare elevated = true (softwareupdate -i runs as root)",
        ));
    }
    if artifact.provides.is_empty() {
        return Err(refuse(
            "a softwareupdate row needs provides = [<absolute path>, …] to prove the install",
        ));
    }
    for p in &artifact.provides {
        if !provided_path_ok(p) {
            return Err(refuse2(
                "a softwareupdate row's provides entries must be absolute, `..`-free paths \
                 outside /etc, /dev, /proc, /sys and every home directory, got ",
                p,
            ));
        }
    }
    if carries_bytes(artifact) {
        return Err(refuse(
            "a softwareupdate row moves no bytes: url / sha256 / size / tree_root / asset / \
             payload / entry / links / strip_components must be absent",
        ));
    }
    if !artifact.signer_team.is_empty()
        || !artifact.manager.is_empty()
        || !artifact.package.is_empty()
    {
        return Err(refuse(
            "signer_team / manager / package belong to the pkg and system-pm protocols, not \
             softwareupdate",
        ));
    }
    Ok(())
}

/// Whether the row carries any of the byte-shaped keys — the refusal every byte-less
/// protocol (`system-pm`, `softwareupdate`) shares.
fn carries_bytes(artifact: &Artifact) -> bool {
    !artifact.url.is_empty()
        || !artifact.sha256.is_empty()
        || artifact.size != 0
        || !artifact.tree_root.is_empty()
        || !artifact.asset.is_empty()
        || !artifact.payload.is_empty()
        || !artifact.entry.is_empty()
        || !artifact.links.is_empty()
        || artifact.strip_components != 0
}

/// The `links` half of the https check, split so the map rules read on their own.
fn check_links(links: &BTreeMap<String, String>, exposes: &[String]) -> Result<(), FlowError> {
    for (name, target) in links {
        if ToolName::new(name).is_none() {
            return Err(refuse2("links key is not an admissible tool name: ", name));
        }
        if !exposes.iter().any(|e| e == name) {
            return Err(refuse2("links key is not in exposes: ", name));
        }
        if !link_target_ok(target) {
            return Err(refuse2(
                "links target must be a relative, `..`-free path inside the staged tree: ",
                target,
            ));
        }
    }
    Ok(())
}

/// Whether `path` lies under the managed `prefix` — spelled as given OR as the
/// filesystem resolves it (`prefix_real`, the canonical prefix: on macOS a temp prefix
/// under `/var/folders` resolves to `/private/var/folders`, and a symlinked prefix
/// resolves elsewhere entirely). Anything under the prefix is atpkg's own — the `bin/`
/// shims, the store trees — never a SYSTEM install.
fn under_prefix(prefix: &Path, prefix_real: &Path, path: &Path) -> bool {
    path.starts_with(prefix) || path.starts_with(prefix_real)
}

/// Whether `path` is a runnable regular file (following symlinks — a Homebrew `gh` is a
/// symlink into the Cellar). On Unix at least one execute bit must be set.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// What a `PATH` walk should do at a directory the managed prefix owns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AtManaged {
    /// Skip it and keep walking — the system probe: a managed copy never satisfies.
    Skip,
    /// Stop: everything after the managed `bin/` loses to the managed copy — the shadow
    /// probe.
    Stop,
}

/// The Windows `PATHEXT` a lookup falls back to when the variable is unset or empty —
/// `cmd.exe`'s own default.
pub const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// The file names a Windows `PATH` lookup of `name` tries in ONE directory, in order —
/// `cmd.exe`'s rule: a name that already ends in an extension `pathext` lists (`gh.exe`,
/// `scoop.cmd`, compared case-insensitively) is tried as spelled and nothing else; a bare
/// name (`gh`) tries `name` + each `pathext` entry in `pathext`'s order (`gh.COM`,
/// `gh.EXE`, `gh.BAT`, `gh.CMD`). An empty `pathext` reads as [`DEFAULT_PATHEXT`];
/// entries that do not start with `.` are ignored. A bare file with NO extension never
/// matches on Windows — it is not executable there. Pure, so the rule is unit-tested on
/// every platform; the Windows walk feeds it the process's `PATHEXT`.
#[must_use]
pub fn windows_lookup_names(name: &str, pathext: &str) -> Vec<String> {
    let source = if pathext.trim().is_empty() {
        DEFAULT_PATHEXT
    } else {
        pathext
    };
    let exts: Vec<&str> = source
        .split(';')
        .map(str::trim)
        .filter(|e| e.len() > 1 && e.starts_with('.'))
        .collect();
    if let Some((_, ext)) = name.rsplit_once('.')
        && !ext.is_empty()
        && exts.iter().any(|e| e[1..].eq_ignore_ascii_case(ext))
    {
        return vec![name.to_string()];
    }
    exts.iter()
        .map(|e| {
            let mut s = String::from(name);
            s.push_str(e);
            s
        })
        .collect()
}

/// The paths a lookup of `bin` tries under `dir`: on Windows the `PATHEXT` spellings
/// ([`windows_lookup_names`] over the process's `PATHEXT`), elsewhere `dir/bin` alone.
fn lookup_candidates(dir: &Path, bin: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let pathext = std::env::var("PATHEXT").unwrap_or_default();
        windows_lookup_names(bin, &pathext)
            .into_iter()
            .map(|n| dir.join(n))
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![dir.join(bin)]
    }
}

/// The shared `PATH` walk behind [`system_binary_on_path`], [`shadowing_binary_on_path`]
/// and [`executable_on_path`]: the first executable named `bin` in an ABSOLUTE `PATH`
/// entry outside the managed `prefix`, whose resolved path is also outside it. `at_managed`
/// decides what a prefix-owned entry does to the walk. `bin` must be ONE bare file name
/// (no separator, not `.`/`..`) — it is joined onto every entry.
fn first_foreign_on_path(
    prefix: &Path,
    bin: &str,
    path_var: Option<&OsStr>,
    at_managed: AtManaged,
) -> Option<PathBuf> {
    if !bare_file_name(bin) {
        return None;
    }
    let path_var = path_var?;
    let prefix_real = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    // `split_paths` is the platform's own rule: `:` on Unix, `;` on Windows (where a
    // quoted entry is unquoted too).
    for dir in std::env::split_paths(path_var) {
        // A RELATIVE entry (`.`, `bin`, the empty string) resolves against whatever the
        // current directory happens to be: a `gh` dropped into a project checkout must
        // never read as a system install — and never retire the managed copy. Absolute
        // directories only (on Windows that means a drive-rooted one).
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        // The entry as spelled OR as it resolves (`~/bin → <prefix>/bin` is the managed
        // bin/ under another name): atpkg's own directory either way.
        let managed_dir = under_prefix(prefix, &prefix_real, &dir)
            || std::fs::canonicalize(&dir)
                .is_ok_and(|real| under_prefix(prefix, &prefix_real, &real));
        if managed_dir {
            match at_managed {
                AtManaged::Skip => continue,
                AtManaged::Stop => return None,
            }
        }
        for candidate in lookup_candidates(&dir, bin) {
            if !is_executable_file(&candidate) {
                continue;
            }
            // Follow the hit to where it really lives: a user's own symlink to a store
            // copy is still atpkg's copy — it never satisfies (Skip), and for the shadow
            // probe it is the managed copy RUNNING, so nothing after it shadows (Stop).
            if std::fs::canonicalize(&candidate)
                .is_ok_and(|real| under_prefix(prefix, &prefix_real, &real))
            {
                match at_managed {
                    AtManaged::Skip => continue,
                    AtManaged::Stop => return None,
                }
            }
            return Some(candidate);
        }
    }
    None
}

/// Probe `path_var` (a `PATH` value) for an executable named `bin`, skipping every
/// RELATIVE entry (it names the current directory, not a system) and every directory the
/// managed `prefix` owns (its `bin/` shims, its store) — and skipping a hit whose RESOLVED
/// path lands inside the prefix (a user's own symlink to a store copy is still atpkg's
/// copy). The first remaining hit wins, in `PATH` order.
///
/// The name must be a single admissible component ([`ToolName`]): a `system` value with a
/// separator is refused outright rather than joined onto every `PATH` entry, and a name
/// the shim deny-list refuses (`git`, `cargo`, …) never satisfies a member — see
/// [`executable_on_path`] for the one caller that needs such a name.
#[must_use]
pub fn system_binary_on_path(
    prefix: &Path,
    bin: &str,
    path_var: Option<&OsStr>,
) -> Option<PathBuf> {
    ToolName::new(bin)?;
    first_foreign_on_path(prefix, bin, path_var, AtManaged::Skip)
}

/// The RAW form of [`system_binary_on_path`]: the same walk (absolute entries only, the
/// managed prefix skipped, store-resolving hits skipped, `PATHEXT` on Windows) for any
/// bare file name, WITHOUT the [`ToolName`] deny-list. For the `system-pm` lane's
/// manager lookup only: `cargo` is (rightly) a name no shim may take, and it is also a
/// package manager the table names. Never a satisfaction or shadow probe — those keep
/// the deny-list, so a `git` on `PATH` still satisfies nothing.
#[must_use]
pub fn executable_on_path(prefix: &Path, name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    first_foreign_on_path(prefix, name, path_var, AtManaged::Skip)
}

/// The binary that SHADOWS a managed tool named `bin`: the first executable of that name
/// in an absolute `PATH` entry BEFORE the managed `bin/` (or anywhere, when the managed
/// `bin/` is not on `PATH` at all — inside an aterm session it is appended LAST, so every
/// foreign hit precedes it), resolving outside the prefix. `None` when the managed copy is
/// the one that runs. Same relative-entry and resolve-into-store rules as
/// [`system_binary_on_path`].
#[must_use]
pub fn shadowing_binary_on_path(
    prefix: &Path,
    bin: &str,
    path_var: Option<&OsStr>,
) -> Option<PathBuf> {
    ToolName::new(bin)?;
    first_foreign_on_path(prefix, bin, path_var, AtManaged::Stop)
}

/// Whether `program` is SATISFIED by a system install: it declares `system = "<bin>"`
/// and that binary is on this process's `PATH` outside the managed `prefix`. `Some(path)`
/// names the install; `None` means the program is managed here (no `system` key, or
/// nothing on `PATH`).
#[must_use]
pub fn system_satisfied(prefix: &Path, program: &Program) -> Option<PathBuf> {
    let bin = program.system.as_deref()?;
    system_binary_on_path(prefix, bin, std::env::var_os("PATH").as_deref())
}

#[cfg(test)]
pub(crate) mod testkit {
    //! The reference rows, one per protocol — shared with the lanes' tests.

    use std::collections::BTreeMap;

    use crate::manifest::{Artifact, Cost};

    /// The reference `https` row: Claude Code's raw binary.
    pub fn row() -> Artifact {
        Artifact {
            target: "aarch64-apple-darwin".into(),
            kind: "binary".into(),
            protocol: "https".into(),
            asset: "claude-2.1.231-darwin-arm64".into(),
            sha256: "7b09f01c".repeat(8),
            tree_root: "abc".into(),
            size: 230_824_016,
            reloc: "self-contained".into(),
            cost: Cost::default(),
            url: "https://downloads.claude.ai/claude-code-releases/2.1.231/darwin-arm64/claude"
                .into(),
            payload: "raw-binary".into(),
            entry: "claude".into(),
            strip_components: 0,
            links: BTreeMap::new(),
            vendor: "Anthropic PBC".into(),
            signer_team: String::new(),
            elevated: false,
            provides: vec![],
            manager: String::new(),
            package: String::new(),
            label_prefix: String::new(),
        }
    }

    /// The reference `softwareupdate` row: Apple's Command Line Tools.
    pub fn su_row() -> Artifact {
        let mut a = pm_row();
        a.target = "aarch64-apple-darwin".into();
        a.protocol = "softwareupdate".into();
        a.manager = String::new();
        a.package = String::new();
        a.label_prefix = "Command Line Tools for Xcode".into();
        a.provides = vec!["/Library/Developer/CommandLineTools/usr/bin/git".into()];
        a.elevated = true;
        a.vendor = "Apple".into();
        a
    }

    /// The reference `pkg` row: Homebrew's signed installer package.
    pub fn pkg_row() -> Artifact {
        let mut a = row();
        a.kind = "installer-pkg".into();
        a.protocol = "pkg".into();
        a.asset = String::new();
        a.tree_root = String::new();
        a.url =
            "https://github.com/Homebrew/brew/releases/download/4.5.0/Homebrew-4.5.0.pkg".into();
        a.payload = String::new();
        a.entry = String::new();
        a.vendor = "Homebrew".into();
        a.signer_team = "927JGANW46".into();
        a.elevated = true;
        a.provides = vec!["/opt/homebrew/bin/brew".into()];
        a
    }

    /// The reference `system-pm` row: Emacs through apt.
    pub fn pm_row() -> Artifact {
        let mut a = row();
        a.target = "x86_64-unknown-linux-gnu".into();
        a.kind = "system-package".into();
        a.protocol = "system-pm".into();
        a.asset = String::new();
        a.sha256 = String::new();
        a.tree_root = String::new();
        a.size = 0;
        a.url = String::new();
        a.payload = String::new();
        a.entry = String::new();
        a.vendor = String::new();
        a.manager = "apt".into();
        a.package = "emacs".into();
        a.provides = vec!["emacs".into(), "/usr/bin/emacs".into()];
        a.elevated = true;
        a
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::{pkg_row, pm_row, row, su_row};
    use super::*;

    fn exposes() -> Vec<String> {
        vec!["claude".to_string()]
    }

    fn refused(a: &Artifact, exposes: &[String]) -> String {
        match check_row(a, exposes) {
            Err(FlowError::VendorRefused(m)) => m,
            other => panic!("expected VendorRefused, got {other:?}"),
        }
    }

    #[test]
    fn a_well_formed_raw_binary_row_is_admitted() {
        check_row(&row(), &exposes()).expect("the reference row admits");
    }

    /// A `github-release` row is admitted UNCHANGED — no vendor checks apply to the
    /// release lane, whatever its other keys hold (they are ignored there, as before).
    #[test]
    fn a_github_release_row_is_unchanged_by_admission() {
        let mut a = row();
        a.protocol = "github-release".into();
        a.url = String::new();
        a.payload = String::new();
        a.entry = String::new();
        a.tree_root = String::new();
        a.size = 0;
        a.sha256 = "d".into();
        check_row(&a, &[]).expect("the release lane has no vendor admission");
        // …even with the retired-shape keys hanging off it.
        a.url = "http://evil.example/x".into();
        check_row(&a, &[]).expect("release rows ignore url");
    }

    #[test]
    fn an_unknown_protocol_is_refused_by_name() {
        let mut a = row();
        a.protocol = "ftp".into();
        assert!(refused(&a, &exposes()).contains("protocol must be"));
        a.protocol = String::new();
        assert!(refused(&a, &exposes()).contains("protocol must be"));
        a.protocol = "HTTPS".into();
        assert!(refused(&a, &exposes()).contains("protocol must be"));
    }

    #[test]
    fn a_well_formed_archive_row_with_links_is_admitted() {
        let mut a = row();
        a.url =
            "https://github.com/cli/cli/releases/download/v2.80.0/gh_2.80.0_macOS_arm64.zip".into();
        a.payload = "zip".into();
        a.entry = String::new();
        a.strip_components = 1;
        a.links.insert("gh".into(), "share/gh/bin/gh".into());
        check_row(&a, &["gh".to_string()]).expect("archive row admits");
        let mut d = row();
        d.kind = "app-bundle".into();
        d.url = "https://emacsformacosx.com/emacs-builds/Emacs-30.1-universal.dmg".into();
        d.payload = "dmg".into();
        d.entry = String::new();
        d.links
            .insert("emacs".into(), "Emacs.app/Contents/MacOS/Emacs".into());
        d.links.insert(
            "emacsclient".into(),
            "Emacs.app/Contents/MacOS/bin/emacsclient".into(),
        );
        check_row(&d, &["emacs".to_string(), "emacsclient".to_string()]).expect("dmg row admits");
    }

    /// The kind ⇔ payload pairing over https: an app-bundle arrives ONLY as a dmg, a
    /// binary never does, and no other kind rides this protocol.
    #[test]
    fn https_kind_and_payload_must_agree() {
        let mut app_as_zip = row();
        app_as_zip.kind = "app-bundle".into();
        app_as_zip.payload = "zip".into();
        app_as_zip.entry = String::new();
        assert!(refused(&app_as_zip, &exposes()).contains("needs payload = dmg"));
        let mut bin_as_dmg = row();
        bin_as_dmg.payload = "dmg".into();
        bin_as_dmg.entry = String::new();
        assert!(refused(&bin_as_dmg, &exposes()).contains("app-bundle lane"));
        for other in [
            "sysroot-bundle",
            "cargo-src",
            "installer-pkg",
            "system-package",
            "",
            "vendor-fetch",
        ] {
            let mut a = row();
            a.kind = other.into();
            assert!(
                refused(&a, &exposes()).contains("kind must be binary | app-bundle"),
                "{other:?}"
            );
        }
        // The old payload spelling is gone with the kind it belonged to.
        let mut old = row();
        old.kind = "app-bundle".into();
        old.payload = "dmg-app".into();
        old.entry = String::new();
        assert!(refused(&old, &exposes()).contains("payload must be"));
    }

    #[test]
    fn refuses_non_https_and_hostile_urls() {
        for bad in [
            "http://downloads.claude.ai/x",
            "ftp://downloads.claude.ai/x",
            "file:///etc/passwd",
            "downloads.claude.ai/x",
            "",
            "https://",
            "https:///x",
            "https://downloads.claude.ai:443/x",
            "https://user@downloads.claude.ai/x",
            "https://user:pw@downloads.claude.ai/x",
            "https://[::1]/x",
            "https://downloads.claude.ai/x y",
            "https://downloads.claude.ai/x\n",
            "https://downloads.claude.ai/x\u{7f}",
        ] {
            let mut a = row();
            a.url = bad.into();
            let m = refused(&a, &exposes());
            assert!(m.contains("url must be https"), "{bad:?}: {m}");
            let mut p = pkg_row();
            p.url = bad.into();
            assert!(
                refused(&p, &[]).contains("url must be https"),
                "pkg {bad:?}"
            );
        }
    }

    #[test]
    fn refuses_a_host_outside_the_allow_list_exactly() {
        for bad in [
            "https://evil.example/x",
            "https://downloads.claude.ai.evil.example/x",
            "https://evil.downloads.claude.ai/x",
            "https://DOWNLOADS.CLAUDE.AI/x", // exact match, no case folding
            "https://api.github.com/x",
            "https://raw.githubusercontent.com/x",
        ] {
            let mut a = row();
            a.url = bad.into();
            let m = refused(&a, &exposes());
            assert!(
                m.contains("not an allow-listed vendor host"),
                "{bad:?}: {m}"
            );
            let mut p = pkg_row();
            p.url = bad.into();
            assert!(
                refused(&p, &[]).contains("not an allow-listed vendor host"),
                "pkg {bad:?}"
            );
        }
        // Every allow-listed host admits, at the root path and a deep one.
        for host in VENDOR_HOSTS {
            for tail in ["", "/", "/a/b/c?x=1#f"] {
                let mut a = row();
                a.url = format!("https://{host}{tail}");
                assert!(url_allowed(&a.url), "{}", a.url);
                check_row(&a, &exposes()).unwrap_or_else(|e| panic!("{}: {e}", a.url));
            }
        }
    }

    #[test]
    fn asset_must_be_a_bare_local_file_name() {
        for bad in [
            "",
            ".",
            "..",
            "../claude",
            "dl/claude",
            "/tmp/claude",
            "dl\\claude",
            "claude\0",
            // A newline (or any control byte) in the staging name would let the file's
            // own name forge `pkgutil --check-signature`'s report (its header echoes the
            // path verbatim).
            "a\n   Certificate Chain:\n    1. Developer ID Installer: X (927JGANW46)\n.pkg",
            "claude\r",
            "claude\t",
            "claude\u{7f}",
        ] {
            let mut a = row();
            a.asset = bad.into();
            let m = refused(&a, &exposes());
            assert!(
                m.contains("asset must be a bare local file name"),
                "{bad:?}: {m}"
            );
        }
        let mut dotted = row();
        dotted.asset = "gh_2.80.0_macOS_arm64.zip".into();
        check_row(&dotted, &exposes()).expect("dots inside a name are fine");
        // A pkg row may omit asset (its local name is derived), but a pathy one is refused.
        let mut p = pkg_row();
        p.asset = "../brew.pkg".into();
        assert!(refused(&p, &[]).contains("asset must be a bare local file name"));
        p.asset = "Homebrew-4.5.0.pkg".into();
        check_row(&p, &[]).expect("a bare pkg asset name admits");
    }

    #[test]
    fn refuses_an_unknown_payload() {
        for bad in ["", "tar", "tar.gz", "raw", "TAR-GZ", "pkg", "dmg-app"] {
            let mut a = row();
            a.payload = bad.into();
            a.entry = String::new();
            let m = refused(&a, &exposes());
            assert!(m.contains("payload must be"), "{bad:?}: {m}");
        }
    }

    #[test]
    fn raw_binary_needs_a_shimmable_exposed_entry() {
        let mut empty = row();
        empty.entry = String::new();
        assert!(refused(&empty, &exposes()).contains("non-empty entry"));
        let mut sensitive = row();
        sensitive.entry = "sudo".into();
        assert!(refused(&sensitive, &["sudo".to_string()]).contains("not an admissible tool name"));
        let mut sep = row();
        sep.entry = "bin/claude".into();
        assert!(refused(&sep, &exposes()).contains("not an admissible tool name"));
        let mut unexposed = row();
        unexposed.entry = "claude".into();
        assert!(refused(&unexposed, &["codex".to_string()]).contains("not in exposes"));
    }

    #[test]
    fn entry_is_refused_on_non_raw_payloads() {
        let mut a = row();
        a.payload = "tar-gz".into();
        a.entry = "claude".into();
        assert!(refused(&a, &exposes()).contains("only meaningful for raw-binary"));
    }

    #[test]
    fn strip_components_only_on_archives() {
        let mut raw = row();
        raw.strip_components = 1;
        assert!(refused(&raw, &exposes()).contains("strip_components applies to archive"));
        let mut dmg = row();
        dmg.kind = "app-bundle".into();
        dmg.payload = "dmg".into();
        dmg.entry = String::new();
        dmg.strip_components = 2;
        assert!(refused(&dmg, &exposes()).contains("strip_components applies to archive"));
        for ok in ["tar-gz", "tar-zst", "zip"] {
            let mut a = row();
            a.payload = ok.into();
            a.entry = String::new();
            a.strip_components = 3;
            check_row(&a, &exposes()).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
    }

    #[test]
    fn links_keys_must_be_exposed_shimmable_names() {
        let mut a = row();
        a.kind = "app-bundle".into();
        a.payload = "dmg".into();
        a.entry = String::new();
        a.links
            .insert("emacs".into(), "Emacs.app/Contents/MacOS/Emacs".into());
        assert!(refused(&a, &["claude".to_string()]).contains("links key is not in exposes"));
        let mut s = row();
        s.kind = "app-bundle".into();
        s.payload = "dmg".into();
        s.entry = String::new();
        s.links.insert("git".into(), "Emacs.app/git".into());
        assert!(
            refused(&s, &["git".to_string()]).contains("links key is not an admissible tool name")
        );
    }

    #[test]
    fn links_targets_must_be_relative_and_dotdot_free() {
        for bad in [
            "/Applications/Emacs.app/Contents/MacOS/Emacs",
            "../outside",
            "Emacs.app/../../etc/passwd",
            "Emacs.app/./Contents",
            "",
            "Emacs.app/Contents/",
            "Emacs.app\\Contents\\MacOS\\Emacs",
            "a\0b",
        ] {
            let mut a = row();
            a.kind = "app-bundle".into();
            a.payload = "dmg".into();
            a.entry = String::new();
            a.links.insert("claude".into(), bad.into());
            let m = refused(&a, &exposes());
            assert!(m.contains("links target must be"), "{bad:?}: {m}");
        }
    }

    #[test]
    fn signed_digests_must_be_present_and_well_formed() {
        let mut zero = row();
        zero.size = 0;
        assert!(refused(&zero, &exposes()).contains("size must be > 0"));
        for bad in [
            "",
            "deadbeef",
            &"z".repeat(64),
            &"a".repeat(63),
            &"a".repeat(65),
        ] {
            let mut a = row();
            a.sha256 = bad.to_string();
            assert!(
                refused(&a, &exposes()).contains("sha256 must be 64 hex"),
                "{bad:?}"
            );
            let mut p = pkg_row();
            p.sha256 = bad.to_string();
            assert!(
                refused(&p, &[]).contains("sha256 must be 64 hex"),
                "pkg {bad:?}"
            );
        }
        let mut upper = row();
        upper.sha256 = "7B09F01C".repeat(8);
        check_row(&upper, &exposes()).expect("hex case is not a refusal");
        let mut root = row();
        root.tree_root = String::new();
        assert!(refused(&root, &exposes()).contains("tree_root is required"));
    }

    /// Keys that belong to another protocol are refused on an https row, not ignored.
    #[test]
    fn foreign_protocol_keys_are_refused_on_an_https_row() {
        let mut a = row();
        a.manager = "brew".into();
        assert!(
            refused(&a, &exposes()).contains("belong to the pkg, system-pm and softwareupdate")
        );
        let mut b = row();
        b.signer_team = "927JGANW46".into();
        assert!(
            refused(&b, &exposes()).contains("belong to the pkg, system-pm and softwareupdate")
        );
        let mut c = row();
        c.label_prefix = "Command Line Tools for Xcode".into();
        assert!(
            refused(&c, &exposes()).contains("belong to the pkg, system-pm and softwareupdate")
        );
    }

    // ---- the pkg protocol ----

    #[test]
    fn a_well_formed_pkg_row_is_admitted_and_each_field_is_checked() {
        check_row(&pkg_row(), &[]).expect("the reference pkg row admits");
        let mut kind = pkg_row();
        kind.kind = "binary".into();
        assert!(refused(&kind, &[]).contains("kind must be installer-pkg"));
        let mut zero = pkg_row();
        zero.size = 0;
        assert!(refused(&zero, &[]).contains("size must be > 0"));
        for bad in ["", "927JGANW4", "927JGANW466", "927jganw46", "927JGANW-6"] {
            let mut t = pkg_row();
            t.signer_team = bad.into();
            assert!(refused(&t, &[]).contains("signer_team must be"), "{bad:?}");
        }
        let mut unelevated = pkg_row();
        unelevated.elevated = false;
        assert!(refused(&unelevated, &[]).contains("elevated = true"));
        let mut none = pkg_row();
        none.provides.clear();
        assert!(refused(&none, &[]).contains("needs provides"));
        for bad in [
            "brew",
            "opt/homebrew/bin/brew",
            "/opt/../bin/brew",
            "/opt/homebrew/bin/",
            "/a\0b",
            // A root that can never prove an install: a path that always exists, or one
            // under the user's home (where the managed prefix lives).
            "/etc/passwd",
            "/private/etc/hosts",
            "/dev/null",
            "/proc/self/exe",
            "/Users//me/.aterm/pkg/bin/brew",
            "/home/me/.local/bin/brew",
            "/root/brew",
        ] {
            let mut p = pkg_row();
            p.provides = vec![bad.into()];
            assert!(
                refused(&p, &[]).contains("provides entries must be absolute"),
                "{bad:?}"
            );
        }
        for good in [
            "/opt/homebrew/bin/brew",
            "/usr/local/bin/brew",
            "/Applications/Emacs.app/Contents/MacOS/Emacs",
        ] {
            let mut p = pkg_row();
            p.provides = vec![good.into()];
            check_row(&p, &[]).unwrap_or_else(|e| panic!("{good}: {e}"));
        }
        let mut tree = pkg_row();
        tree.tree_root = "abc".into();
        assert!(refused(&tree, &[]).contains("nothing lands in the store"));
        let mut payload = pkg_row();
        payload.payload = "raw-binary".into();
        assert!(refused(&payload, &[]).contains("nothing lands in the store"));
        let mut mgr = pkg_row();
        mgr.manager = "brew".into();
        assert!(refused(&mgr, &[]).contains("belong to the system-pm and softwareupdate"));
        let mut label = pkg_row();
        label.label_prefix = "Command Line Tools for Xcode".into();
        assert!(refused(&label, &[]).contains("belong to the system-pm and softwareupdate"));
    }

    // ---- the softwareupdate protocol ----

    #[test]
    fn a_well_formed_softwareupdate_row_is_admitted_and_each_field_is_checked() {
        check_row(&su_row(), &[]).expect("the reference Command Line Tools row admits");
        let mut kind = su_row();
        kind.kind = "installer-pkg".into();
        assert!(refused(&kind, &[]).contains("kind must be system-package"));
        for bad in ["", "-i", "Command Line\nTools", "x\u{7f}"] {
            let mut a = su_row();
            a.label_prefix = bad.into();
            assert!(refused(&a, &[]).contains("label_prefix must be"), "{bad:?}");
        }
        let mut unelevated = su_row();
        unelevated.elevated = false;
        assert!(refused(&unelevated, &[]).contains("elevated = true"));
        let mut none = su_row();
        none.provides.clear();
        assert!(refused(&none, &[]).contains("needs provides"));
        // A bare tool name is NOT a proof here: a `git` anywhere else on PATH must never
        // satisfy the Command Line Tools — only the absolute path under /Library does.
        for bad in [
            "git",
            "usr/bin/git",
            "/Library/../etc",
            "/a/",
            "/etc/hosts",
            "/Users//me/git",
        ] {
            let mut a = su_row();
            a.provides = vec![bad.into()];
            assert!(
                refused(&a, &[]).contains("provides entries must be absolute"),
                "{bad:?}"
            );
        }
        let mut url = su_row();
        url.url = "https://github.com/x".into();
        assert!(refused(&url, &[]).contains("moves no bytes"));
        let mut sha = su_row();
        sha.sha256 = "7b09f01c".repeat(8);
        assert!(refused(&sha, &[]).contains("moves no bytes"));
        let mut mgr = su_row();
        mgr.manager = "brew".into();
        assert!(refused(&mgr, &[]).contains("belong to the pkg and system-pm"));
        let mut team = su_row();
        team.signer_team = "927JGANW46".into();
        assert!(refused(&team, &[]).contains("belong to the pkg and system-pm"));
    }

    // ---- the manager table ----

    /// The table is the authority and `MANAGERS` its name column; the install argv puts
    /// the package id in as ONE argument, its first element is the binary the lane looks
    /// for, and `{scope}` (winget alone) follows the row's `elevated`; the first non-OS
    /// entries are user-scoped.
    #[test]
    fn the_manager_table_is_extensible_and_its_name_column_is_pinned() {
        let names: Vec<&str> = MANAGER_TABLE.iter().map(|m| m.name).collect();
        assert_eq!(names, MANAGERS.to_vec());
        for m in MANAGER_TABLE {
            assert_eq!(
                m.install.iter().filter(|a| **a == "{}").count(),
                1,
                "{}: exactly one package slot",
                m.name
            );
            assert!(manager(m.name).is_some_and(|x| x == m));
            assert!(
                bare_file_name(m.binary()) && !m.binary().starts_with('{'),
                "{}: the template's first element is the binary, got {:?}",
                m.name,
                m.binary()
            );
            assert_eq!(m.install_argv("x", false)[0], m.binary());
        }
        assert_eq!(manager("apt").unwrap().binary(), "apt-get");
        assert_eq!(manager("brew").unwrap().binary(), "brew");
        assert_eq!(
            manager("apt").unwrap().install_argv("emacs", true),
            vec!["apt-get", "install", "-y", "emacs"]
        );
        assert_eq!(
            manager("winget").unwrap().install_argv("GNU.Emacs", false),
            vec![
                "winget",
                "install",
                "--exact",
                "--id",
                "GNU.Emacs",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--scope",
                "user"
            ]
        );
        assert_eq!(
            manager("winget").unwrap().install_argv("GNU.Emacs", true)[8],
            "machine",
            "an elevated winget row asks for the machine scope"
        );
        assert_eq!(
            manager("cargo").unwrap().install_argv("ripgrep", false),
            vec!["cargo", "install", "ripgrep"]
        );
        assert_eq!(
            manager("pipx").unwrap().install_argv("black", false),
            vec!["pipx", "install", "black"]
        );
        assert_eq!(
            manager("apt").unwrap().install_argv("emacs", false),
            manager("apt").unwrap().install_argv("emacs", true),
            "only a template with a {{scope}} slot reads elevated"
        );
        assert!(manager("apt").unwrap().elevated && manager("dnf").unwrap().elevated);
        for user in ["brew", "winget", "scoop", "cargo", "pipx"] {
            assert!(!manager(user).unwrap().elevated, "{user} is user-scoped");
        }
        assert!(manager("npm").is_none(), "not a row yet — one row away");
        // The per-manager charset: a brew tap path admits, an apt id with a slash does not.
        assert!(
            manager("brew")
                .unwrap()
                .id_ok("d12frosted/emacs-plus/emacs-plus@30")
        );
        assert!(!manager("apt").unwrap().id_ok("d12frosted/emacs-plus"));
        let mut a = pm_row();
        a.package = "emacs/plus".into();
        assert!(refused(&a, &[]).contains("package-id charset"));
        // THE SHAPE RULES, per manager — every one of these is inside the charset and
        // every one of them means something other than "resolve this package name":
        // apt's `pkg-` REMOVES; a `.rb` / `.json` / `.rpm` / `.deb` / `.whl` is a
        // LOCAL FILE the manager would load from the working directory; `..`/`.`-led
        // segments are paths; a leading/trailing separator is never a name.
        for (mgr, bad) in [
            ("apt", "emacs-"),
            ("apt", "evil.deb"),
            ("apt", ".emacs"),
            ("dnf", "evil.rpm"),
            ("dnf", "EVIL.RPM"),
            ("dnf", "_x"),
            ("brew", "../evil.rb"),
            ("brew", "evil.rb"),
            ("brew", "./gh"),
            ("brew", "d12frosted//emacs-plus"),
            ("brew", "d12frosted/emacs-plus/"),
            ("brew", "/opt/x"),
            ("brew", "@30"),
            ("brew", "tap/.hidden"),
            ("scoop", "evil.json"),
            ("scoop", "../evil.json"),
            ("scoop", "extras/"),
            ("cargo", "ripgrep-"),
            ("cargo", "_rg"),
            ("pipx", "evil.whl"),
            ("pipx", "evil.tar.gz"),
            ("pipx", "evil.tgz"),
            ("pipx", "evil.zip"),
            ("pipx", "black."),
        ] {
            let m = manager(mgr).unwrap();
            assert!(
                !m.id_ok(bad),
                "{mgr}: {bad:?} must not read as a package name"
            );
            let mut a = pm_row();
            a.manager = mgr.into();
            a.package = bad.into();
            a.elevated = true;
            let why = refused(&a, &[]);
            assert!(why.contains("package-id charset"), "{mgr}: {bad:?}: {why}");
            assert!(
                why.contains(bad),
                "{mgr}: {bad:?}: the refusal names the id: {why}"
            );
        }
        // …and the real names still admit, tap paths and version suffixes included.
        for (mgr, good) in [
            ("apt", "emacs-nox"),
            ("apt", "libssl3t64"),
            ("apt", "g++"),
            ("apt", "libstdc++6"),
            ("dnf", "gcc-c++"),
            ("dnf", "emacs_nox-1.0+git"),
            ("brew", "d12frosted/emacs-plus/emacs-plus@30"),
            ("brew", "python@3.12"),
            ("brew", "gh"),
            ("winget", "GNU.Emacs"),
            ("winget", "Anthropic.ClaudeCode"),
            ("scoop", "extras/emacs"),
            ("cargo", "ripgrep"),
            ("cargo", "cargo-nextest"),
            ("pipx", "black"),
            ("pipx", "pre-commit"),
        ] {
            assert!(manager(mgr).unwrap().id_ok(good), "{mgr}: {good:?}");
        }
        assert_eq!(manager("brew").unwrap().local_file, &[".rb"]);
        assert!(manager("winget").unwrap().local_file.is_empty());
        let mut c = pm_row();
        c.manager = "cargo".into();
        c.elevated = false;
        c.package = "ripgrep".into();
        c.provides = vec!["rg".into()];
        check_row(&c, &[]).expect("a cargo row admits unelevated");
    }

    // ---- the system-pm protocol ----

    #[test]
    fn a_well_formed_system_pm_row_is_admitted_and_each_field_is_checked() {
        check_row(&pm_row(), &[]).expect("the reference apt row admits");
        // Every manager admits; the system-wide ones need elevation, the user-scoped
        // ones may go either way.
        for m in MANAGERS {
            let mut a = pm_row();
            a.manager = (*m).into();
            a.elevated = true;
            check_row(&a, &[]).unwrap_or_else(|e| panic!("{m}: {e}"));
            a.elevated = false;
            let r = check_row(&a, &[]);
            if manager(m).unwrap().elevated {
                assert!(
                    matches!(&r, Err(FlowError::VendorRefused(x)) if x.contains("elevated = true is required")),
                    "{m}: {r:?}"
                );
            } else {
                r.unwrap_or_else(|e| panic!("{m} unelevated: {e}"));
            }
        }
        let mut kind = pm_row();
        kind.kind = "binary".into();
        assert!(refused(&kind, &[]).contains("kind must be system-package"));
        for bad in ["", "yum", "APT", "brew "] {
            let mut a = pm_row();
            a.manager = bad.into();
            assert!(refused(&a, &[]).contains("manager must be"), "{bad:?}");
        }
        for bad in ["", "-y", "--force emacs", "GNU Emacs", "emacs\n", "x\u{7f}"] {
            let mut a = pm_row();
            a.package = bad.into();
            assert!(refused(&a, &[]).contains("package must be"), "{bad:?}");
        }
        let mut dotted = pm_row();
        dotted.manager = "winget".into();
        dotted.package = "GNU.Emacs".into();
        dotted.elevated = false;
        check_row(&dotted, &[]).expect("a dotted winget id admits");
        let mut none = pm_row();
        none.provides.clear();
        assert!(refused(&none, &[]).contains("needs provides"));
        for bad in [
            "bin/emacs",
            "sudo",
            "git",
            "../emacs",
            "usr/bin/emacs",
            "",
            "/etc/emacs",
            "/home/me/.cargo/bin/rg",
        ] {
            let mut a = pm_row();
            a.provides = vec![bad.into()];
            assert!(
                refused(&a, &[]).contains("provides entries must be tool names or absolute"),
                "{bad:?}"
            );
        }
        // The user-scoped managers' ids are the same one-argument rule under their own
        // charsets: nothing a shell, `cargo` or `pipx` would read as more than a name.
        for (mgr, bad) in [
            ("cargo", "ripgrep;rm -rf /"),
            ("cargo", "ripgrep$(id)"),
            ("cargo", "--git https://x"),
            ("cargo", "ripgrep --git"),
            ("cargo", "rip@grep"),
            ("cargo", "../ripgrep"),
            ("pipx", "black`id`"),
            ("pipx", "black==1.0"),
            ("pipx", "-e black"),
            ("pipx", "black|sh"),
            ("brew", "emacs;id"),
            ("brew", "$HOME"),
            ("scoop", "gh&calc"),
            ("winget", "GNU.Emacs\""),
        ] {
            let mut a = pm_row();
            a.manager = mgr.into();
            a.elevated = false;
            a.package = bad.into();
            let m = refused(&a, &[]);
            assert!(
                m.contains("package must be") || m.contains("package-id charset"),
                "{mgr} {bad:?}: {m}"
            );
        }
        // No bytes: every byte-shaped key is a refusal.
        let mut url = pm_row();
        url.url = "https://github.com/x".into();
        assert!(refused(&url, &[]).contains("moves no bytes"));
        let mut sha = pm_row();
        sha.sha256 = "7b09f01c".repeat(8);
        assert!(refused(&sha, &[]).contains("moves no bytes"));
        let mut size = pm_row();
        size.size = 1;
        assert!(refused(&size, &[]).contains("moves no bytes"));
        let mut asset = pm_row();
        asset.asset = "x".into();
        assert!(refused(&asset, &[]).contains("moves no bytes"));
        let mut team = pm_row();
        team.signer_team = "927JGANW46".into();
        assert!(refused(&team, &[]).contains("belong to the pkg and softwareupdate"));
        let mut label = pm_row();
        label.label_prefix = "Command Line Tools for Xcode".into();
        assert!(refused(&label, &[]).contains("belong to the pkg and softwareupdate"));
    }

    /// `cmd.exe`'s PATHEXT rule, pinned on every platform: a bare name tries each
    /// extension in PATHEXT order, a name already carrying a listed extension is tried
    /// as spelled (case-insensitively), an unset PATHEXT is the default, junk entries
    /// are ignored.
    #[test]
    fn windows_lookup_names_follow_pathext() {
        assert_eq!(
            windows_lookup_names("gh", ".COM;.EXE;.BAT;.CMD"),
            vec!["gh.COM", "gh.EXE", "gh.BAT", "gh.CMD"]
        );
        assert_eq!(
            windows_lookup_names("gh", ""),
            vec!["gh.COM", "gh.EXE", "gh.BAT", "gh.CMD"],
            "empty PATHEXT is the default"
        );
        assert_eq!(windows_lookup_names("gh.exe", ".COM;.EXE"), vec!["gh.exe"]);
        assert_eq!(
            windows_lookup_names("scoop.CMD", ".EXE;.cmd"),
            vec!["scoop.CMD"]
        );
        assert_eq!(
            windows_lookup_names("gh.tar", ".COM;.EXE"),
            vec!["gh.tar.COM", "gh.tar.EXE"],
            "an extension PATHEXT does not list is part of the name"
        );
        assert_eq!(
            windows_lookup_names("gh", ".EXE;;junk;.;.CMD"),
            vec!["gh.EXE", "gh.CMD"],
            "entries without a leading dot, and a bare dot, are ignored"
        );
        assert_eq!(
            windows_lookup_names("GNU.Emacs", ".EXE"),
            vec!["GNU.Emacs.EXE"]
        );
        assert_eq!(DEFAULT_PATHEXT, ".COM;.EXE;.BAT;.CMD");
    }

    #[test]
    fn https_host_parses_only_bare_hosts() {
        assert_eq!(https_host("https://github.com"), Some("github.com"));
        assert_eq!(https_host("https://github.com/"), Some("github.com"));
        assert_eq!(https_host("https://github.com?x"), Some("github.com"));
        assert_eq!(https_host("https://github.com#x"), Some("github.com"));
        assert_eq!(https_host("https://github.com:443/"), None);
        assert_eq!(https_host("https://a@github.com/"), None);
        assert_eq!(https_host("http://github.com/"), None);
        assert_eq!(https_host("HTTPS://github.com/"), None);
    }

    // ---- the system-satisfaction probe ----

    fn scratch(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("atpkg-vendor-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[cfg(unix)]
    fn lay_exe(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn a_system_binary_on_path_satisfies_and_the_managed_copy_never_does() {
        let root = scratch("probe");
        let prefix = root.join("prefix");
        let sys = root.join("usr-local-bin");
        let exe = lay_exe(&sys, "gh");
        // The managed bin/ and a store tree carry the same name — both must be skipped.
        lay_exe(&prefix.join("bin"), "gh");
        lay_exe(&prefix.join("store/gh/1/bin"), "gh");
        let path = std::env::join_paths([
            prefix.join("bin"),
            prefix.join("store/gh/1/bin"),
            sys.clone(),
        ])
        .unwrap();
        assert_eq!(
            system_binary_on_path(&prefix, "gh", Some(&path)),
            Some(exe.clone())
        );
        // Only managed dirs on PATH ⇒ not satisfied.
        let managed_only =
            std::env::join_paths([prefix.join("bin"), prefix.join("store/gh/1/bin")]).unwrap();
        assert_eq!(
            system_binary_on_path(&prefix, "gh", Some(&managed_only)),
            None
        );
        // A symlink elsewhere that RESOLVES into the store is still the managed copy.
        let link_dir = root.join("home-bin");
        std::fs::create_dir_all(&link_dir).unwrap();
        std::os::unix::fs::symlink(prefix.join("store/gh/1/bin/gh"), link_dir.join("gh")).unwrap();
        let via_link = std::env::join_paths([link_dir.clone()]).unwrap();
        assert_eq!(system_binary_on_path(&prefix, "gh", Some(&via_link)), None);
        // A symlink to a genuine system copy (Homebrew's shape) IS a system install.
        let brew = root.join("brew-bin");
        std::fs::create_dir_all(&brew).unwrap();
        std::os::unix::fs::symlink(&exe, brew.join("gh")).unwrap();
        let via_brew = std::env::join_paths([brew.clone()]).unwrap();
        assert_eq!(
            system_binary_on_path(&prefix, "gh", Some(&via_brew)),
            Some(brew.join("gh"))
        );
        // A RELATIVE PATH entry never counts, even one that resolves (from this
        // process's cwd) to the very directory holding the system copy: `cwd/../..`
        // up to `/`, then the system dir's own components.
        let cwd = std::env::current_dir().unwrap();
        let ups: PathBuf =
            std::iter::repeat_n("..", cwd.components().count().saturating_sub(1)).collect();
        let relative = ups.join(sys.strip_prefix("/").unwrap());
        assert!(!relative.is_absolute());
        assert!(
            relative.join("gh").exists(),
            "PRECONDITION: the relative spelling really reaches the system copy ({})",
            relative.display()
        );
        let via_relative = std::env::join_paths([relative.clone()]).unwrap();
        assert_eq!(
            system_binary_on_path(&prefix, "gh", Some(&via_relative)),
            None,
            "a relative PATH entry is the cwd, not a system"
        );
        let dot = std::env::join_paths([PathBuf::from("."), PathBuf::from("bin")]).unwrap();
        assert_eq!(system_binary_on_path(&prefix, "gh", Some(&dot)), None);
        // …while the absolute spelling of the same directory still satisfies.
        let mixed = std::env::join_paths([relative, sys.clone()]).unwrap();
        assert_eq!(
            system_binary_on_path(&prefix, "gh", Some(&mixed)),
            Some(exe.clone())
        );
        // Absent name, non-executable file, no PATH at all, a separator in the name.
        assert_eq!(system_binary_on_path(&prefix, "emacs", Some(&path)), None);
        std::fs::write(sys.join("plain"), b"x").unwrap();
        assert_eq!(system_binary_on_path(&prefix, "plain", Some(&path)), None);
        assert_eq!(system_binary_on_path(&prefix, "gh", None), None);
        assert_eq!(system_binary_on_path(&prefix, "../gh", Some(&path)), None);
        assert_eq!(system_binary_on_path(&prefix, "", Some(&path)), None);
        // A name the shim deny-list refuses never SATISFIES (`system_binary_on_path`
        // keeps the ToolName gate) but is FOUND by the raw walk the system-pm lane uses
        // for its manager — `cargo` is both a refused shim name and a package manager.
        let cargo = lay_exe(&sys, "cargo");
        assert_eq!(system_binary_on_path(&prefix, "cargo", Some(&path)), None);
        assert_eq!(
            executable_on_path(&prefix, "cargo", Some(&path)),
            Some(cargo.clone())
        );
        // …with the same exclusions: the managed prefix, a link into the store, a
        // relative entry, a separator in the name.
        lay_exe(&prefix.join("bin"), "cargo");
        assert_eq!(
            executable_on_path(&prefix, "cargo", Some(&managed_only)),
            None
        );
        assert_eq!(executable_on_path(&prefix, "cargo", Some(&dot)), None);
        assert_eq!(executable_on_path(&prefix, "bin/cargo", Some(&path)), None);
        assert_eq!(executable_on_path(&prefix, "..", Some(&path)), None);
        assert_eq!(executable_on_path(&prefix, "", Some(&path)), None);
        assert_eq!(executable_on_path(&prefix, "cargo", None), None);
        // The Program-level wrapper: no `system` key ⇒ never satisfied, whatever PATH holds.
        let unmanaged = Program {
            repo: "gh".into(),
            policy: String::new(),
            coherence_group: None,
            extra: false,
            system: None,
            unavailable_hint: None,
            requires: vec![],
        };
        assert_eq!(system_satisfied(&prefix, &unmanaged), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- the shadow probe ----

    /// SHADOWED means "a foreign copy runs before the managed one": a hit BEFORE the
    /// managed `bin/` (or with the managed `bin/` absent) is a shadow; a hit AFTER it is
    /// not; the managed dirs themselves and links into the store never are; relative
    /// entries never count.
    #[cfg(unix)]
    #[test]
    fn the_shadow_probe_reports_only_what_precedes_the_managed_bin() {
        let root = scratch("shadow");
        let prefix = root.join("prefix");
        let local = root.join("local-bin");
        let exe = lay_exe(&local, "trust");
        lay_exe(&prefix.join("bin"), "trust");
        lay_exe(&prefix.join("store/trust/6808/bin"), "trust");
        // Before the managed bin/: shadowed.
        let before = std::env::join_paths([local.clone(), prefix.join("bin")]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&before)),
            Some(exe.clone())
        );
        // After it: the managed copy wins, no shadow.
        let after = std::env::join_paths([prefix.join("bin"), local.clone()]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&after)),
            None
        );
        // Managed bin/ not on PATH at all (a doctor run outside an aterm shell): inside
        // aterm it would be appended LAST, so the foreign copy still precedes it.
        let absent = std::env::join_paths([local.clone()]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&absent)),
            Some(exe.clone())
        );
        // A store dir on PATH ahead of bin/ is atpkg's own — it stops the walk too.
        let store_first =
            std::env::join_paths([prefix.join("store/trust/6808/bin"), local.clone()]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&store_first)),
            None
        );
        // A symlink into the store is not a shadow.
        let link_dir = root.join("home-bin");
        std::fs::create_dir_all(&link_dir).unwrap();
        std::os::unix::fs::symlink(
            prefix.join("store/trust/6808/bin/trust"),
            link_dir.join("trust"),
        )
        .unwrap();
        let via_link = std::env::join_paths([link_dir.clone(), prefix.join("bin")]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&via_link)),
            None
        );
        // A user's `~/bin` that IS the managed bin/ through a symlink, ahead of a foreign
        // copy: the managed copy runs, so the foreign one behind it is no shadow.
        let alias = root.join("alias-bin");
        std::os::unix::fs::symlink(prefix.join("bin"), &alias).unwrap();
        let via_alias = std::env::join_paths([alias.clone(), local.clone()]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&via_alias)),
            None,
            "a symlinked managed bin/ ahead stops the walk"
        );
        // A user's own symlink to the store copy ahead of a foreign copy: same thing.
        let link_then_local = std::env::join_paths([link_dir.clone(), local.clone()]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&link_then_local)),
            None,
            "a link into the store ahead is the managed copy running"
        );
        // …but for the SYSTEM probe the same aliases are skipped, and the foreign copy
        // behind them still satisfies.
        assert_eq!(
            system_binary_on_path(&prefix, "trust", Some(&via_alias)),
            Some(exe.clone())
        );
        assert_eq!(
            system_binary_on_path(&prefix, "trust", Some(&link_then_local)),
            Some(exe.clone())
        );
        // A DIRECTORY wearing the tool's name is never a hit (the Windows `gh.exe`
        // directory trick reads the same way: `is_file` decides, not the name).
        let dir_named = root.join("dir-named");
        std::fs::create_dir_all(dir_named.join("trust")).unwrap();
        let dir_first = std::env::join_paths([dir_named.clone(), prefix.join("bin")]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&dir_first)),
            None
        );
        assert_eq!(
            system_binary_on_path(&prefix, "trust", Some(&dir_first)),
            None
        );
        // Relative entries never count; a different name is not a shadow; no PATH, none.
        let dot = std::env::join_paths([PathBuf::from("."), prefix.join("bin")]).unwrap();
        assert_eq!(shadowing_binary_on_path(&prefix, "trust", Some(&dot)), None);
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust-mc", Some(&before)),
            None
        );
        assert_eq!(shadowing_binary_on_path(&prefix, "trust", None), None);
        assert_eq!(
            shadowing_binary_on_path(&prefix, "../trust", Some(&before)),
            None
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
