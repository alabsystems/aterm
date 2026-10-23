// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `foreign_db` lock-free and snapshot reads.

use super::*;

#[test]
fn the_immutable_uri_encodes_every_byte_but_the_plain_ones() {
    assert_eq!(
        immutable_uri(Path::new(
            "/Library/Application Support/com.apple.TCC/TCC.db"
        )),
        "file:/Library/Application%20Support/com.apple.TCC/TCC.db?immutable=1"
    );
    assert_eq!(
        immutable_uri(Path::new("/a/b?c#d%e&f=g.db")),
        "file:/a/b%3Fc%23d%25e%26f%3Dg.db?immutable=1",
        "no byte of the path can end it or add a parameter"
    );
}

/// No database file is no answer, and the reader is never asked: nothing
/// there, or a link (never followed) to a real database.
#[test]
fn no_database_file_is_no_answer() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let db = dir.path().join("x.db");
    let asked = |_: &str| -> Option<()> { panic!("the reader is never asked") };
    assert_eq!(read(&db, asked), Err(Unread::Missing));
    #[cfg(unix)]
    {
        let real = dir.path().join("real.db");
        std::fs::write(&real, b"SQLite format 3\0").expect("real");
        std::os::unix::fs::symlink(&real, &db).expect("link");
        assert_eq!(read(&db, asked), Err(Unread::Missing), "a link");
    }
}

/// A rollback journal with something in it beside the database is a write
/// under way: no answer, the reader never asked. An empty one (a TRUNCATE
/// journal at rest) is none.
#[test]
fn a_journal_beside_the_database_is_a_write_under_way() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let db = dir.path().join("x.db");
    std::fs::write(&db, b"SQLite format 3\0").expect("db");
    std::fs::write(dir.path().join("x.db-journal"), b"\xd9\xd5\x05\xf9").expect("journal");
    assert_eq!(
        read(&db, |_| -> Option<()> { panic!("never asked") }),
        Err(Unread::Writing)
    );
    std::fs::write(dir.path().join("x.db-journal"), b"").expect("emptied");
    assert_eq!(read(&db, |_| Some(7)), Ok(7), "an empty journal");
}

/// A file that changes while it is read is no answer: the database itself, a
/// `-wal` that appears, a journal that appears. NEGATIVE CONTROL: the same
/// reader with nothing changed answers.
#[test]
fn a_file_that_changes_during_the_read_is_no_answer() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let db = dir.path().join("x.db");
    std::fs::write(&db, b"SQLite format 3\0").expect("db");
    assert_eq!(read(&db, |_| Some(1)), Ok(1), "unchanged");
    let grow = |p: PathBuf| {
        move |_: &str| {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .expect("open");
            std::io::Write::write_all(&mut f, b"x").expect("write");
            Some(())
        }
    };
    assert_eq!(read(&db, grow(db.clone())), Err(Unread::Changed), "the db");
    let wal = dir.path().join("x.db-wal");
    assert_eq!(read(&db, grow(wal.clone())), Err(Unread::Changed), "a -wal");
    std::fs::remove_file(&wal).expect("no wal");
    let journal = dir.path().join("x.db-journal");
    assert_eq!(
        read(&db, grow(journal.clone())),
        Err(Unread::Writing),
        "a journal"
    );
    std::fs::remove_file(&journal).expect("no journal");
    assert_eq!(read(&db, |_| None::<()>), Err(Unread::Failed), "no answer");
}

/// With no `-wal` the reader gets the file itself as an immutable URI, and
/// with one the path of a private copy beside a copy of the `-wal` (and no
/// `-shm`), gone once it returns; nothing is written beside the live file.
#[test]
fn the_reader_gets_the_file_itself_or_a_private_copy() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let db = dir.path().join("x.db");
    std::fs::write(&db, b"main").expect("db");
    assert_eq!(read(&db, |t| Some(t.to_owned())), Ok(immutable_uri(&db)));
    std::fs::write(dir.path().join("x.db-wal"), b"frames").expect("wal");
    std::fs::write(dir.path().join("x.db-shm"), b"index").expect("shm");
    let mut copied = None;
    let seen = read(&db, |t| {
        let copy = PathBuf::from(t);
        let mut shm = copy.as_os_str().to_os_string();
        shm.push("-shm");
        copied = Some(copy.clone());
        Some((
            std::fs::read(&copy).ok()?,
            std::fs::read(beside(&copy, "-wal")).ok()?,
            Path::new(&shm).exists(),
        ))
    });
    assert_eq!(seen, Ok((b"main".to_vec(), b"frames".to_vec(), false)));
    let copied = copied.expect("asked");
    assert!(!copied.starts_with(dir.path()), "{copied:?}");
    assert!(!copied.exists(), "the private copy is gone");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = read(&db, |t| {
            let private = Path::new(t).parent()?.metadata().ok()?;
            assert_eq!(private.permissions().mode() & 0o777, 0o700);
            Some(())
        });
    }
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .expect("list")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["x.db", "x.db-shm", "x.db-wal"], "nothing beside it");
}

#[cfg(target_os = "macos")]
mod live {
    use super::*;

    /// SQLite's lock bytes on the main file: `PENDING_BYTE` (0x40000000), then
    /// `RESERVED` and the 510 `SHARED` bytes.
    const DB_LOCKS: (u64, u64) = (0x4000_0000, 512);

    /// Take `fcntl` locks on `file`'s bytes (`(start, len, exclusive)`), for as
    /// long as the returned file is open.
    fn hold(file: &Path, locks: &[(u64, u64, bool)]) -> std::fs::File {
        use std::os::fd::AsRawFd as _;
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(file)
            .expect("open to lock");
        for &(start, len, exclusive) in locks {
            // SAFETY: a zeroed `flock` is a valid value of the plain C struct.
            let mut fl: libc::flock = unsafe { std::mem::zeroed() };
            fl.l_type = if exclusive {
                libc::F_WRLCK
            } else {
                libc::F_RDLCK
            } as _;
            fl.l_whence = libc::SEEK_SET as _;
            fl.l_start = i64::try_from(start).expect("start");
            fl.l_len = i64::try_from(len).expect("len");
            // SAFETY: `fl` is valid for the call and the descriptor is open.
            let rc = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETLK, &raw const fl) };
            assert_eq!(rc, 0, "the lock is taken");
        }
        f
    }

    /// Apple's `sqlite3` on `target` with the readers' options (`-nofollow`
    /// among them: no link anywhere in the path); its stdout, or its stderr as
    /// the error.
    fn sqlite(target: &str, sql: &str) -> Result<String, String> {
        let out = crate::spawnfd::command("/usr/bin/sqlite3")
            .args([
                "-init",
                "/dev/null",
                "-readonly",
                "-safe",
                "-nofollow",
                "-batch",
                "-bail",
                target,
                sql,
            ])
            .output()
            .expect("sqlite3");
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    }

    fn make(db: &Path, sql: &str) {
        let out = crate::spawnfd::command("/usr/bin/sqlite3")
            .arg(db)
            .arg(sql)
            .output()
            .expect("sqlite3");
        assert!(out.status.success(), "{out:?}");
    }

    /// A ROLLBACK-JOURNAL database is read in place with no lock: with the lock
    /// a committing writer holds (`EXCLUSIVE`, every lock byte), the read still
    /// answers its rows. NEGATIVE CONTROL: SQLite's own read-only open of the
    /// same file meets that lock (`database is locked`): it would have held
    /// `SHARED`, the lock that fails such a writer's `COMMIT`.
    #[test]
    fn a_rollback_database_is_read_with_no_lock() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let db = std::fs::canonicalize(dir.path())
            .expect("canonical")
            .join("t.db");
        make(&db, "CREATE TABLE t(v); INSERT INTO t VALUES ('kept');");
        let _writer = hold(&db, &[(DB_LOCKS.0, DB_LOCKS.1, true)]);
        let sql = "SELECT v FROM t;";
        let control = sqlite(&db.to_string_lossy(), sql);
        assert!(
            control
                .as_ref()
                .is_err_and(|e| e.contains("database is locked")),
            "{control:?}"
        );
        assert_eq!(read(&db, |t| sqlite(t, sql).ok()), Ok("kept\n".to_owned()));
    }

    /// A WAL database is read from a private copy of it and its `-wal`, which
    /// holds rows no checkpoint moved into the file yet (its holder killed, as
    /// a crash leaves it): with the live `-shm` held as a reader rebuilding its
    /// index holds it (its first page zeroed, its write, checkpoint and recover
    /// locks taken, the dead-man switch shared), the read answers those rows.
    /// NEGATIVE CONTROL: SQLite's own read-only open of the live file meets
    /// those locks at once, as a writer with no busy timeout does.
    #[test]
    fn a_wal_database_is_read_from_a_private_copy() {
        use std::io::{BufRead as _, Write as _};
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let db = std::fs::canonicalize(dir.path())
            .expect("canonical")
            .join("w.db");
        let mut holder = crate::spawnfd::command("/usr/bin/sqlite3")
            .arg(&db)
            .stdin(crate::spawnfd::Stdio::piped())
            .stdout(crate::spawnfd::Stdio::piped())
            .spawn()
            .expect("holder");
        let mut stdin = holder.stdin.take().expect("stdin");
        writeln!(
            stdin,
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE t(v); \
             INSERT INTO t VALUES ('in the wal'); SELECT 'holder wrote';"
        )
        .expect("write");
        stdin.flush().expect("flush");
        let lines = std::io::BufReader::new(holder.stdout.take().expect("stdout")).lines();
        let said: Vec<String> = lines
            .map_while(Result::ok)
            .take_while(|l| l != "holder wrote")
            .collect();
        assert_eq!(said, ["wal", "0"], "the holder wrote its row");
        holder.kill().expect("killed, as a crash ends it");
        let _ = holder.wait();
        let shm = beside(&db, "-shm");
        assert!(beside(&db, "-wal").exists() && shm.exists(), "left behind");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&shm)
            .and_then(|f| f.set_len(0).and_then(|()| f.set_len(32 * 1024)))
            .expect("the index emptied");
        let _index = hold(&shm, &[(120, 3, true), (128, 1, false)]);
        let sql = "SELECT v FROM t;";
        let control = sqlite(&db.to_string_lossy(), sql);
        assert!(
            control
                .as_ref()
                .is_err_and(|e| e.contains("database is locked")),
            "{control:?}"
        );
        assert_eq!(
            read(&db, |t| sqlite(t, sql).ok()),
            Ok("in the wal\n".to_owned())
        );
    }

    /// The witness catches a lock held across a read, freezes its holder while
    /// `write` runs, and lets it run on. NEGATIVE CONTROL: with no lock held,
    /// nothing is caught and `write` never runs.
    #[test]
    fn the_witness_freezes_a_holder_while_the_writer_runs() {
        use std::io::{BufRead as _, Write as _};
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let db = std::fs::canonicalize(dir.path())
            .expect("canonical")
            .join("h.db");
        make(&db, "CREATE TABLE t(v);");
        let mut holder = crate::spawnfd::command("/usr/bin/sqlite3")
            .arg(&db)
            .stdin(crate::spawnfd::Stdio::piped())
            .stdout(crate::spawnfd::Stdio::piped())
            .spawn()
            .expect("holder");
        let mut stdin = holder.stdin.take().expect("stdin");
        writeln!(stdin, "BEGIN IMMEDIATE; SELECT 'held';").expect("write");
        stdin.flush().expect("flush");
        let mut lines = std::io::BufReader::new(holder.stdout.take().expect("stdout")).lines();
        assert_eq!(lines.next().and_then(Result::ok).as_deref(), Some("held"));
        let pid = i32::try_from(holder.id()).expect("pid");
        let (done, wait) = std::sync::mpsc::channel::<()>();
        let state = |p: i32| {
            let out = crate::spawnfd::command("/bin/ps")
                .args(["-o", "state=", "-p", &p.to_string()])
                .output()
                .expect("ps");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        let (read, caught) = witness::while_read(
            &db,
            DB_LOCKS.0,
            DB_LOCKS.1,
            move || {
                wait.recv_timeout(std::time::Duration::from_secs(60))
                    .is_ok()
            },
            || {
                let seen = state(pid);
                let _ = done.send(());
                seen
            },
        );
        assert!(read, "the writer ran");
        let (who, seen) = caught.expect("the holder is caught");
        assert_eq!(who, pid);
        assert!(
            seen.starts_with('T'),
            "frozen while the writer ran: {seen:?}"
        );
        assert!(!state(pid).starts_with('T'), "and runs on");
        writeln!(stdin, "COMMIT;").expect("commit");
        drop(stdin);
        let _ = holder.wait();
        let (read, caught) = witness::while_read(&db, DB_LOCKS.0, DB_LOCKS.1, || 1, || 2);
        assert_eq!((read, caught), (1, None), "no lock, nothing caught");
    }
}
