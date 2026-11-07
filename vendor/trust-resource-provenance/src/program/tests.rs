use std::fs;
use std::io::{BufRead as _, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use super::*;

static SERIAL: Mutex<()> = Mutex::new(());
const ACTOR: &str = "program::tests::native_program_actor";

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

fn command(kind: u32, text: &str, fixed: usize) -> Vec<u8> {
    let length = (fixed + text.len() + 1).next_multiple_of(8);
    let mut out = vec![0u8; length];
    out[..4].copy_from_slice(&kind.to_le_bytes());
    out[4..8].copy_from_slice(&(length as u32).to_le_bytes());
    out[8..12].copy_from_slice(&(fixed as u32).to_le_bytes());
    out[fixed..fixed + text.len()].copy_from_slice(text.as_bytes());
    out
}
fn image(executable: bool, loads: &[&str], extras: Vec<Vec<u8>>) -> Vec<u8> {
    let mut cmds = extras;
    if executable {
        cmds.push(command(0xe, "/usr/lib/dyld", 12));
    }
    cmds.extend(loads.iter().map(|load| command(0xc, load, 24)));
    let mut out = vec![0u8; 32];
    for (offset, value) in [
        (0, 0xfeed_facfu32),
        (4, 0x0100_000c),
        (12, if executable { 2 } else { 6 }),
        (16, cmds.len() as u32),
        (20, cmds.iter().map(Vec::len).sum::<usize>() as u32),
    ] {
        out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    out.extend(cmds.iter().flatten());
    out
}

struct Fixture {
    directory: std::mem::ManuallyDrop<tempfile::TempDir>,
    root: PathBuf,
    program: PathBuf,
    execution_attempted: bool,
    execution_proved: bool,
}
impl Fixture {
    fn new(loads: &[&str]) -> Self {
        // Own build output is a checked private ancestor, unlike world-writable
        // /tmp. No account state or unrelated checkout is created or changed.
        let base = std::env::current_exe().unwrap().parent().unwrap().canonicalize().unwrap();
        let directory = tempfile::Builder::new()
            .prefix("native-program-")
            .suffix(".noindex")
            .tempdir_in(base)
            .unwrap();
        let root = directory.path().join("sdk");
        fs::create_dir(&root).unwrap();
        for name in ["bin", "lib"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        let program = root.join("bin/program");
        fs::write(&program, image(true, loads, Vec::new())).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o555)).unwrap();
        Self {
            directory: std::mem::ManuallyDrop::new(directory),
            root,
            program,
            execution_attempted: false,
            execution_proved: false,
        }
    }
    fn digest(&self) -> [u8; 32] {
        Sha256::digest(&fs::read(&self.program).unwrap()).into()
    }
    fn open(&self) -> io::Result<HeldProgram> {
        HeldProgram::open(&self.root, &self.program, &self.digest(), deadline())
    }
    fn runtime(&self, name: &str, loads: &[&str]) {
        let path = self.root.join("lib").join(name);
        fs::write(
            &path,
            image(false, loads, vec![command(0x8000_001c, "@loader_path/../lib", 12)]),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.execution_attempted && !self.execution_proved {
            eprintln!("preserved native image without execution proof: {}", self.root.display());
        } else {
            unsafe {
                std::mem::ManuallyDrop::drop(&mut self.directory);
            }
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_program_retains_exact_transitive_runtime_and_detects_later_changes() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = Fixture::new(&["@rpath/a.dylib", "/usr/lib/libSystem.B.dylib"]);
    fixture.runtime("a.dylib", &["@rpath/b.dylib", "/usr/lib/libSystem.B.dylib"]);
    fixture.runtime("b.dylib", &["/usr/lib/libSystem.B.dylib"]);
    let pin = fixture.open().unwrap();
    assert_eq!(pin.observation().runtime.len(), 2);
    assert_eq!(pin.observation().system_dependencies, ["/usr/lib/libSystem.B.dylib"]);
    assert_eq!(
        pin.runtime_environment().unwrap(),
        [("DYLD_LIBRARY_PATH".into(), fixture.root.join("lib").into_os_string())]
    );
    pin.validate(deadline()).unwrap();
    let path = fixture.root.join("lib/b.dylib");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&path, image(false, &["/usr/lib/libSystem.B.dylib"], Vec::new())).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
    assert!(
        pin.validate(deadline()).is_err(),
        "restored permissions cannot replace original metadata"
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn unrelated_outer_entries_do_not_change_selected_inputs_but_retargeting_does() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = Fixture::new(&["/usr/lib/libSystem.B.dylib"]);
    let held = fixture.open().unwrap();
    fs::write(fixture.directory.path().join("unrelated"), b"activity outside the SDK").unwrap();
    held.validate(deadline()).unwrap();
    let outer = fixture.directory.path().to_owned();
    let backup = outer.with_extension("retargeted.noindex");
    fs::rename(&outer, &backup).unwrap();
    fs::create_dir(&outer).unwrap();
    assert!(
        held.validate(deadline()).is_err(),
        "a different ancestor inode cannot replace the original"
    );
    drop(held);
    fs::remove_dir_all(backup).unwrap();
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_program_refuses_foreign_loader_selection_aliases_and_unbounded_images() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for loads in [
        &["@rpath/missing.dylib"][..],
        &["/foreign/code.dylib"][..],
        &["@loader_path/code.dylib"][..],
        &["/usr/lib/../foreign"][..],
    ] {
        assert!(Fixture::new(loads).open().is_err());
    }
    let fixture = Fixture::new(&["/usr/lib/libSystem.B.dylib"]);
    fixture.runtime("libSystem.B.dylib", &["/usr/lib/libSystem.B.dylib"]);
    assert!(fixture.open().is_err(), "a selected sysroot cannot shadow OS runtime");
    for alteration in 0..8 {
        let fixture = Fixture::new(&["/usr/lib/libSystem.B.dylib"]);
        let mut digest = fixture.digest();
        match alteration {
            0 => {
                fs::set_permissions(&fixture.program, fs::Permissions::from_mode(0o644)).unwrap();
                fs::write(&fixture.program, b"#!/bin/sh\nexit 0\n").unwrap();
                fs::set_permissions(&fixture.program, fs::Permissions::from_mode(0o555)).unwrap();
            }
            1 => fs::set_permissions(&fixture.program, fs::Permissions::from_mode(0o577)).unwrap(),
            2 => {
                let real = fixture.root.join("real");
                fs::rename(&fixture.program, &real).unwrap();
                std::os::unix::fs::symlink(real, &fixture.program).unwrap();
            }
            3 => {
                fs::rename(fixture.root.join("bin"), fixture.root.join("real-bin")).unwrap();
                std::os::unix::fs::symlink("real-bin", fixture.root.join("bin")).unwrap();
            }
            4 => fs::set_permissions(fixture.root.join("bin"), fs::Permissions::from_mode(0o777))
                .unwrap(),
            5 => {
                fs::set_permissions(&fixture.program, fs::Permissions::from_mode(0o755)).unwrap();
                fs::OpenOptions::new()
                    .write(true)
                    .open(&fixture.program)
                    .unwrap()
                    .set_len(MAX_IMAGE + 1)
                    .unwrap();
            }
            6 => {
                fs::set_permissions(&fixture.program, fs::Permissions::from_mode(0o755)).unwrap();
                fs::write(
                    &fixture.program,
                    image(true, &[], vec![command(0x27, "DYLD_INSERT_LIBRARIES=/foreign", 12)]),
                )
                .unwrap();
            }
            7 => {
                fs::set_permissions(&fixture.program, fs::Permissions::from_mode(0o755)).unwrap();
                fs::write(
                    &fixture.program,
                    image(true, &[], vec![command(0x8000_001c, "/foreign", 12)]),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        if matches!(alteration, 0 | 6 | 7) {
            digest = fixture.digest();
        }
        assert!(
            HeldProgram::open(&fixture.root, &fixture.program, &digest, deadline()).is_err(),
            "accepted altered native input {alteration}"
        );
    }
    let fixture = Fixture::new(&["/usr/lib/libSystem.B.dylib"]);
    assert!(HeldProgram::open(&fixture.root, &fixture.program, &[0; 32], deadline()).is_err());
    assert!(
        HeldProgram::open(&fixture.root, &fixture.program, &fixture.digest(), Instant::now())
            .is_err()
    );
}

struct OwnedChild(Child, Option<std::thread::JoinHandle<()>>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        if let Some(reader) = self.1.take() {
            let _ = reader.join();
        }
    }
}
fn actor(fixture: &mut Fixture) -> OwnedChild {
    fixture.execution_attempted = true;
    let mut child = OwnedChild(
        Command::new(&fixture.program)
            .args(["--exact", ACTOR, "--nocapture"])
            .env_clear()
            .env("NATIVE_PROGRAM_ACTOR", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
        None,
    );
    let output = child.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut read = std::io::BufReader::new(output);
        let mut line = String::new();
        let mut total = 0;
        let mut send = Some(send);
        loop {
            line.clear();
            if read.read_line(&mut line).unwrap() == 0 {
                break;
            }
            total += line.len();
            assert!(total <= 4096);
            if line.contains("held-native-program-running") {
                if let Some(send) = send.take() {
                    send.send(()).unwrap();
                }
            }
        }
    });
    receive.recv_timeout(Duration::from_secs(60)).unwrap();
    child.1 = Some(reader);
    fixture.execution_proved = true;
    child
}
fn finish(mut child: OwnedChild) {
    child.0.stdin.take().unwrap().write_all(b"x").unwrap();
    let end = deadline();
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn native_program_actor() {
    if std::env::var_os("NATIVE_PROGRAM_ACTOR").is_none() {
        return;
    }
    println!("held-native-program-running");
    std::io::stdout().flush().unwrap();
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn actual_native_child_matches_original_held_image_not_equal_bytes_at_another_path() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut fixture = Fixture::new(&[]);
    fs::set_permissions(&fixture.program, fs::Permissions::from_mode(0o755)).unwrap();
    fs::copy(std::env::current_exe().unwrap(), &fixture.program).unwrap();
    fs::set_permissions(&fixture.program, fs::Permissions::from_mode(0o555)).unwrap();
    // Establish actual execution before pinning first-run OS image metadata.
    // Both copies remain at their original paths until a child proved execution.
    finish(actor(&mut fixture));
    let other = fixture.root.join("bin/other");
    fs::copy(&fixture.program, &other).unwrap();
    let digest = fixture.digest();
    let pin = fixture.open().unwrap();
    let other_pin = HeldProgram::open(&fixture.root, &other, &digest, deadline()).unwrap();
    let child = actor(&mut fixture);
    let identity = process::read_identity(child.0.id()).unwrap();
    pin.validate_loaded(identity, deadline()).unwrap();
    assert!(other_pin.validate_loaded(identity, deadline()).is_err());
    let mut reused = identity;
    reused.birth += 1;
    assert!(pin.validate_loaded(reused, deadline()).is_err());
    finish(child);
    assert!(pin.validate_loaded(identity, deadline()).is_err());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "explicit real SDK input profile; no executable is launched"]
fn real_sdk_held_program_profile() {
    let root =
        PathBuf::from(std::env::var_os("TRUST_PROGRAM_TEST_SDK_ROOT").expect("explicit SDK root"));
    for (name, digest_variable) in [
        ("trustc", "TRUST_PROGRAM_TEST_TRUSTC_SHA256"),
        ("targo", "TRUST_PROGRAM_TEST_TARGO_SHA256"),
    ] {
        let digest = std::env::var(digest_variable).unwrap();
        let digest: [u8; 32] = crate::schema::unhex(&digest).unwrap().try_into().unwrap();
        let held =
            HeldProgram::open(&root, &root.join("bin").join(name), &digest, deadline()).unwrap();
        held.validate(deadline()).unwrap();
        println!(
            "actual-sdk-input tool={name} image={} runtime_files={} runtime_bytes={} policy={}",
            held.observation().executable.sha256,
            held.observation().runtime.len(),
            held.observation().runtime.iter().map(|file| file.bytes).sum::<u64>(),
            held.observation().policy
        );
    }
}
