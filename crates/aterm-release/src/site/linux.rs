// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux release asset verification through the signed roster and appcast chain.

use aterm_update_core::linux::LinuxTarget;
use aterm_update_core::{Manifest, Roster, verify_roster};

use super::releases::{LinuxDownload, LinuxKind, LinuxProver, Release, linux_appcast_name};

const APPCAST_LIMIT: u64 = 5_000_000;
const SIG_LIMIT: u64 = 64;
const ROSTER_LIMIT: u64 = 65_536;

/// A [`LinuxProver`] over the published proof assets, read through `get`.
pub struct Chain<'a> {
    pub slug: &'a str,
    /// The paper master keyset (`pins::PAPER_MASTER_PUBKEYS`; a test pins its own).
    pub masters: &'a [&'a str],
    pub now_unix: i64,
    /// A release asset's bytes at `url`, at most `limit` of them.
    pub get: &'a mut super::Fetch<'a>,
}

impl Chain<'_> {
    fn fetch(&mut self, r: &Release, name: &str, limit: u64) -> Result<Vec<u8>, String> {
        if r.named(name).is_none() {
            return Err(format!("missing or duplicate {name}"));
        }
        let bytes = (self.get)(&r.download_url(self.slug, name), limit)?;
        if bytes.len() as u64 > limit {
            return Err(format!("oversized {name}"));
        }
        Ok(bytes)
    }

    fn appcast(
        &self,
        roster: &Roster,
        bytes: &[u8],
        sig: &[u8],
        tag: &str,
    ) -> Result<Manifest, String> {
        let signer = roster
            .authorize_appcast(bytes, sig, self.now_unix)
            .map_err(|e| format!("appcast has no authorized live machine signature ({e:?})"))?;
        let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
        let manifest = Manifest::parse(text)?;
        signer
            .bind(manifest.machine_id.as_deref(), manifest.roster_seq)
            .map_err(|e| format!("appcast signer attribution mismatch ({e:?})"))?;
        let version = tag.strip_prefix('v').unwrap_or(tag);
        if manifest.version != version {
            return Err("release tag/appcast version mismatch".into());
        }
        let commit_ok = manifest.commit.as_deref().is_some_and(|c| {
            c.len() == 40 && c.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        if !commit_ok {
            return Err("invalid source commit".into());
        }
        let dmg = super::releases::dmg_name(version);
        let url = format!(
            "https://github.com/{}/releases/download/{tag}/{dmg}",
            self.slug
        );
        if manifest.dmg != dmg || manifest.url.as_deref() != Some(url.as_str()) {
            return Err("signed release origin mismatch".into());
        }
        if manifest.build_number == 0 {
            return Err("zero release build".into());
        }
        Ok(manifest)
    }
}

impl LinuxProver for Chain<'_> {
    fn prove(
        &mut self,
        release: &Release,
        arch: &'static str,
        authority: &Release,
    ) -> Result<LinuxDownload, String> {
        let target = match arch {
            "x86_64" => LinuxTarget::X86_64,
            "aarch64" => LinuxTarget::Aarch64,
            other => return Err(format!("unsupported Linux architecture {other}")),
        };
        let proof = linux_appcast_name(release)
            .ok_or("native release has no current appcast or exact archived pair")?;
        if linux_appcast_name(authority).as_deref() != Some("aterm-appcast.toml") {
            return Err("an archived appcast cannot supply current policy".into());
        }
        if release.version_key() > authority.version_key() {
            return Err("native release is newer than the current appcast head".into());
        }
        let roster_bytes = self.fetch(authority, "aterm-machines.toml", ROSTER_LIMIT)?;
        let roster_sig = self.fetch(authority, "aterm-machines.toml.sig", SIG_LIMIT)?;
        let verified = verify_roster(self.masters, roster_bytes, &roster_sig)
            .map_err(|e| format!("roster is not signed by the pinned paper master ({e:?})"))?;
        let roster = Roster::parse(&verified).map_err(|e| format!("roster refused ({e:?})"))?;
        roster
            .admit(0, self.now_unix)
            .map_err(|e| format!("roster refused ({e:?})"))?;
        let head = self.fetch(authority, "aterm-appcast.toml", APPCAST_LIMIT)?;
        let head_sig = self.fetch(authority, "aterm-appcast.toml.sig", SIG_LIMIT)?;
        let current = self.appcast(&roster, &head, &head_sig, &authority.tag)?;
        let manifest = if release.tag == authority.tag {
            current.clone()
        } else {
            let bytes = self.fetch(release, &proof, APPCAST_LIMIT)?;
            let sig = self.fetch(release, &format!("{proof}.sig"), SIG_LIMIT)?;
            self.appcast(&roster, &bytes, &sig, &release.tag)?
        };
        let floor = current.min_build.unwrap_or(0);
        if manifest.build_number < floor.max(manifest.min_build.unwrap_or(0)) {
            return Err("release build rollback or below apply floor".into());
        }
        let artifact = manifest
            .linux_artifact(target)?
            .ok_or_else(|| format!("signed release has no Linux artifact for {arch}"))?;
        let listed = release.named(artifact.name).and_then(|a| a.size);
        if listed != Some(artifact.size) {
            return Err("signed Linux name/size does not match release inventory".into());
        }
        Ok(LinuxDownload {
            tag: release.tag.clone(),
            version: release.version().into(),
            label: artifact.name.into(),
            url: release.download_url(self.slug, artifact.name),
            kind: LinuxKind::SignedRaw {
                sha256: artifact.sha256.into(),
                size: artifact.size,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::releases::{Asset, LINUX_ARCHES, elect};
    use ring::signature::{Ed25519KeyPair, KeyPair as _};
    use std::collections::BTreeMap;

    const SLUG: &str = "alabsystems/aterm";
    const NOW: i64 = 1_785_801_600;
    const MASTER: [u8; 32] = [0x61; 32];
    const MACHINE: [u8; 32] = [0x62; 32];
    const OTHER: [u8; 32] = [0x63; 32];

    fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
    }

    fn pk(seed: &[u8; 32]) -> String {
        aterm_codec::base64::encode(kp(seed).public_key().as_ref()).unwrap()
    }

    fn sign(seed: &[u8; 32], bytes: &[u8]) -> Vec<u8> {
        kp(seed).sign(bytes).as_ref().to_vec()
    }

    fn roster() -> Vec<u8> {
        format!(
            "schema = 1\nroster_seq = 3\nvalid_until = \"9999-12-31T23:59:59Z\"\nrevoked = []\n\n\
             [[machine]]\nid = \"m3\"\npubkey = \"{}\"\nadded_at = \"2026-08-01T00:00:00Z\"\n",
            pk(&MACHINE)
        )
        .into_bytes()
    }

    fn appcast(v: &str, build: u64, min: u64, size: u64) -> Vec<u8> {
        format!(
            "schema = 1\nversion = \"{v}\"\nbuild_number = {build}\ncommit = \"{c}\"\n\
             dmg = \"aterm-{v}.dmg\"\nsha256 = \"{h}\"\n\
             url = \"https://github.com/{SLUG}/releases/download/v{v}/aterm-{v}.dmg\"\n\
             linux_aarch64 = \"aterm-{v}-linux-aarch64\"\nlinux_aarch64_sha256 = \"{h}\"\n\
             linux_aarch64_size = {size}\nmin_build = {min}\nmachine_id = \"m3\"\nroster_seq = 3\n",
            c = "a".repeat(40),
            h = "b".repeat(64),
        )
        .into_bytes()
    }

    fn release(
        served: &mut BTreeMap<String, Vec<u8>>,
        v: &str,
        cast: &[u8],
        signer: &[u8; 32],
        listed: u64,
        current: bool,
    ) -> Release {
        let tag = format!("v{v}");
        let proof = if current {
            "aterm-appcast.toml".to_string()
        } else {
            format!("aterm-appcast-{tag}.toml")
        };
        let mut names = vec![
            (format!("aterm-{v}.dmg"), 1),
            (format!("aterm-{v}-mac.zip"), 1),
            (format!("aterm-{v}-linux-aarch64"), listed),
            (proof.clone(), cast.len() as u64),
            (format!("{proof}.sig"), 64),
        ];
        let url = |n: &str| format!("https://github.com/{SLUG}/releases/download/{tag}/{n}");
        served.insert(url(&proof), cast.to_vec());
        served.insert(url(&format!("{proof}.sig")), sign(signer, cast));
        if current {
            let r = roster();
            served.insert(url("aterm-machines.toml.sig"), sign(&MASTER, &r));
            served.insert(url("aterm-machines.toml"), r);
            names.push(("aterm-machines.toml".into(), 1));
            names.push(("aterm-machines.toml.sig".into(), 64));
        }
        Release {
            tag,
            prerelease: false,
            draft: false,
            date: String::new(),
            body: String::new(),
            assets: names
                .into_iter()
                .map(|(name, size)| Asset {
                    name,
                    size: Some(size),
                })
                .collect(),
        }
    }

    fn run(
        served: &BTreeMap<String, Vec<u8>>,
        releases: &[Release],
    ) -> Result<Option<LinuxDownload>, String> {
        elected(served, releases).map(|f| f.linux(LINUX_ARCHES[1]).cloned())
    }

    fn elected(
        served: &BTreeMap<String, Vec<u8>>,
        releases: &[Release],
    ) -> Result<crate::site::releases::Facts, String> {
        let master = pk(&MASTER);
        let masters = [master.as_str()];
        let mut get = |url: &str, _limit: u64| {
            served
                .get(url)
                .cloned()
                .ok_or_else(|| format!("not served: {url}"))
        };
        let mut chain = Chain {
            slug: SLUG,
            masters: &masters,
            now_unix: NOW,
            get: &mut get,
        };
        elect(releases, SLUG, &mut chain, &mut Vec::new()).map_err(|e| e.to_string())
    }

    #[test]
    fn a_signed_executable_of_the_head_is_linked_with_its_digest_and_size() {
        let mut served = BTreeMap::new();
        let head = release(
            &mut served,
            "0.64.0",
            &appcast("0.64.0", 20, 0, 9000),
            &MACHINE,
            9000,
            true,
        );
        let d = run(&served, std::slice::from_ref(&head)).unwrap().unwrap();
        assert_eq!(d.label, "aterm-0.64.0-linux-aarch64");
        assert_eq!(
            d.kind,
            LinuxKind::SignedRaw {
                sha256: "b".repeat(64),
                size: 9000
            }
        );
        let facts = elected(&served, &[head]).unwrap();
        assert_eq!(
            crate::site::pages::linux_install_command(&facts, "aarch64").as_deref(),
            Some("curl -fsSL https://alab.systems/install.sh | bash")
        );
        assert_eq!(
            crate::site::pages::linux_install_command(&facts, "x86_64"),
            None
        );
    }

    #[test]
    fn an_older_executable_is_authorized_under_the_current_policy() {
        let mut served = BTreeMap::new();
        let mut head_cast = String::from_utf8(appcast("0.65.0", 21, 0, 9000)).unwrap();
        head_cast = head_cast
            .lines()
            .filter(|l| !l.starts_with("linux_"))
            .map(|l| format!("{l}\n"))
            .collect();
        let mut head = release(
            &mut served,
            "0.65.0",
            head_cast.as_bytes(),
            &MACHINE,
            1,
            true,
        );
        head.assets
            .retain(|a| a.name != "aterm-0.65.0-linux-aarch64");
        let old = release(
            &mut served,
            "0.64.0",
            &appcast("0.64.0", 20, 0, 9000),
            &MACHINE,
            9000,
            false,
        );
        let d = run(&served, &[head.clone(), old.clone()]).unwrap().unwrap();
        assert_eq!(d.tag, "v0.64.0");
        let facts = elected(&served, &[head.clone(), old]).unwrap();
        assert_eq!(facts.head, "v0.65.0");
        assert_eq!(
            crate::site::pages::linux_install_command(&facts, "aarch64").as_deref(),
            Some("curl -fsSL https://alab.systems/install.sh | bash -s -- --version 0.64.0")
        );
        let floor_cast = head_cast.replace("min_build = 0", "min_build = 21");
        let mut served2 = BTreeMap::new();
        let mut head = release(
            &mut served2,
            "0.65.0",
            floor_cast.as_bytes(),
            &MACHINE,
            1,
            true,
        );
        head.assets
            .retain(|a| a.name != "aterm-0.65.0-linux-aarch64");
        let old = release(
            &mut served2,
            "0.64.0",
            &appcast("0.64.0", 20, 0, 9000),
            &MACHINE,
            9000,
            false,
        );
        let err = run(&served2, &[head, old]).unwrap_err();
        assert!(err.contains("below apply floor"), "{err}");
    }

    #[test]
    fn a_signature_by_no_rostered_machine_refuses_the_sync() {
        let mut served = BTreeMap::new();
        let head = release(
            &mut served,
            "0.64.0",
            &appcast("0.64.0", 20, 0, 9000),
            &OTHER,
            9000,
            true,
        );
        let err = run(&served, &[head]).unwrap_err();
        assert!(
            err.contains("v0.64.0 Linux aarch64 signature/asset verification refused"),
            "{err}"
        );
        assert!(
            err.contains("no authorized live machine signature"),
            "{err}"
        );
    }

    #[test]
    fn a_listed_size_the_appcast_does_not_sign_refuses_the_sync() {
        let mut served = BTreeMap::new();
        let head = release(
            &mut served,
            "0.64.0",
            &appcast("0.64.0", 20, 0, 9000),
            &MACHINE,
            9001,
            true,
        );
        let err = run(&served, &[head]).unwrap_err();
        assert!(err.contains("does not match release inventory"), "{err}");
    }

    #[test]
    fn a_roster_under_another_master_refuses_the_sync() {
        let mut served = BTreeMap::new();
        let head = release(
            &mut served,
            "0.64.0",
            &appcast("0.64.0", 20, 0, 9000),
            &MACHINE,
            9000,
            true,
        );
        let url =
            format!("https://github.com/{SLUG}/releases/download/v0.64.0/aterm-machines.toml.sig");
        served.insert(url, sign(&OTHER, &roster()));
        let err = run(&served, &[head]).unwrap_err();
        assert!(
            err.contains("not signed by the pinned paper master"),
            "{err}"
        );
    }
}
