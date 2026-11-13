// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_machines::init_github::repository_input_watch_paths;
use aterm_machines::model::User;
use aterm_machines::port::{Fake, Stat};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn repairing_the_executed_git_shim_wakes_the_same_admitted_build() {
    // Every look here falls within one minute of the first ([`CHECKOUT_LOOK`]).
    let now = Instant::now();
    use std::os::unix::fs::PermissionsExt as _;

    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let user = User {
        name: "fixture".into(),
        uid: aterm_uds::peer::our_uid(),
        gid: 20,
        home: home.to_string_lossy().into_owned(),
        shell: "/bin/zsh".into(),
        admin: false,
    };
    let mut sys = Fake::root();
    sys.euid = user.uid;
    sys.logged_in(&user);
    let prefix = home.join("Library/Application Support/aterm/pkg");
    let store = prefix.join("store/git");
    let build = store.join("17");
    let payload = build.join("bin/git");
    let shim = prefix.join("bin/git");
    std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
    std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
    for (path, bytes) in [
        (
            &payload,
            b"#!/bin/sh\nprintf 'fixture git ready\\n'\n".as_slice(),
        ),
        (&shim, b"#!/bin/sh\nexit 1\n".as_slice()),
    ] {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        sys.file(path.to_str().unwrap(), bytes)
            .stat_of(path.to_str().unwrap(), Live::new().stat(path).unwrap());
    }
    sys.file(store.join("17.ready").to_str().unwrap(), b"ready");
    sys.link(store.join("current").to_str().unwrap(), "17");
    sys.runtime_ready(&build);
    let private = [
        (
            home.join(".secrets/gh_access_token"),
            b"inert_fixture_token".as_slice(),
        ),
        (
            home.join(".secrets/github-repositories.json"),
            br#"{"schema":2,"account":"fixture","repositories":[]}"#.as_slice(),
        ),
    ];
    for (path, bytes) in &private {
        sys.file(path.to_str().unwrap(), bytes).stat_of(
            path.to_str().unwrap(),
            Stat {
                uid: user.uid,
                gid: user.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
    }
    let private_versions = private
        .each_ref()
        .map(|(path, _)| sys.file_version(path).unwrap());
    let paths = Paths::locate_for_home(&home);
    let mut inputs = Inputs::new(&sys, &paths, true, now);
    let installed = aterm_machines::items::github::installed_git_for(&sys, Some(&home)).unwrap();
    assert_eq!(installed, shim, "the actual command uses the managed shim");
    assert!(
        !aterm_uds::spawnfd::command(&installed)
            .output()
            .unwrap()
            .status
            .success()
    );
    let identity = aterm_machines::items::github::installed_git_identity(&sys);
    let payload_version = sys.file_version(&payload).unwrap();
    let before = repository_input_mark(&sys).unwrap();
    let watched = repository_input_watch_paths(&sys).unwrap();
    assert!(watched.contains(&shim) && watched.contains(&payload));

    // The package repair producer restores the execution route without reinstalling
    // the already admitted payload, touching private inputs, or changing its selection.
    atpkg::platform::install_shim_to(&shim, &payload).unwrap();
    let repaired = std::fs::read(&shim).unwrap();
    sys.file(shim.to_str().unwrap(), &repaired)
        .stat_of(shim.to_str().unwrap(), Live::new().stat(&shim).unwrap());
    assert!(
        aterm_uds::spawnfd::command(&installed)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        aterm_machines::items::github::installed_git_identity(&sys),
        identity
    );
    assert_eq!(sys.file_version(&payload).unwrap(), payload_version);
    assert_eq!(
        private
            .each_ref()
            .map(|(path, _)| sys.file_version(path).unwrap()),
        private_versions
    );
    assert!(
        before != repository_input_mark(&sys).unwrap(),
        "parked setup sees the same repair"
    );
    assert!(
        inputs.changed(&sys, true, now).unwrap() == Some(RepositoryWork::Refresh),
        "the repaired execution route is new readiness"
    );
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_none(),
        "one wake for the same repair"
    );
}

#[test]
fn repaired_materialization_parent_wakes_without_changing_the_credential_inode() {
    // Every look here falls within one minute of the first ([`CHECKOUT_LOOK`]).
    let now = Instant::now();
    use aterm_secrets::client::{self, JoinOptions, JoinPassword, Paths};
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let source = scratch.path().join("vault");
    let inventory = br#"{"schema":2,"account":"fixture","repositories":[]}"#;
    std::fs::write(
        &source,
        format!(
            "gh_access_token=inert_fixture_token\ngithub-repositories.json={}\n",
            std::str::from_utf8(inventory).unwrap()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
    let paths = Paths::for_home(&home);
    client::join(
        &paths,
        &JoinOptions {
            source: source.to_string_lossy().into_owned(),
            host: Some("fixture".into()),
            interactive: false,
            wait: std::time::Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .unwrap();
    assert!(client::materialization_ready(&paths).unwrap());
    let parent = paths.dir.clone();
    let token = home.join(".secrets/gh_access_token");
    let before = std::fs::metadata(&token).unwrap();
    let held = scratch.path().join("held-secrets");
    std::fs::rename(&parent, &held).unwrap();
    std::os::unix::fs::symlink(&held, &parent).unwrap();
    assert!(client::materialization_ready(&paths).is_err());

    // The synthetic input port shares the actual admitted paths and leaf identity. Only
    // its parent admission fact changes, matching the real vault's repair below.
    let user = User {
        name: "fixture".into(),
        uid: before.uid(),
        gid: before.gid(),
        home: home.to_string_lossy().into_owned(),
        shell: "/bin/zsh".into(),
        admin: false,
    };
    let mut sys = Fake::root();
    sys.euid = user.uid;
    sys.logged_in(&user);
    for (path, bytes) in [
        (token.clone(), b"inert_fixture_token".as_slice()),
        (
            home.join(".secrets/github-repositories.json"),
            inventory.as_slice(),
        ),
    ] {
        sys.file(path.to_str().unwrap(), bytes).stat_of(
            path.to_str().unwrap(),
            Stat {
                uid: user.uid,
                gid: user.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
    }
    sys.stat_of(parent.to_str().unwrap(), Live::new().stat(&parent).unwrap());
    let metadata = Metadata::read(&sys, &paths, None, &mut RepositoryInputPathCache::default())
        .unwrap()
        .0;
    let mut inputs = Inputs::new(&sys, &paths, true, now);
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_none(),
        "blocked but unchanged"
    );

    std::fs::remove_file(&parent).unwrap();
    std::fs::rename(&held, &parent).unwrap();
    assert!(client::materialization_ready(&paths).unwrap());
    assert_eq!(std::fs::metadata(&token).unwrap().ino(), before.ino());
    sys.stat_of(parent.to_str().unwrap(), Live::new().stat(&parent).unwrap());
    assert_eq!(
        metadata.files,
        Metadata::read(&sys, &paths, None, &mut RepositoryInputPathCache::default())
            .unwrap()
            .0
            .files
    );
    assert_eq!(
        inputs.changed(&sys, true, now).unwrap(),
        Some(RepositoryWork::Refresh),
        "path admission repaired"
    );
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_none(),
        "one wake for the repair"
    );
    let mut policy = Live::new().stat(&parent).unwrap();
    policy.len += 1;
    sys.stat_of(parent.to_str().unwrap(), policy);
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_none(),
        "unrelated directory contents do not move admission"
    );
}

#[test]
fn local_readiness_changes_wake_once_without_programs_or_unchanged_private_reads() {
    // The checkout scope is read at the first look and once a minute ([`CHECKOUT_LOOK`]).
    let now = Instant::now();
    let minute = |n: u32| now + CHECKOUT_LOOK * n;
    let user = User {
        name: "fixture".into(),
        uid: 501,
        gid: 20,
        home: "/input-observer-home".into(),
        shell: "/bin/zsh".into(),
        admin: false,
    };
    let paths = Paths::locate_for_home(std::path::Path::new(&user.home));
    let mut sys = Fake::root();
    sys.euid = user.uid;
    let mut cold = Readiness::new(
        &sys,
        &paths,
        || panic!("an unread account cannot inspect sharing or holds"),
        || panic!("an unread account cannot establish a receipt basis"),
        now,
    );
    assert!(
        cold.request(
            &sys,
            || panic!("an unread account cannot inspect inputs"),
            Default::default(),
            now,
        )
        .is_none()
    );
    sys.logged_in(&user);
    /// Make `path` the user's own private file holding `bytes`.
    fn seed(sys: &Fake, user: &User, path: &str, bytes: &[u8]) {
        sys.file(path, bytes).stat_of(
            path,
            Stat {
                uid: user.uid,
                gid: user.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
    }
    let file = |path: &str, bytes: &[u8]| seed(&sys, &user, path, bytes);
    let token = format!("{}/.secrets/gh_access_token", user.home);
    let inventory = format!("{}/.secrets/github-repositories.json", user.home);
    let helper = format!("{}/.local/bin/aterm", user.home);
    let profile = format!("{}/.config/aterm/init.toml", user.home);
    file(&profile, b"");
    file(&token, b"inert_token_one");
    file(
        &inventory,
        br#"{"schema":2,"account":"fixture","repositories":[{"id":1,"name":"code","owner":{"login":"fixture"},"full_name":"fixture/code","clone_url":"https://github.com/fixture/code.git","private":true,"default_branch":"main"}]}"#,
    );
    let directory = Stat {
        uid: user.uid,
        gid: user.gid,
        mode: 0o700,
        dir: true,
        symlink: false,
        len: 0,
    };
    sys.stat_of(&user.home, directory);
    for parent in std::path::Path::new(&profile).ancestors().skip(1) {
        sys.stat_of(parent.to_str().unwrap(), directory);
    }
    let checkout = format!("{}/code", user.home);
    sys.stat_of(&checkout, directory);
    file(&helper, b"helper one");
    sys.executable(aterm_machines::items::github::CLT_GIT, b"git one");
    let private_reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&private_reads);
    sys.on_owned_read(move |_, path| {
        if path.file_name().is_some_and(|name| {
            name == "gh_access_token"
                || name == "github-repositories.json"
                || name == "init.toml"
                || name == "installed-command.json"
        }) {
            counted.fetch_add(1, Ordering::SeqCst);
        }
    });
    let mut inputs = Inputs::new(&sys, &paths, true, now);
    let context = aterm_machines::items::Ctx {
        facts: aterm_machines::model::HostFacts::default(),
        users: vec![user.clone()],
        console: None,
        host_name: String::new(),
        cache_dir: PathBuf::new(),
        disrupt: false,
    };
    let captured =
        aterm_machines::init_github::InitialRepositoryInputs::capture(&sys, &context).unwrap();
    let refresh_scope = captured.refresh_scope(&sys).unwrap();
    assert_eq!(
        inputs
            .seen
            .as_ref()
            .and_then(RepositoryInputMark::refresh_scope),
        Some(refresh_scope),
        "the actual observer and Native capture share the production scope builder"
    );
    private_reads.store(0, Ordering::SeqCst);
    for _ in 0..3 {
        assert!(inputs.changed(&sys, true, now).unwrap().is_none());
    }
    assert_eq!(
        private_reads.load(Ordering::SeqCst),
        0,
        "metadata gates private reads"
    );
    assert_eq!(
        inputs.next_look(now),
        Some(minute(1)),
        "settled inputs owe their next look at the checkout minute"
    );
    sys.stat_of(&format!("{checkout}/.git"), directory);
    assert!(
        inputs
            .changed(&sys, true, minute(1) - Duration::from_millis(1))
            .unwrap()
            .is_none(),
        "a look within the minute leaves the checkout scope unread"
    );
    assert!(
        inputs.changed(&sys, true, minute(1)).unwrap() == Some(RepositoryWork::Repair),
        "an externally completed checkout is useful input"
    );
    private_reads.store(0, Ordering::SeqCst);
    assert!(inputs.changed(&sys, true, minute(2)).unwrap().is_none());
    assert_eq!(
        private_reads.load(Ordering::SeqCst),
        0,
        "cached checkout scope gates private reads too"
    );
    file(&format!("{checkout}/.git/config"), b"repaired origin");
    assert!(
        inputs
            .changed(&sys, true, minute(3) - Duration::from_millis(1))
            .unwrap()
            .is_none(),
        "a read that found the scope unchanged starts the minute too"
    );
    assert!(
        inputs.changed(&sys, true, minute(3)).unwrap() == Some(RepositoryWork::Repair),
        "in-place Git identity repair is useful input"
    );
    file(&format!("{checkout}/working.txt"), b"unrelated work");
    assert!(
        inputs.changed(&sys, true, minute(4)).unwrap().is_none(),
        "ordinary worktree content is not observed"
    );
    assert!(inputs.changed(&sys, true, minute(5)).unwrap().is_none());
    assert_eq!(
        inputs
            .seen
            .as_ref()
            .and_then(RepositoryInputMark::refresh_scope),
        Some(refresh_scope),
        "checkout-only repair cannot request its own remote refresh"
    );
    // A declaration read within the settle of its change owes a look once it has settled: an
    // edit in the same tick leaves the stamp a watch compares unmoved.
    let clock = sys.now;
    sys.now = 1;
    assert!(inputs.changed(&sys, true, minute(5)).unwrap().is_none());
    assert_eq!(
        inputs.next_look(minute(5)),
        Some(minute(5) + settled_look()),
        "an unsettled declaration owes a look once settled"
    );
    // A declaration refused (another owner's) is no settling read, however recent: what
    // replaces it is heard, and a look each settle would read it for nothing.
    let owned = sys.stat(std::path::Path::new(&profile)).unwrap();
    sys.stat_of(&profile, Stat { uid: 0, ..owned });
    assert_eq!(
        Inputs::new(&sys, &paths, true, minute(5)).unsettled,
        None,
        "a refused declaration owes no settle look"
    );
    sys.stat_of(&profile, owned);
    sys.now = clock;
    let file = |path: &str, bytes: &[u8]| seed(&sys, &user, path, bytes);
    let settled = minute(5) + settled_look();
    assert!(inputs.changed(&sys, true, settled).unwrap().is_none());
    assert_eq!(
        inputs.next_look(settled),
        Some(minute(6)),
        "a settled read owes only the checkout minute again"
    );
    use aterm_secrets::converge::{RefreshObservation, RefreshVersion, RepositoryRequest};
    let before = RefreshObservation {
        scope: Some(refresh_scope),
        version: RefreshVersion {
            input: 7,
            attempt: 9,
            fulfilled: 9,
        },
        recheck_ms: Some(aterm_secrets::converge::RECHECK_MS),
        ..RefreshObservation::default()
    };
    // The retained factory was constructed before this account and its private inputs
    // were readable. Refused admissions must not turn that temporary gap into a no-op.
    private_reads.store(0, Ordering::SeqCst);
    for unavailable in [
        User {
            uid: user.uid + 1,
            ..user.clone()
        },
        User {
            home: "/another-input-observer-home".into(),
            ..user.clone()
        },
    ] {
        sys.logged_in(&unavailable);
        assert!(
            cold.request(
                &sys,
                || panic!("another account cannot inspect sharing or inputs"),
                before,
                now,
            )
            .is_none()
        );
    }
    assert_eq!(private_reads.load(Ordering::SeqCst), 0);
    sys.logged_in(&user);
    let Some(RepositoryRequest::Refresh(recovered)) = cold.request(&sys, || true, before, now)
    else {
        panic!("the retained factory recovers its initially unread own account");
    };
    assert_eq!(recovered.scope, Some(refresh_scope));
    assert_eq!(recovered.base_input, before.version.input);
    private_reads.store(0, Ordering::SeqCst);
    assert!(cold.request(&sys, || true, before, now).is_none());
    assert_eq!(private_reads.load(Ordering::SeqCst), 0);

    let root = Fake::root();
    root.logged_in(&User {
        uid: 0,
        ..user.clone()
    });
    let mut refused = Readiness::new(
        &root,
        &paths,
        || panic!("root cannot inspect this login's inputs"),
        || panic!("root cannot establish this login's receipt basis"),
        now,
    );
    assert!(
        refused
            .request(&root, || panic!("root remains refused"), before, now,)
            .is_none()
    );
    private_reads.store(0, Ordering::SeqCst);
    assert!(refused.request(&sys, || false, before, now).is_none());
    assert_eq!(private_reads.load(Ordering::SeqCst), 0);
    assert!(
        refused.request(&sys, || true, before, now).is_some(),
        "a held recovery reads current inputs only when allowed again"
    );
    // Receipt first, then local source capture, exactly as the real factory.
    let mut delayed = Inputs::new(&sys, &paths, true, now);
    let mut cursor = RepositoryRefreshCursor::new(before, delayed.seen.as_ref());
    assert!(
        delayed
            .request(&sys, true, before, &mut cursor, now)
            .is_some()
    );
    file(&token, b"inert_token_delayed");
    let next = repository_input_mark(&sys)
        .unwrap()
        .refresh_scope()
        .unwrap();
    let winner = RefreshObservation {
        scope: Some(next),
        version: RefreshVersion {
            input: 8,
            attempt: 10,
            fulfilled: 10,
        },
        ..before
    };
    let mut unavailable = user.clone();
    unavailable.uid += 1;
    sys.logged_in(&unavailable);
    assert!(
        delayed
            .request(&sys, true, winner, &mut cursor, now)
            .is_none(),
        "an unavailable own-account sample cannot relabel retained source facts"
    );
    sys.logged_in(&user);
    let Some(RepositoryRequest::Refresh(request)) =
        delayed.request(&sys, true, winner, &mut cursor, now)
    else {
        panic!("the actual callback observes the delivered source");
    };
    assert_eq!(
        request.base_input, 7,
        "a delayed actor retains its previous source basis"
    );
    assert!(
        winner.fulfills(request),
        "the winner already refreshed this materialization"
    );
    // An observed intermediate source cannot let an old same-byte success
    // acknowledge the later return before any owner admitted that transition.
    file(&token, b"inert_token_one");
    let Some(RepositoryRequest::Refresh(away)) =
        delayed.request(&sys, true, winner, &mut cursor, now)
    else {
        panic!("the intermediate source is observed");
    };
    file(&token, b"inert_token_delayed");
    let Some(RepositoryRequest::Refresh(back)) =
        delayed.request(&sys, true, winner, &mut cursor, now)
    else {
        panic!("the return is observed");
    };
    assert_eq!(back.scope, winner.scope);
    assert!(
        !winner.fulfills(away.merge(back)),
        "unadmitted ABA still owes a new generation"
    );
    // Semantic source A returns before this actor samples at all. The init
    // profile's inode is excluded from scope, so the semantic selection is A.
    let declared = b"[develop]\ncheckouts=['~/elsewhere']\n";
    file(&profile, declared);
    let other_scope = repository_input_mark(&sys)
        .unwrap()
        .refresh_scope()
        .unwrap();
    let other = RefreshObservation {
        scope: Some(other_scope),
        version: RefreshVersion {
            input: 9,
            attempt: 11,
            fulfilled: 11,
        },
        ..winner
    };
    file(&profile, b"");
    let Some(RepositoryRequest::Refresh(intervening)) =
        delayed.request(&sys, true, other, &mut cursor, now)
    else {
        panic!("a current source after a foreign admitted generation owes immediate refresh");
    };
    assert_eq!(intervening.scope, winner.scope);
    assert_eq!(intervening.base_input, 9);
    file(&profile, declared);
    let Some(RepositoryRequest::Refresh(returned)) =
        delayed.request(&sys, true, other, &mut cursor, now)
    else {
        panic!("the later semantic selection is observed");
    };
    assert_eq!(
        returned.base_input, 9,
        "the confirmed intervening source retains its temporal floor"
    );
    assert!(
        !other.fulfills(intervening.merge(returned)),
        "an earlier same-selection success cannot cover the return"
    );
    file(&profile, b"");
    assert_eq!(
        inputs.changed(&sys, true, now).unwrap(),
        Some(RepositoryWork::Refresh)
    );
    for declared in [
        b"[develop]\ncheckouts=['~/code']\n".as_slice(),
        b"".as_slice(),
    ] {
        file(&profile, declared);
        assert_eq!(
            inputs.changed(&sys, true, now).unwrap(),
            Some(RepositoryWork::Refresh)
        );
        private_reads.store(0, Ordering::SeqCst);
        assert!(inputs.changed(&sys, true, now).unwrap().is_none());
        assert_eq!(
            private_reads.load(Ordering::SeqCst),
            0,
            "settled refresh scope stays metadata-only"
        );
    }
    file(&profile, b"[develop]\ncheckouts=[]\n");
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_none(),
        "an empty declaration changes no refresh scope"
    );
    for (path, bytes) in [
        (token.as_str(), b"inert_token_two".as_slice()),
        (helper.as_str(), b"helper two".as_slice()),
        (
            aterm_machines::items::github::CLT_GIT,
            b"git two".as_slice(),
        ),
    ] {
        file(path, bytes);
        assert_eq!(
            inputs.changed(&sys, true, now).unwrap(),
            Some(RepositoryWork::Refresh),
            "changed declared input"
        );
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_none(),
            "one wake for unchanged readiness"
        );
    }
    let declaration = aterm_types::installed_command::path(std::path::Path::new(&user.home));
    let custom = format!("{}/custom store/aterm", user.home);
    let payload = format!("{}/custom payload/aterm", user.home);
    sys.link(&custom, &payload);
    let select = |path: &str| {
        let bytes = aterm_types::installed_command::Selection::new(path.into())
            .unwrap()
            .encode()
            .unwrap();
        file(declaration.to_str().unwrap(), &bytes);
    };
    select(&custom);
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_some(),
        "declared missing helper replaces legacy readiness"
    );
    assert!(inputs.changed(&sys, true, now).unwrap().is_none());
    private_reads.store(0, Ordering::SeqCst);
    assert!(inputs.changed(&sys, true, now).unwrap().is_none());
    assert_eq!(
        private_reads.load(Ordering::SeqCst),
        0,
        "settled declaration bytes stay unread"
    );
    file(&payload, b"arrived custom command");
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_some(),
        "a dangling selected target repair wakes"
    );
    assert!(inputs.changed(&sys, true, now).unwrap().is_none());
    file(&helper, b"old legacy command changed");
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_none(),
        "exclusive selection retires legacy paths"
    );
    let replacement_helper = format!("{}/replacement store/aterm", user.home);
    select(&replacement_helper);
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_some(),
        "declaration replacement is observed"
    );
    assert!(inputs.changed(&sys, true, now).unwrap().is_none());
    file(&payload, b"old custom command changed");
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_none(),
        "old declared targets retire"
    );
    file(&replacement_helper, b"repaired replacement command");
    assert!(inputs.changed(&sys, true, now).unwrap().is_some());
    assert!(inputs.changed(&sys, true, now).unwrap().is_none());
    file(
        &format!("{}/.config/aterm/init.toml", user.home),
        b"[develop]\ncheckouts=[]\n",
    );
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_none(),
        "unrelated settings do not recreate a source change"
    );
    let selected = format!("{}/selected-token", user.home);
    file(&selected, b"selected_token_one");
    let scoped = format!("[github]\naccount='fixture'\ntoken_file='{selected}'\n");
    file(&profile, scoped.as_bytes());
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_some(),
        "a profile retarget is current input"
    );
    assert!(inputs.changed(&sys, true, now).unwrap().is_none());
    private_reads.store(0, Ordering::SeqCst);
    file(&token, b"old_unselected_token");
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_none(),
        "old token paths retire"
    );
    assert_eq!(private_reads.load(Ordering::SeqCst), 0);
    file(&selected, b"selected_token_two");
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_some(),
        "new token path remains observed"
    );
    let parent = std::path::Path::new(&profile).parent().unwrap();
    sys.stat_of(
        parent.to_str().unwrap(),
        Stat {
            mode: 0o770,
            ..directory
        },
    );
    assert!(inputs.changed(&sys, true, now).unwrap().is_some());
    assert!(inputs.changed(&sys, true, now).unwrap().is_none());
    sys.stat_of(parent.to_str().unwrap(), directory);
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_some(),
        "unchanged-leaf parent admission repairs wake"
    );
    assert!(inputs.changed(&sys, true, now).unwrap().is_none());
    let replacement = format!("{}/replacement-token", user.home);
    file(&replacement, b"replacement_token_one");
    let replacement_profile =
        format!("[github]\naccount='fixture'\ntoken_file='{replacement}'\n").into_bytes();
    let change_once = Arc::new(std::sync::atomic::AtomicBool::new(true));
    sys.on_owned_read({
        let profile = profile.clone();
        let change_once = Arc::clone(&change_once);
        move |sys, path| {
            if path == std::path::Path::new(&profile) && change_once.swap(false, Ordering::SeqCst) {
                sys.file(&profile, &replacement_profile).stat_of(
                    &profile,
                    Stat {
                        uid: 501,
                        gid: 20,
                        mode: 0o600,
                        dir: false,
                        symlink: false,
                        len: replacement_profile.len() as u64,
                    },
                );
            }
        }
    });
    file(
        &profile,
        format!("{scoped}# new selection read\n").as_bytes(),
    );
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_some(),
        "a raced profile read is recaptured, not relabeled"
    );
    assert!(inputs.changed(&sys, true, now).unwrap().is_none());
    file(&replacement, b"replacement_token_two");
    assert!(
        inputs.changed(&sys, true, now).unwrap().is_some(),
        "the raced-in selection remains observable"
    );
    assert!(inputs.changed(&sys, true, now).unwrap().is_none());
    private_reads.store(0, Ordering::SeqCst);
    file(&replacement, b"inert_token_new");
    assert!(
        inputs.changed(&sys, false, now).unwrap().is_none(),
        "Stop/off holds the observer"
    );
    assert_eq!(private_reads.load(Ordering::SeqCst), 0);
    assert!(
        inputs.changed(&sys, true, now).unwrap() == Some(RepositoryWork::Refresh),
        "current authority resumes with fresh inputs"
    );
    assert!(
        sys.calls().iter().all(|call| call == "identity"),
        "{:?}",
        sys.calls()
    );

    // Canonical identity is an independent subject. A late name after a provisional pass
    // owes local convergence even when repository observation is held or already fulfilled.
    use aterm_agent::machine_name::file::MachineName;
    use aterm_secrets::client::IdentityDebt;
    use aterm_types::machine_name::{Class, Nickname, Number};
    let mut name_sys = Fake::root();
    name_sys.euid = user.uid;
    name_sys.logged_in(&user);
    let state = PathBuf::from(&user.home).join("state");
    let name_path = state.join(aterm_agent::machine_name::file::FILE);
    let mut naming = NamingInputs::new(&paths, Some(state));
    assert_eq!(naming.request(&name_sys, || true), IdentityDebt::None);
    let write_name = |bytes: &[u8]| {
        name_sys.file(name_path.to_str().unwrap(), bytes).stat_of(
            name_path.to_str().unwrap(),
            Stat {
                uid: user.uid,
                gid: user.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
    };
    write_name(b"malformed canonical input");
    assert_eq!(
        naming.request(&name_sys, || true),
        IdentityDebt::None,
        "an unread/invalid canonical name is not a provisional target"
    );
    let mut saved = MachineName::unnumbered(Nickname::parse("dog").unwrap(), 1);
    saved.num = Some(Number::new(85).unwrap());
    saved.class = Class::Chosen;
    write_name(saved.to_toml().unwrap().as_bytes());
    assert_eq!(naming.request(&name_sys, || true), IdentityDebt::Changed);
    assert_eq!(naming.request(&name_sys, || true), IdentityDebt::None);
    saved.rev += 1;
    write_name(saved.to_toml().unwrap().as_bytes());
    assert_eq!(
        naming.request(&name_sys, || true),
        IdentityDebt::None,
        "metadata/revision changes with the same identity owe no work"
    );
    saved.nick = Nickname::parse("otter").unwrap();
    write_name(saved.to_toml().unwrap().as_bytes());
    assert_eq!(naming.request(&name_sys, || false), IdentityDebt::None);
    assert_eq!(
        naming.request(&name_sys, || true),
        IdentityDebt::Changed,
        "a sharing hold does not consume the newer name"
    );
    saved.num = Some(Number::new(86).unwrap());
    write_name(saved.to_toml().unwrap().as_bytes());
    assert_eq!(naming.request(&name_sys, || true), IdentityDebt::Changed);
    assert_eq!(naming.request(&name_sys, || true), IdentityDebt::None);
    let mut unadmitted = NamingInputs::new(&paths, naming.state.clone());
    let unknown = Fake::root();
    assert_eq!(
        unadmitted.request(&unknown, || panic!(
            "unknown account reads no choices or names"
        )),
        IdentityDebt::None
    );
    assert_eq!(
        unadmitted.request(&name_sys, || true),
        IdentityDebt::Changed,
        "temporary identity failure leaves the actual observer retryable"
    );

    // Exercise the actual factory assembly with repository admission withheld. A repository
    // receipt/gate is not naming authority and cannot consume its independent input subject.
    let sharing = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let shares = Arc::clone(&sharing);
    let name_sys = Arc::new(name_sys);
    let mut runtime = observing(
        name_sys.clone(),
        &paths,
        naming.state.clone(),
        Arc::new(|| false),
        Arc::new(move || shares.load(Ordering::SeqCst)),
        None,
    );
    let snapshot = runtime.snapshot();
    assert!(snapshot.repositories.is_none());
    assert_eq!(snapshot.identity, IdentityDebt::Changed);
    assert_eq!(runtime.snapshot().identity, IdentityDebt::None);
    sharing.store(false, Ordering::SeqCst);
    saved.nick = Nickname::parse("heron").unwrap();
    let bytes = saved.to_toml().unwrap();
    name_sys
        .file(name_path.to_str().unwrap(), bytes.as_bytes())
        .stat_of(
            name_path.to_str().unwrap(),
            Stat {
                uid: user.uid,
                gid: user.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
    assert_eq!(runtime.snapshot().identity, IdentityDebt::None);
    sharing.store(true, Ordering::SeqCst);
    let snapshot = runtime.snapshot();
    assert!(snapshot.repositories.is_none());
    assert_eq!(snapshot.identity, IdentityDebt::Changed);
}

/// When the inputs owe a look with nothing rung: the checkout minute after the last read of it,
/// sooner a settle after a look that failed or read a declaration that had not settled, at once
/// before any read, and never while held, since what lifts a hold is watched. Where nothing
/// watches, every turn of the lane owes a look.
#[test]
fn inputs_owe_their_next_look_by_what_no_watch_hears() {
    let now = Instant::now();
    let mut inputs = Inputs {
        paths: Paths::locate_for_home(std::path::Path::new("/inputs-next-look")),
        metadata: None,
        seen: None,
        held: false,
        control: RepositoryInputPathCache::default(),
        checkouts_read: None,
        unsettled: None,
    };
    assert_eq!(
        inputs.next_look(now),
        Some(now),
        "no read yet: a look at once"
    );
    inputs.checkouts_read = Some(now);
    assert_eq!(inputs.next_look(now), Some(now + CHECKOUT_LOOK));
    inputs.unsettled = Some(now);
    assert_eq!(inputs.next_look(now), Some(now + settled_look()));
    inputs.checkouts_read = None;
    assert_eq!(
        inputs.next_look(now),
        Some(now + settled_look()),
        "a first read that failed retries at the settle, never at once"
    );
    inputs.held = true;
    assert_eq!(inputs.next_look(now), None);
    inputs.held = false;
    inputs.unsettled = None;
    inputs.checkouts_read = Some(now);
    let paths = inputs.paths.clone();
    let cursor =
        RepositoryRefreshCursor::new(aterm_secrets::converge::RefreshObservation::default(), None);
    let mut readiness = Readiness {
        paths,
        ready: Some((inputs, cursor)),
    };
    assert_eq!(
        readiness.next_look(now),
        Some(now + CHECKOUT_LOOK),
        "admitted inputs owe their own"
    );
    let mut observed = Observed {
        readiness: Readiness {
            paths: readiness.paths.clone(),
            ready: readiness.ready.take(),
        },
        watched: None,
        fixed: BTreeSet::new(),
    };
    assert_eq!(
        readiness.next_look(now),
        Some(now + settled_look()),
        "an account lookup that has not admitted the inputs retries as a failed look does"
    );
    assert_eq!(
        observed.next_look(now),
        Some(now),
        "no watch: a look every turn"
    );
    observed.readiness.ready = None;
    assert_eq!(observed.readiness.watched().count(), 0);
}

/// The watch follows what the last look read: an input newly read is watched, its first look
/// telling once; one still read keeps its key; one no longer read is let go ([`HANG`] detects a
/// tell or a release that never comes).
/// NEGATIVE CONTROLS: an `arm` that never unwatches leaves the dropped file armed; one that
/// watches only what it has no key for yet, without keeping the key, tells the kept input again.
#[test]
fn the_watch_follows_what_the_last_look_read() {
    const HANG: Duration = Duration::from_secs(60);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(scratch.path()).unwrap();
    let file = |name: &str| {
        let dir = base.join(name);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("input");
        std::fs::write(&path, name).unwrap();
        path
    };
    let (a, c) = (file("a"), file("c"));
    let b = base.join("b");
    std::fs::create_dir(&b).unwrap();
    let events = aterm_uds::file_watch::InertEvents::default();
    let armed = Arc::clone(&events.armed);
    let (told, telling) = std::sync::mpsc::channel();
    let watch = FileWatch::spawn(
        "converge-inputs-test",
        Box::new(events),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    let mut watched = Watched::over(watch);
    watched.arm([Watch::File(a.clone()), Watch::Folder(b.clone())].into());
    let first: BTreeSet<u64> = (0..2)
        .map(|_| telling.recv_timeout(HANG).unwrap())
        .collect();
    assert_eq!(
        first,
        BTreeSet::from([0, 1]),
        "each newly watched input tells once"
    );
    watched.arm([Watch::Folder(b.clone()), Watch::File(c.clone())].into());
    assert_eq!(
        watched.keys,
        BTreeMap::from([(Watch::File(c), 2), (Watch::Folder(b), 1)]),
        "a kept input keeps its key; a new one takes the next"
    );
    assert_eq!(
        telling.recv_timeout(HANG).unwrap(),
        2,
        "the new input tells"
    );
    let began = Instant::now();
    while armed
        .lock()
        .unwrap()
        .values()
        .any(|path| path.starts_with(a.parent().unwrap()))
    {
        assert!(
            began.elapsed() < HANG,
            "the input no look reads stayed armed"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
