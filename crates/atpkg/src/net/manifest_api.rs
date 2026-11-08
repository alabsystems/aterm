// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Exact package-metadata acquisition through the existing native release owner.

use agh::download::{Expect, Sink};
use agh::model::{AssetObject, ReleaseObject};
use agh::{Github, GithubBuilder, Slug};

use super::{MANIFEST_CAP, SIG_CAP, manifest_assets};

pub(super) fn fetch(slug: &str, program: &str, build: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
    // Preserve credential-less package discovery: no identity configuration or
    // vault is adopted. Default How::read uses the API, including its anonymous
    // read route; How::anonymous would instead select the browser download URL.
    let gh = GithubBuilder::new(agh::policy::Router::empty()).build();
    fetch_with(&gh, slug, program, build)
}

fn selected<'a>(
    release: &'a ReleaseObject,
    tag: &str,
    name: &str,
    cap: u64,
) -> Result<&'a AssetObject, String> {
    if release.tag_name != tag || release.draft {
        return Err(format!("expected published package release {tag}"));
    }
    let mut matches = release.assets.iter().filter(|asset| asset.name == name);
    let asset = matches
        .next()
        .ok_or_else(|| format!("package release {tag} has no {name}"))?;
    if matches.next().is_some() {
        return Err(format!("package release {tag} has multiple {name} assets"));
    }
    if !asset.is_uploaded() || asset.size == 0 || asset.size > cap {
        return Err(format!(
            "package asset {name} is not uploaded or exceeds its {cap}-byte bound"
        ));
    }
    Ok(asset)
}

fn fetch_with(
    gh: &Github,
    slug: &str,
    program: &str,
    build: u64,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let (tag, name, sig) = manifest_assets(program, build)
        .ok_or_else(|| "invalid package metadata identity".to_string())?;
    // Keep the same safe slug spelling admitted by the browser URL derivation.
    super::direct_manifest_urls(slug, program, build)
        .ok_or_else(|| "invalid package repository identity".to_string())?;
    let slug = Slug::parse(slug).map_err(|error| error.why.clone())?;
    let releases = gh.releases(&slug);
    let release = releases
        .published_by_tag(&tag)
        .map_err(|error| format!("package release {slug} tag {tag}: {}", error.why))?;
    // Validate both catalog entries before downloading either half.
    let toml_asset = selected(&release, &tag, &name, MANIFEST_CAP)?;
    let sig_asset = selected(&release, &tag, &sig, SIG_CAP)?;
    if toml_asset.id == sig_asset.id {
        return Err("package metadata assets have the same identity".to_string());
    }
    let download = |asset: &AssetObject, cap| {
        let context = |error: agh::Error| {
            format!(
                "package asset {slug} tag {tag} name {}: {}",
                asset.name, error.why
            )
        };
        let expect = Expect::from_asset(asset).map_err(context)?;
        // The native owner enforces uploaded state, exact size, its digest (or
        // the existing bracket recheck), protected redirects and the byte cap.
        releases
            .download(asset, Sink::Bounded(cap), expect)
            .map_err(context)?
            .data
            .ok_or_else(|| "package metadata download returned no bytes".to_string())
    };
    Ok((
        download(toml_asset, MANIFEST_CAP)?,
        download(sig_asset, SIG_CAP)?,
    ))
}

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::*;
    use agh_fake::{AssetSpec, DigestSpec, FakeGithub, FakeOrigin, ReleaseSpec, RepoSpec};

    fn world() -> (FakeGithub, Github, u64) {
        let fake = FakeGithub::start();
        fake.add_repo("fixture/app", RepoSpec::public());
        let id = fake.add_release(
            "fixture/app",
            ReleaseSpec::published("atpkg-p-1", "2026-11-08T00:00:00Z"),
        );
        let ports = fake.ports();
        let gh = GithubBuilder::new(agh::policy::Router::empty())
            .endpoints(agh::wire::Endpoints::loopback(
                ports.api,
                ports.uploads,
                ports.web,
                ports.cdn,
            ))
            .env(std::sync::Arc::new(agh::identity::MapEnv::new()))
            .retry(agh::retry::RetryPolicy::NONE)
            .build();
        (fake, gh, id)
    }

    #[test]
    fn exact_pair_uses_native_api_and_cdn_without_credentials_or_web() {
        let (fake, gh, id) = world();
        fake.add_asset(id, AssetSpec::uploaded("pkg-p-1.toml", b"raw toml"));
        fake.add_asset(id, AssetSpec::uploaded("pkg-p-1.toml.sig", b"raw sig"));
        assert_eq!(
            fetch_with(&gh, "fixture/app", "p", 1).unwrap(),
            (b"raw toml".to_vec(), b"raw sig".to_vec())
        );
        let seen = fake.seen();
        assert!(
            seen.iter()
                .any(|row| row.path_and_query.contains("/releases/assets/"))
        );
        assert!(
            seen.iter()
                .all(|row| row.origin != FakeOrigin::Web && !row.had_authorization)
        );
    }

    #[test]
    fn native_digest_mismatch_is_refused() {
        let (fake, gh, id) = world();
        let mut asset = AssetSpec::uploaded("pkg-p-1.toml", b"tampered bytes");
        asset.digest = DigestSpec::Wrong;
        fake.add_asset(id, asset);
        fake.add_asset(id, AssetSpec::uploaded("pkg-p-1.toml.sig", b"raw sig"));
        assert!(fetch_with(&gh, "fixture/app", "p", 1).is_err());
    }

    #[test]
    fn catalog_refuses_missing_duplicate_wrong_release_state_and_caps_before_transfer() {
        let (fake, gh, id) = world();
        fake.add_asset(id, AssetSpec::uploaded("pkg-p-1.toml", b"raw toml"));
        fake.add_asset(id, AssetSpec::uploaded("pkg-p-1.toml.sig", b"raw sig"));
        let slug = Slug::parse("fixture/app").unwrap();
        let original = gh.releases(&slug).published_by_tag("atpkg-p-1").unwrap();
        let name = "pkg-p-1.toml";
        for case in 0..7 {
            let mut release = original.clone();
            match case {
                0 => release.assets.retain(|asset| asset.name != name),
                1 => release.assets.push(release.assets[0].clone()),
                2 => release.tag_name = "atpkg-p-2".into(),
                3 => release.draft = true,
                4 => release.assets[0].state = "starter".into(),
                5 => release.assets[0].size = MANIFEST_CAP + 1,
                6 => release.assets[0].size = 0,
                _ => unreachable!(),
            }
            assert!(
                selected(&release, "atpkg-p-1", name, MANIFEST_CAP).is_err(),
                "case {case}"
            );
        }
        let mut release = original;
        release.assets[1].size = SIG_CAP + 1;
        assert!(selected(&release, "atpkg-p-1", "pkg-p-1.toml.sig", SIG_CAP).is_err());
    }
}
