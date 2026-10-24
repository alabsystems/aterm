// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The agent's inert tests (the design's §13): the serve loop and the watch connection
//! over unix sockets with the Fake `System`, and the seam bin end to end. Every command
//! they run is the agent's own debug executable in a stand-in mode; every wait for what
//! must happen is a one-minute hang detector.

#[cfg(not(target_os = "linux"))]
mod seam;
mod serve;
mod watch;

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_vz_agent::fake::Fake;
use aterm_vz_agent::system::System;
use aterm_vz_agent::wire::{self, Exec, Exit, Frame, Stdin};

/// The hang detector: a wait for what must happen fails after a minute, never sooner.
pub const HANG: Duration = Duration::from_secs(60);

/// The agent's debug executable, which the stand-ins are.
pub const AGENT: &str = env!("CARGO_BIN_EXE_aterm-vz-agent");

/// The executable the stand-ins run: `ATERM_VZ_AGENT_TEST_IMAGE` (an absolute path to a
/// retained copy, for contained runs) when set, else Cargo's bin.
pub fn agent_exe() -> PathBuf {
    match std::env::var_os("ATERM_VZ_AGENT_TEST_IMAGE") {
        Some(path) => {
            let path = PathBuf::from(path);
            assert!(
                path.is_absolute(),
                "ATERM_VZ_AGENT_TEST_IMAGE must be absolute"
            );
            path
        }
        None => PathBuf::from(AGENT),
    }
}

/// This test process's pid, which a parked stand-in (`hang`, `group`) outlives by at
/// most a quarter second.
pub fn test_pid() -> String {
    std::process::id().to_string()
}

/// `probe` until it answers, failing as hung after [`HANG`].
pub fn until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = probe() {
            return v;
        }
        assert!(start.elapsed() < HANG, "hung: {what} after {HANG:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A serve loop on a unix socket in a scratch directory, answering with `fake`.
pub struct Agent {
    pub dir: aterm_tempfile::TempDir,
    sock: PathBuf,
    pub fake: Arc<Fake>,
}

impl Agent {
    pub fn new(fake: Fake) -> Self {
        let dir = aterm_tempfile::tempdir().unwrap();
        let sock = dir.path().join("c.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let fake = Arc::new(fake);
        let system: Arc<dyn System> = fake.clone();
        std::thread::spawn(move || {
            let _ = aterm_vz_agent::serve::serve(&listener, &system);
        });
        Self { dir, sock, fake }
    }

    pub fn connect(&self) -> Conn {
        Conn::new(UnixStream::connect(&self.sock).unwrap())
    }

    /// A command connection carrying `request`.
    pub fn request(&self, request: &Frame) -> Conn {
        let mut c = self.connect();
        c.send(request);
        c
    }

    /// `argv` run in `/` with only `PATH`, its clock the Fake's.
    pub fn exec(&self, argv: &[&[u8]]) -> Exec {
        Exec {
            argv: argv.iter().map(|a| a.to_vec()).collect(),
            cwd: b"/".to_vec(),
            env: vec![(b"PATH".to_vec(), b"/usr/bin:/bin".to_vec())],
            stdin: Stdin::Null,
            merge: false,
            clock_unix_ns: self.fake.now_unix_ns(),
        }
    }

    /// The stand-in `mode args…`.
    pub fn stand_in(&self, mode: &[&str]) -> Exec {
        let exe = agent_exe();
        let mut argv: Vec<&[u8]> = vec![exe.as_os_str().as_encoded_bytes(), b"stand-in"];
        argv.extend(mode.iter().map(|a| a.as_bytes()));
        self.exec(&argv)
    }
}

/// How a request ended, with everything before it.
#[derive(Debug)]
pub struct Ended {
    pub out: Vec<u8>,
    pub err: Vec<u8>,
    pub exit: Exit,
}

impl Ended {
    pub fn out(&self) -> String {
        String::from_utf8_lossy(&self.out).into_owned()
    }

    pub fn err(&self) -> String {
        String::from_utf8_lossy(&self.err).into_owned()
    }
}

/// The host's end of one connection.
pub struct Conn(pub UnixStream);

impl Conn {
    pub fn new(s: UnixStream) -> Self {
        s.set_read_timeout(Some(HANG)).unwrap();
        Self(s)
    }

    pub fn send(&mut self, frame: &Frame) {
        wire::write_frame(&mut self.0, frame).unwrap();
    }

    /// The next frame, `None` at EOF; a minute of silence fails as hung.
    pub fn frame(&mut self) -> Option<Frame> {
        match wire::read_frame(&mut self.0) {
            Ok(f) => f,
            Err(wire::WireError::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                panic!("hung: no frame for {HANG:?}")
            }
            Err(e) => panic!("{e}"),
        }
    }

    /// Output frames until the `Exit`, which must be the connection's last frame.
    pub fn ended(&mut self) -> Ended {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        loop {
            match self.frame() {
                Some(Frame::Out(b)) => out.extend(b),
                Some(Frame::Err(b)) => err.extend(b),
                Some(Frame::Exit(exit)) => {
                    assert_eq!(
                        self.frame(),
                        None,
                        "the Exit is the connection's last frame"
                    );
                    return Ended { out, err, exit };
                }
                other => panic!("not a command's frame: {other:?}"),
            }
        }
    }

    /// Output frames until `out` holds `line`.
    pub fn out_until(&mut self, line: &str) -> String {
        let mut out = Vec::new();
        loop {
            let text = String::from_utf8_lossy(&out).into_owned();
            if text.contains(line) {
                return text;
            }
            match self.frame() {
                Some(Frame::Out(b)) => out.extend(b),
                other => panic!("waiting for `{line}` in {text:?}: {other:?}"),
            }
        }
    }
}
