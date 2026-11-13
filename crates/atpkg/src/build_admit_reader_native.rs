// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bind the normal managed shim, its actual hop, and a retained reader image.

use super::{QUERY, QUERY_ENV, normal_capabilities};
use aterm_uds::spawnfd::Tether as _;
use std::fs::{File, Metadata};
use std::io;
use std::os::unix::fs::{FileExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_SHIM: u64 = 128 * 1024;
type Stamp = (u64, u64, u64, i64, i64, i64, i64);
fn stamp(meta: &Metadata) -> Stamp {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}
fn error(why: impl ToString) -> io::Error {
    io::Error::other(why.to_string())
}

struct Shim {
    name: &'static str,
    path: PathBuf,
    file: File,
    stamp: Stamp,
    bytes: Vec<u8>,
    environment: crate::shim_env::ShimEnv,
}

fn normal_bin() -> io::Result<PathBuf> {
    let layout = crate::store::resolve_for_lookup()
        .map_err(error)?
        .ok_or_else(|| error("normal managed package layout unavailable"))?;
    let bin = layout.bin_dir().canonicalize()?;
    if !std::fs::symlink_metadata(&bin)?.is_dir() {
        return Err(error("normal managed bin directory is unavailable"));
    }
    Ok(bin)
}

fn normal_shim(name: &str) -> io::Result<Option<PathBuf>> {
    let bin = normal_bin()?;
    let named = bin.join(name);
    let first = crate::vendor::first_on_path(name, std::env::var_os("PATH").as_deref());
    match std::fs::symlink_metadata(&named) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if normal_bin()? != bin || first.is_some() {
                return Err(io::Error::other(format!(
                    "{name} has a shadowed or changing normal route"
                )));
            }
            return Ok(None);
        }
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let shim = named.canonicalize()?;
    let first = first
        .ok_or_else(|| error(format!("normal managed {name} is not executable on PATH")))?
        .canonicalize()?;
    if first != shim {
        return Err(error(format!(
            "normal managed {name} is shadowed; reader protocol is unproved"
        )));
    }
    Ok(Some(shim))
}

impl Shim {
    fn capture(name: &'static str, path: PathBuf) -> io::Result<(Self, PathBuf)> {
        let (file, meta) = aterm_uds::regular_file::open(&path)?;
        if meta.len() == 0 || meta.len() > MAX_SHIM || meta.mode() & 0o022 != 0 {
            return Err(error(
                "normal managed shim is not a bounded private-writer file",
            ));
        }
        let mut bytes = vec![0; usize::try_from(meta.len()).map_err(error)?];
        file.read_exact_at(&mut bytes, 0)?;
        let text = std::str::from_utf8(&bytes).map_err(error)?;
        let helper = hop_helper(&path, text)?;
        let environment = crate::platform::parse_sh_shim_env(text);
        let this = Self {
            name,
            path,
            file,
            stamp: stamp(&meta),
            bytes,
            environment,
        };
        this.check()?;
        Ok((this, helper))
    }

    fn check(&self) -> io::Result<()> {
        if normal_shim(self.name)?.as_ref() != Some(&self.path)
            || stamp(&self.file.metadata()?) != self.stamp
            || stamp(&std::fs::symlink_metadata(&self.path)?) != self.stamp
        {
            return Err(error("normal managed shim changed"));
        }
        let mut bytes = vec![0; self.bytes.len()];
        self.file.read_exact_at(&mut bytes, 0)?;
        if bytes != self.bytes || stamp(&self.file.metadata()?) != self.stamp {
            return Err(error("normal managed shim bytes changed"));
        }
        Ok(())
    }
}

struct AbsentShim {
    name: &'static str,
    bin: PathBuf,
    directory: File,
    identity: (u64, u64),
}

impl AbsentShim {
    fn capture(name: &'static str) -> io::Result<Self> {
        let bin = normal_bin()?;
        let directory = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY)
            .open(&bin)?;
        let meta = directory.metadata()?;
        let this = Self {
            name,
            bin,
            directory,
            identity: (meta.dev(), meta.ino()),
        };
        this.check()?;
        Ok(this)
    }

    fn check(&self) -> io::Result<()> {
        let held = self.directory.metadata()?;
        let named = std::fs::symlink_metadata(&self.bin)?;
        if normal_bin()? != self.bin
            || !held.is_dir()
            || !named.is_dir()
            || (held.dev(), held.ino()) != self.identity
            || (named.dev(), named.ino()) != self.identity
            || normal_shim(self.name)?.is_some()
        {
            return Err(error(format!(
                "normal {} absence is no longer established",
                self.name
            )));
        }
        Ok(())
    }
}

fn hop_helper(shim: &Path, text: &str) -> io::Result<PathBuf> {
    let marker = "# atpkg cache helper ";
    if text.lines().filter(|line| line.starts_with(marker)).count() != 1 {
        return Err(error("normal shim has no unique recorded helper"));
    }
    let helper = crate::build_cache::registered::shim_helper(text)
        .ok_or_else(|| error("normal shim helper cannot be named"))?;
    // Reuse the actual complete generator, not a shell parser or suffix match.
    // Any extra executable prelude, older template or altered tail refuses.
    let target = crate::platform::parse_sh_shim_target(text)
        .ok_or_else(|| error("normal shim has no maintained target"))?;
    let env = crate::platform::parse_sh_shim_env(text);
    let expected = crate::platform::canonical_build_hop_shim(shim, &target, &env, &helper)?;
    if text.as_bytes() != expected.body {
        return Err(error(
            "normal shim differs from the complete maintained build hop",
        ));
    }
    Ok(helper)
}

/// A precise reader witness, held with the actual shim and selected image.
/// This is availability evidence only; no response supplies memory or CPU.
pub struct Readiness {
    readers: Vec<Reader>,
    absent: Vec<AbsentShim>,
    expected: aterm_json::Value,
}

struct Reader {
    shim: Shim,
    image: aterm_uds::tether::SelectedExecutable,
}

impl Readiness {
    /// Query the actual normal reader before any new reservation record exists.
    /// # Errors
    /// The managed hop, image, execution proof, protocol, namespace or original
    /// machine identity is unsupported, unavailable, changed or cancelled.
    pub fn normal(stopped: &mut dyn FnMut() -> bool) -> io::Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(60);
        if stopped() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let mut candidates = Vec::new();
        let mut absent = Vec::new();
        // The two existing managed build-hop frontends, not a reader registry.
        for name in ["targo", "targo-tippy"] {
            match normal_shim(name)? {
                Some(path) => candidates.push(Shim::capture(name, path)?),
                None if name == "targo-tippy" => absent.push(AbsentShim::capture(name)?),
                None => return Err(error("normal managed Targo is absent")),
            }
        }
        let expected = normal_capabilities().map_err(error)?;
        let mut readers = Vec::new();
        for (shim, helper) in candidates {
            let image = aterm_uds::tether::SelectedExecutable::retain(&helper, deadline)?;
            // HOPv2 proves this exact copy actually ran and was positively waited,
            // including for an older reader that cannot answer the second query.
            // It is deliberately NOT authority for the retained-record schema.
            let first = query(
                &image,
                &helper,
                &shim.environment,
                Query::Hop,
                deadline,
                stopped,
            )?;
            if !first.status.success()
                || first.stdout != format!("{}\n", super::super::HOP_CAPABILITY).as_bytes()
            {
                return Err(error(
                    "normal reader copy returned no exact hop execution proof",
                ));
            }
            shim.check()?;
            let answered = query(
                &image,
                &helper,
                &shim.environment,
                Query::Reader,
                deadline,
                stopped,
            )?;
            if !answers(&answered, &expected) {
                return Err(error(format!(
                    "normal {} reader does not support this retained-reservation schema and machine namespace",
                    shim.name
                )));
            }
            readers.push(Reader { shim, image });
        }
        let this = Self {
            readers,
            absent,
            expected,
        };
        this.check()?;
        Ok(this)
    }

    /// Recheck original route/image and local namespace before publishing intent.
    /// # Errors
    /// Any original selection, held file, machine, boot or namespace changed.
    pub fn check(&self) -> io::Result<()> {
        for reader in &self.readers {
            reader.shim.check()?;
            reader
                .image
                .path(Instant::now() + Duration::from_secs(60))?;
        }
        for absent in &self.absent {
            absent.check()?;
        }
        if normal_capabilities().map_err(error)? != self.expected {
            return Err(error(
                "reader witness belongs to another machine or namespace",
            ));
        }
        Ok(())
    }
}

fn answers(output: &std::process::Output, expected: &aterm_json::Value) -> bool {
    output.status.success()
        && aterm_json::from_slice::<aterm_json::Value>(&output.stdout)
            .ok()
            .as_ref()
            == Some(expected)
}

enum Query {
    Hop,
    Reader,
}

fn configure_query(
    command: &mut aterm_uds::spawnfd::Command,
    environment: &crate::shim_env::ShimEnv,
    query: Query,
) {
    for (name, value) in environment.entries() {
        command.env(name, value);
    }
    command.args([super::super::VERB, "--capabilities"]);
    command.env_remove(QUERY_ENV);
    if matches!(query, Query::Reader) {
        command.env(QUERY_ENV, QUERY);
    }
}

fn query(
    image: &aterm_uds::tether::SelectedExecutable,
    helper: &Path,
    environment: &crate::shim_env::ShimEnv,
    selection: Query,
    deadline: Instant,
    stopped: &mut dyn FnMut() -> bool,
) -> io::Result<std::process::Output> {
    let mut command = aterm_uds::spawnfd::command(image.path(deadline)?);
    command.arg0(helper).process_group(0).tether();
    configure_query(&mut command, environment, selection);
    let output = aterm_uds::deadline::output_until_complete_stdout_with_limit_unless(
        &mut command,
        deadline,
        16 * 1024,
        stopped,
    )
    .map_err(error)?;
    image.path(deadline)?;
    Ok(output)
}

#[cfg(test)]
#[path = "build_admit_reader_native_tests.rs"]
mod tests;
