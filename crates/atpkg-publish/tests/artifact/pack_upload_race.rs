// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Deterministic controls for immutable staging admission. These are local
//! data-only FakeHost operations, with no threads, clocks or publication service.

use super::*;
use atpkg_publish::host::{HostError, Probe, Release, ReleaseHost, Replace};
use std::sync::Mutex;

struct BeforeUpload {
    inner: Arc<FakeHost>,
    once: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    when: Replace,
    before_create: bool,
    reservation_fault: ReservationFault,
    payload_order: Arc<Mutex<Vec<Vec<String>>>>,
    created_titles: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone, Copy, Default)]
enum ReservationFault {
    #[default]
    None,
    LoseAck,
    BeforeWrite,
}

impl BeforeUpload {
    fn over(inner: Arc<FakeHost>) -> Self {
        Self {
            inner,
            once: Mutex::new(None),
            when: Replace::Clobber,
            before_create: false,
            reservation_fault: ReservationFault::None,
            payload_order: Arc::default(),
            created_titles: Arc::default(),
        }
    }
}

impl ReleaseHost for BeforeUpload {
    fn releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError> {
        self.inner.releases(slug)
    }

    fn release(&self, slug: &Slug, tag: &str) -> Result<Release, HostError> {
        self.inner.release(slug, tag)
    }

    fn download(
        &self,
        slug: &Slug,
        tag: &str,
        assets: &[&str],
        dir: &Path,
    ) -> Result<(), HostError> {
        self.inner.download(slug, tag, assets, dir)
    }

    fn create(
        &self,
        slug: &Slug,
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> Result<(), HostError> {
        self.created_titles.lock().unwrap().push(title.to_string());
        if self.before_create {
            let once = self.once.lock().unwrap().take();
            if let Some(once) = once {
                once();
            }
        }
        self.inner.create(slug, tag, title, notes, files)
    }

    fn upload(
        &self,
        slug: &Slug,
        tag: &str,
        files: &[PathBuf],
        replace: Replace,
    ) -> Result<(), HostError> {
        if replace == Replace::Clobber {
            self.payload_order.lock().unwrap().push(
                files
                    .iter()
                    .map(|file| file.file_name().unwrap().to_string_lossy().into_owned())
                    .collect(),
            );
        }
        let once = if replace == self.when {
            self.once.lock().unwrap().take()
        } else {
            None
        };
        if let Some(once) = once {
            once();
        }
        if replace == Replace::Never
            && matches!(self.reservation_fault, ReservationFault::BeforeWrite)
        {
            return Err(HostError::Unreachable(
                "reservation never reached the host".to_string(),
            ));
        }
        let result = self.inner.upload(slug, tag, files, replace);
        if replace == Replace::Never
            && result.is_ok()
            && matches!(self.reservation_fault, ReservationFault::LoseAck)
        {
            return Err(HostError::Unreachable(
                "reservation committed; its ACK was lost".to_string(),
            ));
        }
        result
    }

    fn publish_draft(&self, slug: &Slug, tag: &str) -> Result<(), HostError> {
        self.inner.publish_draft(slug, tag)
    }

    fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Probe {
        self.inner.probe_public(slug, tag, asset)
    }

    fn fetch_public(&self, slug: &Slug, tag: &str, asset: &str, dest: &Path, cap: u64) -> Probe {
        self.inner.fetch_public(slug, tag, asset, dest, cap)
    }

    fn clone_repo(&self, slug: &Slug, dest: &Path) -> Result<(), HostError> {
        self.inner.clone_repo(slug, dest)
    }

    fn fetch_repo(&self, slug: &Slug, dir: &Path) -> Result<(), HostError> {
        self.inner.fetch_repo(slug, dir)
    }

    fn repo_seen(&self, slug: &Slug) -> Result<bool, HostError> {
        self.inner.repo_seen(slug)
    }
}

fn pack_files(w: &World, directory: &str, version: &str) -> (PathBuf, Vec<PathBuf>) {
    let dir = w.root.join(directory);
    let manifest = dir.join("pkg-ay-9.toml");
    put(&manifest, format!("# {directory}\nschema = 1\nprogram = \"ay\"\nversion = \"{version}\"\nbuild_number = 9\n").as_bytes());
    let archive = dir.join("ay-9-aarch64-apple-darwin.tar.zst");
    put(&archive, directory.as_bytes());
    (manifest.clone(), vec![archive, manifest])
}

fn empty_release(w: &World) -> Slug {
    let slug = w.ctx.staging_slug("ay");
    w.host.put(
        &slug,
        "atpkg-ay-9",
        FakeRelease {
            prerelease: true,
            ..FakeRelease::default()
        },
    );
    slug
}

#[test]
fn managed_pack_titles_lead_with_the_exact_product_version() {
    let w = world("human-pack-titles");
    for (program, repo, build, version, title) in [
        ("ay", "ay", 9, "0.27.0", "ay 0.27.0 (build 9)"),
        (
            "codex",
            "codex-alab",
            2026110901,
            "0.2.0-dev.15239+codex.0.167.0.g022ae520c",
            "codex-alab 0.2.0-dev.15239+codex.0.167.0.g022ae520c (build 2026110901)",
        ),
    ] {
        let dir = w.root.join(format!("title-{program}"));
        let manifest_name = format!("pkg-{program}-{build}.toml");
        let manifest = dir.join(&manifest_name);
        let manifest_bytes = format!(
            "schema = 1\nprogram = \"{program}\"\nversion = \"{version}\"\nbuild_number = {build}\n"
        )
        .into_bytes();
        put(&manifest, &manifest_bytes);
        let archive_name = format!("{program}-{build}.tar.zst");
        let archive = dir.join(&archive_name);
        let payload = b"the qualified payload is unchanged";
        put(&archive, payload);
        let wrapper = BeforeUpload::over(Arc::clone(&w.host));
        let titles = Arc::clone(&wrapper.created_titles);
        let mut ctx = w.ctx.clone();
        ctx.hosts.staging = Arc::new(wrapper);
        let tag = upload::tag(program, build);

        assert_eq!(
            upload::publish(
                &ctx,
                repo,
                &tag,
                &manifest,
                &[archive, manifest.clone()],
                "pack"
            )
            .unwrap(),
            upload::Uploaded::Staged
        );
        assert_eq!(*titles.lock().unwrap(), vec![title.to_string()]);
        let slug = ctx.staging_slug(repo);
        assert_eq!(
            w.host.asset(&slug, &tag, &manifest_name),
            Some(manifest_bytes)
        );
        assert_eq!(
            w.host.asset(&slug, &tag, &archive_name),
            Some(payload.to_vec())
        );
        assert!(
            w.host
                .asset(&slug, &tag, upload::VERSION_RESERVATION)
                .is_some()
        );
    }
}

/// Both calls use production upload::publish. The hook completes B immediately
/// after A's last check returned, before A's first payload clobber starts.
#[test]
fn a_reserved_staging_version_refuses_another_host_after_the_snapshot() {
    let w = world("staged-version-interleaving");
    let staging = w.ctx.staging_slug("ay");
    let tag = "atpkg-ay-9";
    let manifest_name = "pkg-ay-9.toml";
    let first = pack_files(&w, "host-a", "0.1.0+gaaaaaaaaa");
    let second = pack_files(&w, "host-b", "0.1.0+gbbbbbbbbb");
    let first_bytes = std::fs::read(&first.0).unwrap();
    let second_bytes = std::fs::read(&second.0).unwrap();
    assert_ne!(first_bytes, second_bytes);

    // Serial control: the same different-version request is refused once its
    // predecessor's manifest is visible to check_staged.
    upload::publish(&w.ctx, "ay", tag, &first.0, &first.1, "A").unwrap();
    let serial = upload::publish(&w.ctx, "ay", tag, &second.0, &second.1, "B");
    assert!(matches!(serial, Err(Error::Refused(_))), "{serial:?}");
    assert_eq!(
        w.host.asset(&staging, tag, manifest_name),
        Some(first_bytes.clone())
    );

    // Start with no manifest or reservation. At the clobber boundary the
    // reservation must already exclude a different version from another host.
    w.host.put(
        &staging,
        tag,
        FakeRelease {
            prerelease: true,
            ..FakeRelease::default()
        },
    );
    let observed_b = Arc::new(Mutex::new(None));
    let result_b = Arc::clone(&observed_b);
    let host = Arc::clone(&w.host);
    let stage_b = staging.clone();
    let mut ctx_b = w.ctx.clone();
    ctx_b.state_dir = w.root.join("host-b-state");
    let _lock_b = pack::pack_lock(&ctx_b, "ay", Duration::ZERO).expect("B's local pack lock");
    let wrapped = Arc::new(BeforeUpload {
        inner: Arc::clone(&w.host),
        once: Mutex::new(Some(Box::new(move || {
            let result = upload::publish(&ctx_b, "ay", tag, &second.0, &second.1, "B");
            *result_b.lock().unwrap() = Some((
                result.map_err(|error| error.to_string()),
                host.asset(&stage_b, tag, manifest_name),
            ));
        }))),
        when: Replace::Clobber,
        before_create: false,
        reservation_fault: ReservationFault::None,
        payload_order: Arc::default(),
        created_titles: Arc::default(),
    });
    let mut ctx_a = w.ctx.clone();
    ctx_a.state_dir = w.root.join("host-a-state");
    let _lock_a =
        pack::pack_lock(&ctx_a, "ay", Duration::ZERO).expect("A's independent local pack lock");
    ctx_a.hosts.staging = wrapped;
    let result_a = upload::publish(&ctx_a, "ay", tag, &first.0, &first.1, "A");

    assert_eq!(result_a.unwrap(), upload::Uploaded::Staged);
    let (result_b, before_a) = observed_b
        .lock()
        .unwrap()
        .take()
        .expect("B ran at the clobber boundary");
    assert!(
        result_b
            .unwrap_err()
            .contains("one build number names one version")
    );
    assert_eq!(before_a, None, "B never uploaded its different manifest");
    assert_eq!(
        w.host.asset(&staging, tag, manifest_name),
        Some(first_bytes),
        "A's admitted version remains the sole staged version"
    );
}

#[test]
fn simultaneous_first_reservations_choose_one_version_and_same_version_can_resume() {
    let w = world("first-version-reservation");
    let first = pack_files(&w, "a", "0.1.0+gaaaaaaaaa");
    for (other_version, same) in [("0.1.0+gbbbbbbbbb", false), ("0.1.0+gaaaaaaaaa", true)] {
        let slug = empty_release(&w);
        let second = pack_files(&w, if same { "same" } else { "different" }, other_version);
        let expected = std::fs::read(if same { &first.0 } else { &second.0 }).unwrap();
        let ctx_b = w.ctx.clone();
        let done_b = Arc::new(Mutex::new(None));
        let observed = Arc::clone(&done_b);
        let wrapper = BeforeUpload {
            when: Replace::Never,
            once: Mutex::new(Some(Box::new(move || {
                *observed.lock().unwrap() = Some(
                    upload::publish(&ctx_b, "ay", "atpkg-ay-9", &second.0, &second.1, "B")
                        .map_err(|error| error.to_string()),
                );
            }))),
            ..BeforeUpload::over(Arc::clone(&w.host))
        };
        let mut ctx_a = w.ctx.clone();
        ctx_a.hosts.staging = Arc::new(wrapper);
        let result_a = upload::publish(&ctx_a, "ay", "atpkg-ay-9", &first.0, &first.1, "A");
        assert_eq!(*done_b.lock().unwrap(), Some(Ok(upload::Uploaded::Staged)));
        if same {
            assert_eq!(result_a.unwrap(), upload::Uploaded::Staged);
            let reservation = w
                .host
                .asset(&slug, "atpkg-ay-9", upload::VERSION_RESERVATION);
            upload::publish(&w.ctx, "ay", "atpkg-ay-9", &first.0, &first.1, "resume").unwrap();
            assert_eq!(
                w.host
                    .asset(&slug, "atpkg-ay-9", upload::VERSION_RESERVATION),
                reservation
            );
        } else {
            assert!(matches!(result_a, Err(Error::Refused(_))), "{result_a:?}");
        }
        assert_eq!(
            w.host.asset(&slug, "atpkg-ay-9", "pkg-ay-9.toml"),
            Some(expected)
        );
    }
}

#[test]
fn a_lost_reservation_ack_is_reconciled_only_by_reading_the_winner() {
    let w = world("version-ack");
    let first = pack_files(&w, "first", "0.1.0+gaaaaaaaaa");
    for fault in [ReservationFault::LoseAck, ReservationFault::BeforeWrite] {
        let slug = empty_release(&w);
        let mut wrapper = BeforeUpload::over(Arc::clone(&w.host));
        wrapper.reservation_fault = fault;
        let mut ctx = w.ctx.clone();
        ctx.hosts.staging = Arc::new(wrapper);
        let result = upload::publish(&ctx, "ay", "atpkg-ay-9", &first.0, &first.1, "A");
        if matches!(fault, ReservationFault::LoseAck) {
            assert_eq!(result.unwrap(), upload::Uploaded::Staged);
        } else {
            assert!(matches!(result, Err(Error::CannotRun(_))), "{result:?}");
            assert_eq!(w.host.asset(&slug, "atpkg-ay-9", "pkg-ay-9.toml"), None);
        }
    }
}

#[test]
fn racing_release_creation_reads_the_winning_version_before_any_payload() {
    let w = world("version-create-race");
    let first = pack_files(&w, "a", "0.1.0+gaaaaaaaaa");
    let second = pack_files(&w, "b", "0.1.0+gbbbbbbbbb");
    let expected = std::fs::read(&second.0).unwrap();
    let ctx_b = w.ctx.clone();
    let observed = Arc::new(Mutex::new(None));
    let completed = Arc::clone(&observed);
    let wrapper = BeforeUpload {
        before_create: true,
        once: Mutex::new(Some(Box::new(move || {
            *completed.lock().unwrap() = Some(
                upload::publish(&ctx_b, "ay", "atpkg-ay-9", &second.0, &second.1, "B")
                    .map_err(|error| error.to_string()),
            );
        }))),
        ..BeforeUpload::over(Arc::clone(&w.host))
    };
    let order = Arc::clone(&wrapper.payload_order);
    let mut ctx = w.ctx.clone();
    ctx.hosts.staging = Arc::new(wrapper);
    assert!(matches!(
        upload::publish(&ctx, "ay", "atpkg-ay-9", &first.0, &first.1, "A"),
        Err(Error::Refused(_))
    ));
    assert_eq!(
        *observed.lock().unwrap(),
        Some(Ok(upload::Uploaded::Staged))
    );
    assert!(
        order.lock().unwrap().is_empty(),
        "losing create never reaches payload upload"
    );
    assert_eq!(
        w.host
            .asset(&ctx.staging_slug("ay"), "atpkg-ay-9", "pkg-ay-9.toml"),
        Some(expected)
    );
}

#[test]
fn unreadable_or_malformed_reservations_and_legacy_conflicts_never_clobber_payloads() {
    let w = world("version-reservation-refusals");
    let first = pack_files(&w, "first", "0.1.0+gaaaaaaaaa");
    let other = pack_files(&w, "other", "0.1.0+gbbbbbbbbb");
    upload::publish(&w.ctx, "ay", "atpkg-ay-9", &first.0, &first.1, "A").unwrap();
    let slug = w.ctx.staging_slug("ay");
    let valid = w.host.release_of(&slug, "atpkg-ay-9").unwrap();
    let record = valid.assets[upload::VERSION_RESERVATION].clone();
    let mut trailing = record.clone();
    trailing.push(0);
    let mut invalid_utf8 = record.clone();
    *invalid_utf8.last_mut().unwrap() = 0xff;
    for malformed in [
        b"unknown".to_vec(),
        record[..record.len() - 1].to_vec(),
        trailing,
        invalid_utf8,
        vec![b'x'; 65_536],
    ] {
        w.host.put(&slug, "atpkg-ay-9", valid.clone());
        w.host
            .put_asset(&slug, "atpkg-ay-9", upload::VERSION_RESERVATION, malformed);
        let before = w.host.release_of(&slug, "atpkg-ay-9");
        assert!(upload::publish(&w.ctx, "ay", "atpkg-ay-9", &first.0, &first.1, "A").is_err());
        assert_eq!(w.host.release_of(&slug, "atpkg-ay-9"), before);
    }
    w.host.put(&slug, "atpkg-ay-9", valid.clone());
    w.host
        .fail_once(&slug, "atpkg-ay-9", upload::VERSION_RESERVATION);
    assert!(matches!(
        upload::publish(&w.ctx, "ay", "atpkg-ay-9", &first.0, &first.1, "A"),
        Err(Error::CannotRun(_))
    ));
    assert_eq!(w.host.release_of(&slug, "atpkg-ay-9"), Some(valid.clone()));

    let mut legacy = valid;
    legacy.assets.remove(upload::VERSION_RESERVATION);
    legacy.assets.insert(
        "pkg-ay-9.toml".to_string(),
        std::fs::read(&other.0).unwrap(),
    );
    w.host.put(&slug, "atpkg-ay-9", legacy.clone());
    assert!(matches!(
        upload::publish(&w.ctx, "ay", "atpkg-ay-9", &first.0, &first.1, "A"),
        Err(Error::Refused(_))
    ));
    assert_eq!(w.host.release_of(&slug, "atpkg-ay-9"), Some(legacy));
}

#[test]
fn a_manifest_that_changes_during_legacy_admission_is_checked_after_reservation() {
    let w = world("version-legacy-change");
    let slug = empty_release(&w);
    let first = pack_files(&w, "first", "0.1.0+gaaaaaaaaa");
    let other = pack_files(&w, "old-writer", "0.1.0+gbbbbbbbbb");
    let other_bytes = std::fs::read(&other.0).unwrap();
    let placed = other_bytes.clone();
    let host = Arc::clone(&w.host);
    let target = slug.clone();
    let wrapper = BeforeUpload {
        when: Replace::Never,
        once: Mutex::new(Some(Box::new(move || {
            host.put_asset(&target, "atpkg-ay-9", "pkg-ay-9.toml", placed);
        }))),
        ..BeforeUpload::over(Arc::clone(&w.host))
    };
    let mut ctx = w.ctx.clone();
    ctx.hosts.staging = Arc::new(wrapper);
    assert!(matches!(
        upload::publish(&ctx, "ay", "atpkg-ay-9", &first.0, &first.1, "A"),
        Err(Error::Refused(_))
    ));
    assert_eq!(
        w.host.asset(&slug, "atpkg-ay-9", "pkg-ay-9.toml"),
        Some(other_bytes)
    );
}

#[test]
fn raw_managed_staging_is_guarded_and_other_raw_release_protocols_are_unchanged() {
    let w = world("raw-version-admission");
    let first = pack_files(&w, "first", "0.1.0+gaaaaaaaaa");
    let other = pack_files(&w, "other", "0.1.0+gbbbbbbbbb");
    let signature = first.0.with_extension("toml.sig");
    put(&signature, b"data-only signature fixture");
    let wrapped = BeforeUpload::over(Arc::clone(&w.host));
    let order = Arc::clone(&wrapped.payload_order);
    let mut ctx = w.ctx.clone();
    ctx.hosts.staging = Arc::new(wrapped);
    let args = |owner: &str, repo: &str, tag: &str, paths: &[PathBuf]| {
        [owner.to_owned(), repo.to_owned(), tag.to_owned()]
            .into_iter()
            .chain(paths.iter().map(|path| path.to_string_lossy().into_owned()))
            .collect::<Vec<_>>()
    };
    // Deliberately supply the manifest first: ready metadata still uploads last.
    upload::run(
        &ctx,
        &args(
            "staging",
            "ay",
            "atpkg-ay-9",
            &[first.0.clone(), first.1[0].clone(), signature],
        ),
    )
    .unwrap();
    assert_eq!(
        *order.lock().unwrap(),
        [vec![
            "ay-9-aarch64-apple-darwin.tar.zst".to_string(),
            "pkg-ay-9.toml.sig".to_string(),
            "pkg-ay-9.toml".to_string(),
        ]]
    );
    let slug = ctx.staging_slug("ay");
    let before = w.host.release_of(&slug, "atpkg-ay-9");
    assert!(matches!(
        upload::run(&ctx, &args("staging", "ay", "atpkg-ay-9", &other.1)),
        Err(Error::Refused(_))
    ));
    assert!(matches!(
        upload::run(&ctx, &args("public", "ay", "atpkg-ay-9", &first.1)),
        Err(Error::Refused(_))
    ));
    assert_eq!(w.host.release_of(&slug, "atpkg-ay-9"), before);
    assert_eq!(
        w.host.release_of(&ctx.public_slug("ay"), "atpkg-ay-9"),
        None
    );
    assert!(upload::run(&ctx, &args("staging", "ay", "atpkg-ay-9", &first.1[..1])).is_err());
    let mismatched = pack_files(&w, "wrong-program", "0.1.0+gaaaaaaaaa");
    let bytes = std::fs::read_to_string(&mismatched.0)
        .unwrap()
        .replace("program = \"ay\"", "program = \"other\"");
    put(&mismatched.0, bytes.as_bytes());
    assert!(upload::run(&ctx, &args("staging", "ay", "atpkg-ay-9", &mismatched.1)).is_err());
    assert_eq!(w.host.release_of(&slug, "atpkg-ay-9"), before);

    for (owner, tag) in [
        ("staging", "scratch-v1"),
        ("public", "scratch-v1"),
        ("staging", "atpkg-index-9"),
    ] {
        let slug = if owner == "staging" {
            ctx.staging_slug("other")
        } else {
            ctx.public_slug("other")
        };
        w.host.put(&slug, tag, FakeRelease::default());
        upload::run(&ctx, &args(owner, "other", tag, &first.1[..1])).unwrap();
        assert_eq!(
            w.host
                .asset(&slug, tag, "ay-9-aarch64-apple-darwin.tar.zst"),
            Some(b"first".to_vec())
        );
        assert_eq!(w.host.asset(&slug, tag, upload::VERSION_RESERVATION), None);
    }
}
