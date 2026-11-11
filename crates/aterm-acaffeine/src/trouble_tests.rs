// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scratch(dir: &Path) -> Paths {
    Paths {
        state_dir: dir.join("state"),
        home: dir.join("home"),
        logs_dir: dir.join("logs"),
        local_volume: |_| true,
    }
}

/// A trouble keeps the time it was first seen while it is noted again, and a cleared one
/// starts anew; it lasts once [`LASTING_SECS`] have passed since then.
#[test]
fn a_trouble_keeps_when_it_began_until_it_is_cleared() {
    let dir = crate::test_scratch_dir("trouble");
    let paths = scratch(dir.path());
    assert_eq!(read(&paths), None);
    note_at(&paths, "the disk is full", 1_000).expect("noted");
    note_at(&paths, "the disk is still full", 1_500).expect("noted again");
    let trouble = read(&paths).expect("recorded");
    assert_eq!(
        trouble,
        Trouble {
            since: 1_000,
            why: "the disk is still full".to_string()
        }
    );
    assert!(!trouble.lasting(1_000 + LASTING_SECS - 1));
    assert!(trouble.lasting(1_000 + LASTING_SECS));
    assert!(!trouble.lasting(0), "a clock set back reads as not yet");
    clear(&paths);
    assert_eq!(read(&paths), None);
    clear(&paths);
    note_at(&paths, "again", 9_000).expect("noted");
    assert_eq!(read(&paths).map(|t| t.since), Some(9_000));
}

/// The file is two lines, the reason one printable line however it was written.
#[test]
fn the_reason_is_one_printable_line() {
    let text = encode(&Trouble {
        since: 7,
        why: "launchctl said:\nno\u{1b}[2J".to_string(),
    });
    assert_eq!(text, "since=7\nwhy=launchctl said: no [2J\n");
    assert_eq!(
        decode(&text),
        Some(Trouble {
            since: 7,
            why: "launchctl said: no [2J".to_string()
        })
    );
    for bad in [
        "",
        "since=7\n",
        "why=x\n",
        "since=x\nwhy=y\n",
        "since=+7\nwhy=y\n",
    ] {
        assert_eq!(decode(bad), None, "{bad:?}");
    }
    let dirty = format!(
        "since=7\nwhy=\u{1b}[2J\u{9b}31m{}\n",
        "x".repeat(WHY_CAP + 32)
    );
    let cleaned = decode(&dirty).expect("scrubbed");
    assert!(!cleaned.why.chars().any(char::is_control));
    assert!(cleaned.why.chars().count() <= WHY_CAP);
}

/// TROUBLE READ REFUSES SYMLINKS, FIFOS, AND OVERSIZE FILES: `read` opens with
/// `O_NOFOLLOW | O_NONBLOCK`, rejects files past `READ_CAP`, and never blocks on a FIFO.
/// NEGATIVE CONTROL: a regular trouble file under `READ_CAP` decodes cleanly.
#[cfg(unix)]
#[test]
fn trouble_read_refuses_symlinks_fifos_and_oversize_files() {
    let dir = crate::test_scratch_dir("trouble");
    let paths = scratch(dir.path());
    std::fs::create_dir_all(&paths.state_dir).expect("state dir");

    note_at(&paths, "steady", 42).expect("regular trouble");
    assert_eq!(
        read(&paths),
        Some(Trouble {
            since: 42,
            why: "steady".into(),
        })
    );

    let real = dir.path().join("real.trouble");
    std::fs::write(&real, "since=99\nwhy=aliased\n").expect("real");
    let trouble_path = paths.trouble_file();
    std::fs::remove_file(&trouble_path).expect("remove regular");
    std::os::unix::fs::symlink(&real, &trouble_path).expect("symlink");
    assert_eq!(read(&paths), None, "symlinked trouble file must be refused");

    std::fs::remove_file(&trouble_path).expect("remove symlink");
    let c_fifo = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(
        trouble_path.as_os_str(),
    ))
    .expect("cstring");
    // SAFETY: `c_fifo` is a NUL-terminated path inside our exclusive scratch directory.
    let rc = unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo");
    assert_eq!(read(&paths), None, "FIFO trouble path must not block");

    std::fs::remove_file(&trouble_path).expect("remove fifo");
    let huge = format!("since=42\nwhy=ok\n# {}\n", "pad".repeat(READ_CAP as usize));
    std::fs::write(&trouble_path, huge).expect("oversize");
    assert_eq!(read(&paths), None, "oversize trouble file must be refused");
}
