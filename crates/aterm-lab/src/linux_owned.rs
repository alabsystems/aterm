// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Creation receipts for Lima's namespace, VM directories and guest build roots.

use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Component, Path, PathBuf};

const OWNER: &str = "aterm-lima-owner-v2";
const RECORD_MAX: u64 = 4096;
const GUEST_PREFIX: &str = "aterm-lane.";
const VM_MARKER: &str = ".aterm-lab-created";
// GNU stat in the guest, BSD stat for a local Unix runner of the same commands.
const IDENTITY: &str =
    "identity() { stat -c '%d:%i:%u' -- \"$1\" 2>/dev/null || stat -f '%d:%i:%u' -- \"$1\"; }; ";

fn directory_id(path: &Path) -> Result<String, String> {
    let m = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !m.is_dir() || m.uid() != crate::sys::getuid() || m.mode() & 0o022 != 0 {
        return Err(format!(
            "{} is not an owned directory protected from other users' writes; retained",
            path.display()
        ));
    }
    Ok(format!("{}:{}", m.dev(), m.ino()))
}

fn read_record(path: &Path) -> Result<Option<String>, String> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}; retained", path.display())),
    };
    let m = file.metadata().map_err(|e| e.to_string())?;
    if !m.is_file()
        || m.uid() != crate::sys::getuid()
        || m.mode() & 0o777 != 0o600
        || m.nlink() != 1
        || m.len() > RECORD_MAX
    {
        return Err(format!(
            "{} is not a private ownership receipt; retained",
            path.display()
        ));
    }
    let mut text = String::with_capacity(usize::try_from(m.len()).unwrap_or(0).saturating_add(1));
    std::io::Read::by_ref(&mut file)
        .take(RECORD_MAX + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > RECORD_MAX {
        return Err("ownership receipt grew; retained".into());
    }
    Ok(Some(text))
}

fn write_new(path: &Path, text: &str) -> Result<(), String> {
    if text.len() as u64 > RECORD_MAX {
        return Err("ownership receipt is too large; retained".into());
    }
    let mut file = crate::util::create_new_regular_file(path, 0o600)
        .map_err(|e| format!("{}: {e}; retained", path.display()))?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    // Losing an ownership receipt cannot authorize adoption after a power loss.
    aterm_durable::durable(&file).map_err(|e| e.to_string())?;
    aterm_durable::durable_dir(path.parent().ok_or("receipt has no parent")?)
        .map_err(|e| e.to_string())
}

fn absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path.to_string_lossy().chars().any(char::is_control)
        && !path.to_string_lossy().split('/').any(|p| p == ".")
        && !path.components().any(|c| matches!(c, Component::ParentDir))
}

/// An explicit Lima home, or the lab's private default, never the owner's `~/.lima`.
pub fn home(named: Option<&str>, default: &Path) -> Result<PathBuf, String> {
    let path = named
        .filter(|s| !s.is_empty())
        .map_or_else(|| default.to_path_buf(), PathBuf::from);
    if !absolute(&path) {
        return Err("LIMA_HOME must be an absolute path without '..' or control characters".into());
    }
    Ok(path)
}

/// A namespace and VM name whose receipts are checked again at each command boundary.
pub struct Namespace {
    pub path: PathBuf,
    vm: String,
    token: String,
}

impl Namespace {
    pub fn open(path: &Path, vm: &str) -> Result<Self, String> {
        if !absolute(path)
            || vm.is_empty()
            || !vm
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err("invalid Lima namespace or VM name".into());
        }
        let parent = path
            .parent()
            .ok_or("Lima namespace has no parent")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let path = parent.join(path.file_name().ok_or("Lima namespace has no name")?);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&path)
                    .map_err(|e| e.to_string())?;
                let token = directory_id(&path)?;
                write_new(&path.join("owner"), &format!("{OWNER}\n{token}\n"))?;
            }
            Err(e) => return Err(format!("{}: {e}; retained", path.display())),
        }
        let ns = Self {
            token: directory_id(&path)?,
            path,
            vm: vm.into(),
        };
        ns.check()?;
        Ok(ns)
    }

    pub fn lock_path(&self) -> PathBuf {
        crate::linux::lane_lock_path(&self.vm, &self.path)
    }
    pub fn vm_dir(&self) -> PathBuf {
        self.path.join(&self.vm)
    }
    fn receipt(&self, kind: &str) -> PathBuf {
        self.path.join(format!(".{}.{kind}-owner", self.vm))
    }

    pub fn check(&self) -> Result<(), String> {
        let want = format!("{OWNER}\n{}\n", self.token);
        if directory_id(&self.path)? != self.token
            || std::fs::symlink_metadata(&self.path)
                .map_err(|e| e.to_string())?
                .mode()
                & 0o077
                != 0
            || read_record(&self.path.join("owner"))?.as_deref() != Some(want.as_str())
        {
            return Err(format!(
                "{} has no matching creation receipt; retained. Use a new LIMA_HOME for the lab",
                self.path.display()
            ));
        }
        Ok(())
    }

    pub fn admit_vm(&self, dir: &Path) -> Result<(), String> {
        self.check()?;
        if dir != self.vm_dir() {
            return Err("VM is outside the owned Lima namespace; retained".into());
        }
        let want = format!("{OWNER}\n{}\n{}\n", self.token, directory_id(dir)?);
        if read_record(&self.receipt("vm"))?.as_deref() != Some(want.as_str())
            || read_record(&dir.join(VM_MARKER))?.as_deref() != Some(want.as_str())
        {
            return Err(format!(
                "{} has no matching VM creation receipt; retained",
                dir.display()
            ));
        }
        Ok(())
    }

    /// The creator runs only after both the VM directory and its receipts were absent.
    pub fn create_vm<T>(&self, create: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.check()?;
        match std::fs::symlink_metadata(self.vm_dir()) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("VM path exists without this creation; retained".into()),
        }
        if read_record(&self.receipt("vm"))?.is_some()
            || read_record(&self.receipt("guest"))?.is_some()
        {
            return Err("VM creation receipts already exist; retained".into());
        }
        let result = create()?;
        self.check()?;
        let want = format!(
            "{OWNER}\n{}\n{}\n",
            self.token,
            directory_id(&self.vm_dir())?
        );
        write_new(&self.vm_dir().join(VM_MARKER), &want)?;
        write_new(&self.receipt("vm"), &want)?;
        self.admit_vm(&self.vm_dir())?;
        Ok(result)
    }

    pub fn with_vm<T>(&self, run: impl FnOnce() -> T) -> Result<T, String> {
        self.admit_vm(&self.vm_dir())?;
        Ok(run())
    }

    /// An existing directory must be owned before a disk measurement, lock or VM query.
    pub fn existing_vm(&self) -> Result<(), String> {
        self.check()?;
        match std::fs::symlink_metadata(self.vm_dir()) {
            Ok(_) => self.admit_vm(&self.vm_dir()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if read_record(&self.receipt("vm"))?.is_some()
                    || read_record(&self.receipt("guest"))?.is_some()
                {
                    Err("VM directory is missing but creation receipts remain; retained".into())
                } else {
                    Ok(())
                }
            }
            Err(e) => Err(format!("VM directory cannot be judged: {e}; retained")),
        }
    }

    /// Forget receipts only after a successful deletion removed the VM directory.
    pub fn retired_vm(&self) -> Result<(), String> {
        self.check()?;
        match std::fs::symlink_metadata(self.vm_dir()) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("deleted VM path still exists; receipts retained".into()),
        }
        let record = read_record(&self.receipt("vm"))?.ok_or("VM receipt disappeared; retained")?;
        let rows: Vec<&str> = record.lines().collect();
        if rows.len() != 3 || rows[0] != OWNER || rows[1] != self.token || !numeric_id(rows[2], 2) {
            return Err("VM receipt changed; retained".into());
        }
        if let Some(guest) = read_record(&self.receipt("guest"))? {
            GuestDir::parse(&guest, &format!("{}:{}:{}", self.token, self.vm, rows[2]))?;
            std::fs::remove_file(self.receipt("guest")).map_err(|e| e.to_string())?;
        }
        std::fs::remove_file(self.receipt("vm")).map_err(|e| e.to_string())
    }

    fn guest_owner(&self) -> Result<String, String> {
        self.admit_vm(&self.vm_dir())?;
        Ok(format!(
            "{}:{}:{}",
            self.token,
            self.vm,
            directory_id(&self.vm_dir())?
        ))
    }

    fn saved_guest(&self) -> Result<Option<GuestDir>, String> {
        let owner = self.guest_owner()?;
        read_record(&self.receipt("guest"))?
            .map(|s| GuestDir::parse(&s, &owner))
            .transpose()
    }

    pub fn prepare_guest(
        &self,
        home: &str,
        mut run: impl FnMut(&[String]) -> Result<String, String>,
    ) -> Result<GuestDir, String> {
        if let Some(guest) = self.saved_guest()? {
            if Path::new(&guest.path).parent() != Some(Path::new(home)) {
                return Err("guest HOME changed; build root retained".into());
            }
            if guest.check(false, &mut run)? {
                return Ok(guest);
            }
            // The exact old root is absent; no path is removed or adopted.
            std::fs::remove_file(self.receipt("guest")).map_err(|e| e.to_string())?;
        }
        let guest = GuestDir::create(home, &self.guest_owner()?, &mut run)?;
        write_new(&self.receipt("guest"), &guest.render())?;
        Ok(guest)
    }

    pub fn clear_guest(
        &self,
        mut run: impl FnMut(&[String]) -> Result<String, String>,
    ) -> Result<(), String> {
        if let Some(guest) = self.saved_guest()? {
            guest.check(true, &mut run)?;
        }
        Ok(())
    }
}

fn numeric_id(text: &str, fields: usize) -> bool {
    text.split(':').count() == fields
        && text
            .split(':')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// An exclusive guest directory, with a guest identity and an independent host receipt.
pub struct GuestDir {
    pub path: String,
    id: String,
    owner: String,
}

impl GuestDir {
    fn render(&self) -> String {
        format!("{OWNER}\n{}\n{}\n{}\n", self.owner, self.path, self.id)
    }

    fn parse(text: &str, owner: &str) -> Result<Self, String> {
        let rows: Vec<&str> = text.lines().collect();
        let [magic, got, path, id] = rows.as_slice() else {
            return Err("malformed guest receipt; retained".into());
        };
        if text.len() as u64 > RECORD_MAX
            || !text.ends_with('\n')
            || *magic != OWNER
            || *got != owner
            || !absolute(Path::new(path))
            || !numeric_id(id, 3)
            || !Path::new(path).file_name().is_some_and(|n| {
                n.to_string_lossy()
                    .strip_prefix(GUEST_PREFIX)
                    .is_some_and(|suffix| !suffix.is_empty())
            })
        {
            return Err("guest receipt does not match its creation; retained".into());
        }
        Ok(Self {
            path: (*path).into(),
            id: (*id).into(),
            owner: owner.into(),
        })
    }

    fn create(
        home: &str,
        owner: &str,
        run: &mut impl FnMut(&[String]) -> Result<String, String>,
    ) -> Result<Self, String> {
        if !absolute(Path::new(home)) {
            return Err("invalid guest HOME".into());
        }
        let argv = vec![
            "sh".into(),
            "-eu".into(),
            "-c".into(),
            format!(
                "{IDENTITY}d=$(mktemp -d \"$1/{GUEST_PREFIX}XXXXXXXXXXXX\"); chmod 0755 \"$d\"; (umask 077; set -C; printf '%s\\n' \"$2\" > \"$d/.aterm-owner\"); printf '%s\\n' \"$d\"; identity \"$d\""
            ),
            "owned-guest-create".into(),
            home.into(),
            owner.into(),
        ];
        let text = run(&argv)?;
        if text.len() as u64 > RECORD_MAX || !text.ends_with('\n') {
            return Err("guest creation returned an incomplete receipt; retained".into());
        }
        let rows: Vec<&str> = text.lines().collect();
        let [path, id] = rows.as_slice() else {
            return Err("guest creation returned no exact receipt; retained".into());
        };
        let record = format!("{OWNER}\n{owner}\n{path}\n{id}\n");
        let guest = Self::parse(&record, owner)?;
        if Path::new(&guest.path).parent() != Some(Path::new(home)) {
            return Err("guest creation returned a foreign root; retained".into());
        }
        Ok(guest)
    }

    fn check(
        &self,
        clear: bool,
        run: &mut impl FnMut(&[String]) -> Result<String, String>,
    ) -> Result<bool, String> {
        let script = format!(
            "{IDENTITY}if [ ! -e \"$1\" ] && [ ! -L \"$1\" ]; then printf 'MISSING\\n'; exit 0; fi; if [ -L \"$1\" ] || [ ! -d \"$1\" ]; then exit 1; fi; [ \"$(identity \"$1\")\" = \"$2\" ]; if [ -L \"$1/.aterm-owner\" ] || [ ! -f \"$1/.aterm-owner\" ]; then exit 1; fi; [ \"$(cat -- \"$1/.aterm-owner\")\" = \"$3\" ]; chmod 0755 \"$1\"; if [ \"$4\" = clear ]; then for p in \"$1\"/* \"$1\"/.[!.]* \"$1\"/..?*; do if [ ! -e \"$p\" ] && [ ! -L \"$p\" ]; then continue; fi; if [ \"$p\" = \"$1/.aterm-owner\" ]; then continue; fi; rm -rf -- \"${{p:?}}\"; done; fi; printf 'OWNED\\n'"
        );
        let argv = vec![
            "sh".into(),
            "-eu".into(),
            "-c".into(),
            script,
            "owned-guest-check".into(),
            self.path.clone(),
            self.id.clone(),
            self.owner.clone(),
            if clear { "clear" } else { "check" }.into(),
        ];
        match run(&argv)?.as_str() {
            "OWNED\n" => Ok(true),
            "MISSING\n" => Ok(false),
            _ => Err("guest ownership check returned no exact verdict; retained".into()),
        }
    }
}

#[cfg(test)]
#[path = "linux_owned_tests.rs"]
mod tests;
