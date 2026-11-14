// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How install.sh finds the verifier.

use aterm_toml::{Table, Value};

/// The architectures a static verifier is built for, as install.sh names them.
pub const ARCHES: [&str; 2] = ["x86_64", "aarch64"];

/// The release asset of `arch`'s static verifier in release `version`.
#[must_use]
pub fn asset_name(version: &str, arch: &str) -> String {
    format!("aterm-bootstrap-verify-{version}-linux-{arch}")
}

/// The largest verifier asset install.sh downloads (`release_asset_size`'s bound).
pub const MAX_ASSET: u64 = 64 * 1024 * 1024;

/// The command line install.sh calls the verifier with and the line it reads back.
pub const INTERFACE: u32 = 1;

/// The cargo `--config` a release builds `triple`'s verifier under.
#[must_use]
pub fn static_build_config(triple: &str) -> String {
    format!("target.{triple}.rustflags=[\"-C\", \"target-feature=+crt-static\"]")
}

/// install.sh's names for the pin's four lines.
pub const KEYS: [&str; 4] = [
    "LINUX_VERIFIER_REPO",
    "LINUX_VERIFIER_TAG",
    "LINUX_VERIFIER_SHA256_X86_64",
    "LINUX_VERIFIER_SHA256_AARCH64",
];

/// The repository, the release whose verifier install.sh runs, and each architecture's SHA-256
/// of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pin {
    /// `owner/name`: where the release is, whatever repository the install reads.
    pub repo: String,
    /// `v<version>`.
    pub tag: String,
    pub x86_64: String,
    pub aarch64: String,
}

fn repository(repo: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    repo.split_once('/')
        .is_some_and(|(owner, name)| part(owner) && part(name))
}

fn canonical_tag(tag: &str) -> bool {
    tag.strip_prefix('v').is_some_and(|version| {
        let parts: Vec<&str> = version.split('.').collect();
        parts.len() == 3
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.bytes().all(|b| b.is_ascii_digit())
                    && (*part == "0" || !part.starts_with('0'))
            })
    })
}

/// The value of each of `keys` in `text`, `script`'s text: each key on exactly one line of its
/// own, `<key><assign>"value"`, the value a plain word (no `"`, `$`, `` ` `` or `\`, which a
/// shell or PowerShell would read as more than the word).
fn read_values(
    text: &str,
    script: &str,
    keys: &[&str],
    assign: &str,
) -> Result<Vec<String>, String> {
    let mut values = Vec::with_capacity(keys.len());
    for key in keys {
        let prefix = format!("{key}{assign}");
        let lines: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with(&prefix))
            .collect();
        let [line] = lines.as_slice() else {
            return Err(format!(
                "{script} must set {key} on exactly one line of its own ({} found)",
                lines.len()
            ));
        };
        let value = line[prefix.len()..]
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .filter(|value| !value.contains(['"', '$', '`', '\\']))
            .ok_or_else(|| format!("{script}'s {key} is not a plain double-quoted word: {line}"))?;
        values.push(value.to_string());
    }
    Ok(values)
}

/// Empty (`repo`, `tag` and every digest), or an `owner/name` repository and a canonical
/// release tag with each of `digests` (an architecture and its SHA-256) empty or 64 lowercase
/// hex digits and at least one of them set.
fn validate_pin(repo: &str, tag: &str, digests: &[(&str, &str)]) -> Result<(), String> {
    if repo.is_empty() && tag.is_empty() && digests.iter().all(|(_, d)| d.is_empty()) {
        return Ok(());
    }
    if !repository(repo) {
        return Err(format!(
            "the verifier pin's repository {repo:?} is not owner/name"
        ));
    }
    if !canonical_tag(tag) {
        return Err(format!(
            "the verifier pin's tag {tag:?} is not v<MAJOR>.<MINOR>.<PATCH>"
        ));
    }
    for (arch, digest) in digests {
        if !digest.is_empty() && !aterm_codec::hex::is_lower(digest, 64) {
            return Err(format!(
                "the verifier pin's {arch} SHA-256 {digest:?} is not 64 lowercase hex digits"
            ));
        }
    }
    if digests.iter().all(|(_, digest)| digest.is_empty()) {
        return Err(format!("the verifier pin names {tag} but no digest"));
    }
    Ok(())
}

impl Pin {
    /// The pin in `install_sh`, the script's text: each key on exactly one line of its own,
    /// `KEY="value"`.
    /// # Errors
    /// A key missing, repeated, or not spelt so.
    pub fn read(install_sh: &str) -> Result<Self, String> {
        let values = read_values(install_sh, "install.sh", &KEYS, "=")?;
        let [repo, tag, x86_64, aarch64] =
            <[String; 4]>::try_from(values).map_err(|_| "four pin values".to_string())?;
        let pin = Self {
            repo,
            tag,
            x86_64,
            aarch64,
        };
        pin.validate()?;
        Ok(pin)
    }

    /// The four lines, as install.sh carries them.
    #[must_use]
    pub fn lines(&self) -> String {
        format!(
            "{}=\"{}\"\n{}=\"{}\"\n{}=\"{}\"\n{}=\"{}\"\n",
            KEYS[0], self.repo, KEYS[1], self.tag, KEYS[2], self.x86_64, KEYS[3], self.aarch64
        )
    }

    /// `install_sh` with its pin, whatever it is, replaced by this one.
    /// # Errors
    /// The script carries no readable pin, or not as four lines together in order.
    pub fn rewrite(&self, install_sh: &str) -> Result<String, String> {
        self.validate()?;
        let lines = Self::read(install_sh)?.lines();
        if install_sh.matches(&lines).count() != 1 {
            return Err("install.sh's pin is not its four lines together, in order".into());
        }
        Ok(install_sh.replacen(&lines, &self.lines(), 1))
    }

    /// Whether no release is pinned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.repo.is_empty()
            && self.tag.is_empty()
            && self.x86_64.is_empty()
            && self.aarch64.is_empty()
    }

    /// `arch`'s pinned SHA-256, empty when it has none.
    #[must_use]
    pub fn sha256(&self, arch: &str) -> &str {
        match arch {
            "x86_64" => &self.x86_64,
            "aarch64" => &self.aarch64,
            _ => "",
        }
    }

    /// Empty, or an `owner/name` repository and a canonical release tag with each digest empty or
    /// 64 lowercase hex digits and at least one of them set.
    /// # Errors
    /// Any other shape.
    pub fn validate(&self) -> Result<(), String> {
        let digests = ARCHES.map(|arch| (arch, self.sha256(arch)));
        validate_pin(&self.repo, &self.tag, &digests)
    }
}

/// Where the record of the pinned release is committed, from the checkout's root.
pub const RECORD: &str = "crates/aterm-bootstrap-verify/pinned.toml";

/// One architecture's verifier as the release's cut froze it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub arch: String,
    pub asset: String,
    pub sha256: String,
    pub size: u64,
}

/// The receipts of the release install.sh's pin names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub repo: String,
    pub tag: String,
    pub build_number: u64,
    pub commit: String,
    pub interface: u32,
    pub verifiers: Vec<Receipt>,
}

const RECORD_SCHEMA: i64 = 1;

fn exact_keys(table: &Table, keys: &[&str], what: &str) -> Result<(), String> {
    let mut found: Vec<&str> = table.keys().map(String::as_str).collect();
    found.sort_unstable();
    let mut wanted = keys.to_vec();
    wanted.sort_unstable();
    if found == wanted {
        Ok(())
    } else {
        Err(format!("{what} has the keys {found:?}, not {wanted:?}"))
    }
}

fn string(table: &Table, key: &str) -> Result<String, String> {
    match table.get(key) {
        Some(Value::String(text)) => Ok(text.clone()),
        _ => Err(format!("the verifier record's {key} is not a string")),
    }
}

fn number(table: &Table, key: &str) -> Result<u64, String> {
    match table.get(key) {
        Some(Value::Integer(n)) => {
            u64::try_from(*n).map_err(|_| format!("the verifier record's {key} is negative"))
        }
        _ => Err(format!("the verifier record's {key} is not an integer")),
    }
}

impl Record {
    /// The release's version, its tag without the `v`.
    #[must_use]
    pub fn version(&self) -> &str {
        self.tag.strip_prefix('v').unwrap_or(&self.tag)
    }

    /// The pin install.sh carries for this release.
    #[must_use]
    pub fn pin(&self) -> Pin {
        let digest = |arch: &str| {
            self.verifiers
                .iter()
                .find(|receipt| receipt.arch == arch)
                .map_or_else(String::new, |receipt| receipt.sha256.clone())
        };
        Pin {
            repo: self.repo.clone(),
            tag: self.tag.clone(),
            x86_64: digest("x86_64"),
            aarch64: digest("aarch64"),
        }
    }

    /// The record as [`RECORD`] holds it.
    #[must_use]
    pub fn to_toml(&self) -> String {
        self.toml_under(
            "# install.sh's bootstrap verifier pin: the receipts of the release it names, as\n\
             # that release's cut froze them. `targo --unverified ship pin-verifier` writes this\n\
             # file and tools/install.sh's pin lines together, and the tests hold each to the\n\
             # other and to RELEASES.ledger. Never edited by hand.\n",
        )
    }

    /// The record's TOML under `header`, its comment lines.
    fn toml_under(&self, header: &str) -> String {
        let mut text = format!(
            "{header}schema = {RECORD_SCHEMA}\nrepo = {:?}\ntag = {:?}\nbuild_number = {}\n\
             commit = {:?}\ninterface = {}\n",
            self.repo, self.tag, self.build_number, self.commit, self.interface
        );
        for receipt in &self.verifiers {
            text.push_str(&format!(
                "\n[[verifier]]\narch = {:?}\nasset = {:?}\nsha256 = {:?}\nsize = {}\n",
                receipt.arch, receipt.asset, receipt.sha256, receipt.size
            ));
        }
        text
    }

    /// The record in `text`, every key known and of its type, then [`Self::validate`]d.
    /// # Errors
    /// Not TOML, a key missing, unknown or mistyped, or a record [`Self::validate`] refuses.
    pub fn parse(text: &str) -> Result<Self, String> {
        let record = Self::fields(text)?;
        record.validate()?;
        Ok(record)
    }

    /// The record in `text`, every key known and of its type.
    fn fields(text: &str) -> Result<Self, String> {
        let table = text.parse::<Table>().map_err(|e| {
            format!(
                "the verifier record is not TOML: {}",
                e.message_text().replace(['\r', '\n'], " ")
            )
        })?;
        exact_keys(
            &table,
            &[
                "schema",
                "repo",
                "tag",
                "build_number",
                "commit",
                "interface",
                "verifier",
            ],
            "the verifier record",
        )?;
        if table.get("schema") != Some(&Value::Integer(RECORD_SCHEMA)) {
            return Err(format!("the verifier record is not schema {RECORD_SCHEMA}"));
        }
        let Some(Value::Array(rows)) = table.get("verifier") else {
            return Err("the verifier record's verifier is not an array of tables".into());
        };
        let verifiers = rows
            .iter()
            .map(|row| {
                let Value::Table(row) = row else {
                    return Err("a verifier of the record is not a table".to_string());
                };
                exact_keys(row, &["arch", "asset", "sha256", "size"], "a verifier")?;
                Ok(Receipt {
                    arch: string(row, "arch")?,
                    asset: string(row, "asset")?,
                    sha256: string(row, "sha256")?,
                    size: number(row, "size")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let record = Self {
            repo: string(&table, "repo")?,
            tag: string(&table, "tag")?,
            build_number: number(&table, "build_number")?,
            commit: string(&table, "commit")?,
            interface: u32::try_from(number(&table, "interface")?)
                .map_err(|_| "the verifier record's interface is out of range".to_string())?,
            verifiers,
        };
        Ok(record)
    }

    /// A claimed release (a build number and a 40-digit commit) published at an `owner/name`
    /// repository, with receipts for one or more of [`ARCHES`], in that order, each the release's
    /// own asset name, a lowercase SHA-256 and a size install.sh downloads.
    /// # Errors
    /// Any other record.
    pub fn validate(&self) -> Result<(), String> {
        self.receipts_hold(&ARCHES, asset_name)?;
        let pin = self.pin();
        if pin.is_empty() {
            return Err("the verifier record pins nothing".into());
        }
        pin.validate()
    }

    /// The claim, the interface, and one receipt each of some of `arches`, in that order, named
    /// `asset(version, arch)`, of a size a script downloads.
    fn receipts_hold(
        &self,
        arches: &[&str],
        asset: fn(&str, &str) -> String,
    ) -> Result<(), String> {
        if self.build_number == 0 || !aterm_codec::hex::is_lower(&self.commit, 40) {
            return Err(format!(
                "the verifier record names no claim (build {}, commit {:?})",
                self.build_number, self.commit
            ));
        }
        if self.interface == 0 {
            return Err("the verifier record's interface is 0".into());
        }
        let order: Vec<Option<usize>> = self
            .verifiers
            .iter()
            .map(|receipt| arches.iter().position(|arch| *arch == receipt.arch))
            .collect();
        if self.verifiers.is_empty()
            || order.iter().any(Option::is_none)
            || order.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(format!(
                "the verifier record's architectures are not one each of {arches:?}, in order"
            ));
        }
        for receipt in &self.verifiers {
            let want = asset(self.version(), &receipt.arch);
            if receipt.asset != want {
                return Err(format!(
                    "the verifier record names {} where {} publishes {want}",
                    receipt.asset, self.tag
                ));
            }
            if !(1..=MAX_ASSET).contains(&receipt.size) {
                return Err(format!(
                    "the verifier record's {} is {} bytes, outside 1..={MAX_ASSET}",
                    receipt.asset, receipt.size
                ));
            }
        }
        Ok(())
    }

    /// Whether `ledger` (RELEASES.ledger's text) records this release's claim:
    /// `<build_number> <version>`.
    /// # Errors
    /// No such line.
    pub fn claimed_in(&self, ledger: &str) -> Result<(), String> {
        let build = self.build_number.to_string();
        let claimed = ledger.lines().any(|line| {
            let mut fields = line.split_whitespace();
            fields.next() == Some(build.as_str())
                && fields.next() == Some(self.version())
                && fields.next().is_none()
        });
        if claimed {
            Ok(())
        } else {
            Err(format!(
                "RELEASES.ledger records no claim `{build} {}`: the verifier record names a \
                 release this tree never claimed",
                self.version()
            ))
        }
    }
}

/// install.sh's pin held to the record committed beside it ([`RECORD`], `record` its text when
/// there is one) and to `ledger`.
/// # Errors
/// A pin with no record, a record with no pin, any difference between them, a record the ledger
/// does not claim, or one of another interface.
pub fn check(install_sh: &str, record: Option<&str>, ledger: &str) -> Result<Pin, String> {
    let pin = Pin::read(install_sh)?;
    let Some(text) = record else {
        return if pin.is_empty() {
            Ok(pin)
        } else {
            Err(format!(
                "install.sh pins {}'s verifier, but {RECORD} is missing: only `ship \
                 pin-verifier`, from that release's receipts, writes a pin",
                pin.tag
            ))
        };
    };
    let record = Record::parse(text)?;
    if pin.is_empty() {
        return Err(format!(
            "{RECORD} records {} but install.sh pins nothing: delete the record, or run `ship \
             pin-verifier` again",
            record.tag
        ));
    }
    if record.pin() != pin {
        return Err(format!(
            "install.sh's pin is not the one {RECORD} records:\n{}— the release's receipts \
             say:\n{}",
            pin.lines(),
            record.pin().lines()
        ));
    }
    record.claimed_in(ledger)?;
    if record.interface != INTERFACE {
        return Err(format!(
            "the pinned {} verifier speaks interface {}, and this tree's install.sh calls \
             interface {INTERFACE}: pin a release built from this tree",
            record.tag, record.interface
        ));
    }
    Ok(pin)
}

/// The architectures get-aterm.ps1 runs a Windows verifier on, as it names them.
pub const WINDOWS_ARCHES: [&str; 1] = ["x86_64"];

/// The release asset of `arch`'s Windows verifier in release `version`.
#[must_use]
pub fn windows_asset_name(version: &str, arch: &str) -> String {
    format!("aterm-bootstrap-verify-{version}-windows-{arch}.exe")
}

/// The command line get-aterm.ps1 calls the Windows verifier with and the line it reads back
/// (`crate::run_windows`). 2: `<dir> <latest> <tag> <slug> <arch> [<zip>]`, answering
/// `crate::windows::NO_ZIP` for a latest release with no zip for `<arch>` and holding an older
/// `<tag>` to the latest release's roster and floors (1 took `<dir> <tag> <slug> <arch> [<zip>]`
/// and asked about the latest release alone).
pub const WINDOWS_INTERFACE: u32 = 2;

/// get-aterm.ps1's names for its pin's three lines, each `$Name = "value"`.
pub const WINDOWS_KEYS: [&str; 3] = [
    "$AtermVerifierRepo",
    "$AtermVerifierTag",
    "$AtermVerifierSha256X86_64",
];

/// Where the record of the release get-aterm.ps1's pin names is committed, from the checkout's
/// root.
pub const WINDOWS_RECORD: &str = "crates/aterm-bootstrap-verify/pinned-windows.toml";

/// get-aterm.ps1's pin: the repository, the release whose Windows verifier it runs, and that
/// verifier's SHA-256. Empty until a release publishes the verifier; the script then installs
/// nothing, since PowerShell has no Ed25519 to check the chain with itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowsPin {
    /// `owner/name`: where the release is, whatever repository the install reads.
    pub repo: String,
    /// `v<version>`.
    pub tag: String,
    pub x86_64: String,
}

impl WindowsPin {
    /// The pin in `get_aterm_ps1`, the script's text: each key on exactly one line of its own,
    /// `$Name = "value"`.
    /// # Errors
    /// A key missing, repeated, or not spelt so.
    pub fn read(get_aterm_ps1: &str) -> Result<Self, String> {
        let values = read_values(get_aterm_ps1, "get-aterm.ps1", &WINDOWS_KEYS, " = ")?;
        let [repo, tag, x86_64] =
            <[String; 3]>::try_from(values).map_err(|_| "three pin values".to_string())?;
        let pin = Self { repo, tag, x86_64 };
        pin.validate()?;
        Ok(pin)
    }

    /// The three lines, as get-aterm.ps1 carries them.
    #[must_use]
    pub fn lines(&self) -> String {
        format!(
            "{} = \"{}\"\n{} = \"{}\"\n{} = \"{}\"\n",
            WINDOWS_KEYS[0], self.repo, WINDOWS_KEYS[1], self.tag, WINDOWS_KEYS[2], self.x86_64
        )
    }

    /// `get_aterm_ps1` with its pin, whatever it is, replaced by this one.
    /// # Errors
    /// The script carries no readable pin, or not as three lines together in order.
    pub fn rewrite(&self, get_aterm_ps1: &str) -> Result<String, String> {
        self.validate()?;
        let lines = Self::read(get_aterm_ps1)?.lines();
        if get_aterm_ps1.matches(&lines).count() != 1 {
            return Err("get-aterm.ps1's pin is not its three lines together, in order".into());
        }
        Ok(get_aterm_ps1.replacen(&lines, &self.lines(), 1))
    }

    /// Whether no release is pinned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.repo.is_empty() && self.tag.is_empty() && self.x86_64.is_empty()
    }

    /// `arch`'s pinned SHA-256, empty when it has none.
    #[must_use]
    pub fn sha256(&self, arch: &str) -> &str {
        match arch {
            "x86_64" => &self.x86_64,
            _ => "",
        }
    }

    /// Empty, or an `owner/name` repository and a canonical release tag with a digest of 64
    /// lowercase hex digits.
    /// # Errors
    /// Any other shape.
    pub fn validate(&self) -> Result<(), String> {
        let digests = WINDOWS_ARCHES.map(|arch| (arch, self.sha256(arch)));
        validate_pin(&self.repo, &self.tag, &digests)
    }
}

/// The receipts of the release get-aterm.ps1's pin names: a [`Record`] of Windows verifiers
/// ([`WINDOWS_ARCHES`], [`windows_asset_name`]), committed at [`WINDOWS_RECORD`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsRecord(pub Record);

impl WindowsRecord {
    /// The pin get-aterm.ps1 carries for this release.
    #[must_use]
    pub fn pin(&self) -> WindowsPin {
        let record = &self.0;
        let digest = |arch: &str| {
            record
                .verifiers
                .iter()
                .find(|receipt| receipt.arch == arch)
                .map_or_else(String::new, |receipt| receipt.sha256.clone())
        };
        WindowsPin {
            repo: record.repo.clone(),
            tag: record.tag.clone(),
            x86_64: digest("x86_64"),
        }
    }

    /// The record as [`WINDOWS_RECORD`] holds it.
    #[must_use]
    pub fn to_toml(&self) -> String {
        self.0.toml_under(
            "# get-aterm.ps1's bootstrap verifier pin: the receipts of the release it names, as\n\
             # that release's signed appcast carries them. `targo --unverified ship pin-verifier\n\
             # --windows --from-release` writes this file and apps/aterm-win/get-aterm.ps1's pin\n\
             # lines together, and the tests hold each to the other and to RELEASES.ledger.\n\
             # Never edited by hand.\n",
        )
    }

    /// The record in `text`, every key known and of its type, then [`Self::validate`]d.
    /// # Errors
    /// Not TOML, a key missing, unknown or mistyped, or a record [`Self::validate`] refuses.
    pub fn parse(text: &str) -> Result<Self, String> {
        let record = Self(Record::fields(text)?);
        record.validate()?;
        Ok(record)
    }

    /// A claimed release published at an `owner/name` repository, with a receipt for each of
    /// some of [`WINDOWS_ARCHES`], in that order, each the release's own asset name, a lowercase
    /// SHA-256 and a size get-aterm.ps1 downloads.
    /// # Errors
    /// Any other record.
    pub fn validate(&self) -> Result<(), String> {
        self.0.receipts_hold(&WINDOWS_ARCHES, windows_asset_name)?;
        let pin = self.pin();
        if pin.is_empty() {
            return Err("the verifier record pins nothing".into());
        }
        pin.validate()
    }
}

/// get-aterm.ps1's pin held to the record committed beside it ([`WINDOWS_RECORD`], `record` its
/// text when there is one) and to `ledger`, as [`check`] holds install.sh's.
/// # Errors
/// A pin with no record, a record with no pin, any difference between them, a record the ledger
/// does not claim, or one of another interface.
pub fn check_windows(
    get_aterm_ps1: &str,
    record: Option<&str>,
    ledger: &str,
) -> Result<WindowsPin, String> {
    let pin = WindowsPin::read(get_aterm_ps1)?;
    let Some(text) = record else {
        return if pin.is_empty() {
            Ok(pin)
        } else {
            Err(format!(
                "get-aterm.ps1 pins {}'s verifier, but {WINDOWS_RECORD} is missing: only the \
                 release's receipts, written with the record, make a pin",
                pin.tag
            ))
        };
    };
    let record = WindowsRecord::parse(text)?;
    if pin.is_empty() {
        return Err(format!(
            "{WINDOWS_RECORD} records {} but get-aterm.ps1 pins nothing: delete the record, or \
             write the pin from it again",
            record.0.tag
        ));
    }
    if record.pin() != pin {
        return Err(format!(
            "get-aterm.ps1's pin is not the one {WINDOWS_RECORD} records:\n{}- the release's \
             receipts say:\n{}",
            pin.lines(),
            record.pin().lines()
        ));
    }
    record.0.claimed_in(ledger)?;
    if record.0.interface != WINDOWS_INTERFACE {
        return Err(format!(
            "the pinned {} verifier speaks interface {}, and this tree's get-aterm.ps1 calls \
             interface {WINDOWS_INTERFACE}: pin a release built from this tree",
            record.0.tag, record.0.interface
        ));
    }
    Ok(pin)
}

/// Where get-aterm.ps1 is, from the checkout's root.
pub const WINDOWS_SCRIPT: &str = "apps/aterm-win/get-aterm.ps1";

/// One architecture's verifier as a signed appcast carries it: its asset name, SHA-256 and
/// size, each `None` when the appcast names it not.
pub type SignedFields<'a> = (Option<&'a str>, Option<&'a str>, Option<u64>);

/// The Windows verifiers the signed appcast of release `version` names, as the receipts a
/// [`WindowsRecord`] holds, and the interface they speak: `signed` is each of
/// [`WINDOWS_ARCHES`]' `windows_verifier_<arch>`, `_sha256` and `_size` in that order, and
/// `interface` its `windows_verifier_interface`. `None` when it names none. These signed fields
/// are what binds get-aterm.ps1's pin to the release signer, as the `linux_verifier_*` fields
/// bind install.sh's.
///
/// # Errors
/// An architecture with some of its three fields and not the others, a name that is not
/// [`windows_asset_name`]'s for `version`, a digest that is not 64 lowercase hex digits, a
/// size outside `64..=MAX_ASSET`, or receipts with no interface (or an interface with none).
pub fn signed_windows_receipts(
    version: &str,
    signed: [SignedFields<'_>; WINDOWS_ARCHES.len()],
    interface: Option<u32>,
) -> Result<Option<(Vec<Receipt>, u32)>, String> {
    let mut receipts = Vec::new();
    for (arch, fields) in WINDOWS_ARCHES.iter().zip(signed) {
        let (asset, sha256, size) = match fields {
            (None, None, None) => continue,
            (Some(asset), Some(sha256), Some(size)) => (asset, sha256, size),
            _ => {
                return Err(format!(
                    "the signed appcast's Windows {arch} verifier is incomplete: its name, \
                     sha256 and size appear together or not at all"
                ));
            }
        };
        if asset != windows_asset_name(version, arch) {
            return Err(format!(
                "the signed appcast names the Windows {arch} verifier {asset:?}, not \
                 v{version}'s canonical asset"
            ));
        }
        if !aterm_codec::hex::is_lower(sha256, 64) {
            return Err(format!(
                "the signed appcast's Windows {arch} verifier digest {sha256:?} is not 64 \
                 lowercase hex digits"
            ));
        }
        if !(64..=MAX_ASSET).contains(&size) {
            return Err(format!(
                "the signed appcast's Windows {arch} verifier is {size} bytes, outside \
                 64..={MAX_ASSET}"
            ));
        }
        receipts.push(Receipt {
            arch: (*arch).to_string(),
            asset: asset.to_string(),
            sha256: sha256.to_string(),
            size,
        });
    }
    match (receipts.is_empty(), interface) {
        (true, None) => Ok(None),
        (false, Some(interface)) => Ok(Some((receipts, interface))),
        (true, Some(_)) => {
            Err("the signed appcast names a Windows verifier interface but no verifier".into())
        }
        (false, None) => Err("the signed appcast names a Windows verifier but no interface".into()),
    }
}

/// `bytes` begin as get-aterm.ps1's verifier must: a PE32+ image for x86-64 (`MZ`, the
/// `PE\0\0` signature its `e_lfanew` names, machine `IMAGE_FILE_MACHINE_AMD64`, optional
/// header magic `0x20b`) that is an executable and not a DLL, for the console subsystem, whose
/// standard output get-aterm.ps1 reads.
///
/// # Errors
/// The first of those it is not.
pub fn windows_verifier_image(bytes: &[u8]) -> Result<(), String> {
    let u16_at = |at: usize| {
        bytes
            .get(at..at.checked_add(2)?)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |at: usize| {
        bytes
            .get(at..at.checked_add(4)?)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if bytes.get(..2) != Some(&b"MZ"[..]) {
        return Err("the Windows verifier is not a Windows executable (no MZ header)".into());
    }
    let pe = u32_at(0x3c)
        .and_then(|at| usize::try_from(at).ok())
        .filter(|at| bytes.get(*at..at.saturating_add(4)) == Some(&b"PE\0\0"[..]))
        .ok_or("the Windows verifier names no PE header")?;
    let field = |offset: usize, what: &str| {
        pe.checked_add(offset)
            .and_then(u16_at)
            .ok_or_else(|| format!("the Windows verifier's PE header ends before its {what}"))
    };
    if field(4, "machine")? != 0x8664 {
        return Err("the Windows verifier is not an x86-64 image".into());
    }
    let characteristics = field(22, "characteristics")?;
    if (characteristics & 0x0002) == 0 || (characteristics & 0x2000) != 0 {
        return Err("the Windows verifier is not an executable image (or is a DLL)".into());
    }
    if field(20, "optional header size")? < 70 || field(24, "optional header")? != 0x20b {
        return Err("the Windows verifier is not a PE32+ image".into());
    }
    if field(24 + 68, "subsystem")? != 3 {
        return Err("the Windows verifier is not a console program".into());
    }
    Ok(())
}

/// `get_aterm_ps1` (get-aterm.ps1's text) pinned to `record`'s release, and the record as
/// [`WINDOWS_RECORD`] holds it: the pair [`check_windows`] holds to each other and to
/// `ledger` (RELEASES.ledger's text), which the release tooling writes together.
///
/// # Errors
/// A record [`WindowsRecord::validate`] refuses, one `ledger` does not claim, one of another
/// interface, or a script whose pin it cannot place.
pub fn windows_pin_files(
    record: &WindowsRecord,
    get_aterm_ps1: &str,
    ledger: &str,
) -> Result<(String, String), String> {
    record.validate()?;
    record.0.claimed_in(ledger)?;
    let script = record.pin().rewrite(get_aterm_ps1)?;
    let body = record.to_toml();
    check_windows(&script, Some(&body), ledger)?;
    Ok((script, body))
}

#[cfg(test)]
#[path = "pin_tests.rs"]
mod tests;
