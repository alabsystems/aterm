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

impl Pin {
    /// The pin in `install_sh`, the script's text: each key on exactly one line of its own,
    /// `KEY="value"`.
    /// # Errors
    /// A key missing, repeated, or not spelt so.
    pub fn read(install_sh: &str) -> Result<Self, String> {
        let mut values = Vec::with_capacity(KEYS.len());
        for key in KEYS {
            let prefix = format!("{key}=");
            let lines: Vec<&str> = install_sh
                .lines()
                .filter(|line| line.starts_with(&prefix))
                .collect();
            let [line] = lines.as_slice() else {
                return Err(format!(
                    "install.sh must set {key} on exactly one line of its own ({} found)",
                    lines.len()
                ));
            };
            let value = line[prefix.len()..]
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .filter(|value| !value.contains(['"', '$', '`', '\\']))
                .ok_or_else(|| {
                    format!("install.sh's {key} is not a plain double-quoted word: {line}")
                })?;
            values.push(value.to_string());
        }
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
        if self.is_empty() {
            return Ok(());
        }
        if !repository(&self.repo) {
            return Err(format!(
                "the verifier pin's repository {:?} is not owner/name",
                self.repo
            ));
        }
        if !canonical_tag(&self.tag) {
            return Err(format!(
                "the verifier pin's tag {:?} is not v<MAJOR>.<MINOR>.<PATCH>",
                self.tag
            ));
        }
        for arch in ARCHES {
            let digest = self.sha256(arch);
            if !digest.is_empty() && !aterm_codec::hex::is_lower(digest, 64) {
                return Err(format!(
                    "the verifier pin's {arch} SHA-256 {digest:?} is not 64 lowercase hex digits"
                ));
            }
        }
        if ARCHES.iter().all(|arch| self.sha256(arch).is_empty()) {
            return Err(format!("the verifier pin names {} but no digest", self.tag));
        }
        Ok(())
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
        let mut text = format!(
            "# install.sh's bootstrap verifier pin: the receipts of the release it names, as\n\
             # that release's cut froze them. `targo --unverified ship pin-verifier` writes this\n\
             # file and tools/install.sh's pin lines together, and the tests hold each to the\n\
             # other and to RELEASES.ledger. Never edited by hand.\n\
             schema = {RECORD_SCHEMA}\nrepo = {:?}\ntag = {:?}\nbuild_number = {}\n\
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
        record.validate()?;
        Ok(record)
    }

    /// A claimed release (a build number and a 40-digit commit) published at an `owner/name`
    /// repository, with receipts for one or more of [`ARCHES`], in that order, each the release's
    /// own asset name, a lowercase SHA-256 and a size install.sh downloads.
    /// # Errors
    /// Any other record.
    pub fn validate(&self) -> Result<(), String> {
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
            .map(|receipt| ARCHES.iter().position(|arch| *arch == receipt.arch))
            .collect();
        if self.verifiers.is_empty()
            || order.iter().any(Option::is_none)
            || order.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(format!(
                "the verifier record's architectures are not one each of {ARCHES:?}, in order"
            ));
        }
        for receipt in &self.verifiers {
            let want = asset_name(self.version(), &receipt.arch);
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
        let pin = self.pin();
        if pin.is_empty() {
            return Err("the verifier record pins nothing".into());
        }
        pin.validate()
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

#[cfg(test)]
#[path = "pin_tests.rs"]
mod tests;
