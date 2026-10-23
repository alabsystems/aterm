// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The terminal a program draws on, as one trait with a real and an in-memory side (docs/DESIGN-amacs-2026-10-19.md §5.1).

use std::io;
use std::time::Instant;

use crate::signals::Signals;

/// A terminal's size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

/// What a wait on the terminal ended with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// This many bytes were read into the buffer.
    Bytes(usize),
    /// The deadline passed with nothing to read.
    Timeout,
    /// Signals are pending; [`Tty::take_signals`] takes them.
    Signal,
    /// The terminal hung up: nothing more will be read.
    Closed,
}

/// A private mode a terminal program sets and must put back before it leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The alternate screen (DEC 1049): the program's own page, the shell's left as it was.
    AltScreen,
    /// Kitty's keyboard protocol with these flags pushed onto the stack of the screen shown (amacs pushes 1|4, D-20), and carried to the other screen when the alternate screen is entered or left.
    KittyKeys(u8),
    /// Mouse reports for presses, releases and drags (1002), SGR-encoded (1006).
    Mouse,
    /// Bracketed paste (2004).
    Paste,
    /// Focus reports (1004).
    Focus,
    /// DEC 2048's in-band size reports.
    InBandResize,
    /// The cursor and the drawing state: the screen writer hides the cursor during a frame and sets its shape.
    Cursor,
}

/// A set of [`Mode`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Modes(u8);

impl Modes {
    pub const NONE: Self = Self(0);
    pub const ALT_SCREEN: Self = Self(1);
    pub const KITTY_KEYS: Self = Self(1 << 1);
    pub const MOUSE: Self = Self(1 << 2);
    pub const PASTE: Self = Self(1 << 3);
    pub const FOCUS: Self = Self(1 << 4);
    pub const IN_BAND_RESIZE: Self = Self(1 << 5);
    pub const CURSOR: Self = Self(1 << 6);

    #[inline]
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// The set these bits name; bits no mode uses are dropped.
    #[inline]
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0x7f)
    }

    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[inline]
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[inline]
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl Mode {
    /// The set holding only this mode (any kitty flags are the same mode).
    #[inline]
    #[must_use]
    pub const fn bit(self) -> Modes {
        match self {
            Self::AltScreen => Modes::ALT_SCREEN,
            Self::KittyKeys(_) => Modes::KITTY_KEYS,
            Self::Mouse => Modes::MOUSE,
            Self::Paste => Modes::PASTE,
            Self::Focus => Modes::FOCUS,
            Self::InBandResize => Modes::IN_BAND_RESIZE,
            Self::Cursor => Modes::CURSOR,
        }
    }

    /// The bytes that set it.
    #[must_use]
    pub fn set_bytes(self) -> Vec<u8> {
        match self {
            Self::AltScreen => b"\x1b[?1049h".to_vec(),
            Self::KittyKeys(flags) => format!("\x1b[>{flags}u").into_bytes(),
            Self::Mouse => b"\x1b[?1002h\x1b[?1006h".to_vec(),
            Self::Paste => b"\x1b[?2004h".to_vec(),
            Self::Focus => b"\x1b[?1004h".to_vec(),
            Self::InBandResize => b"\x1b[?2048h".to_vec(),
            Self::Cursor => Vec::new(),
        }
    }

    /// The bytes that set it again on a terminal that may have lost it (after the process was stopped and continued) without stacking a second kitty entry: kitty's flags are set in place (`CSI = flags ; 1 u`), not pushed.
    #[must_use]
    pub fn reassert_bytes(self) -> Vec<u8> {
        match self {
            Self::KittyKeys(flags) => format!("\x1b[={flags};1u").into_bytes(),
            other => other.set_bytes(),
        }
    }

    /// The bytes that put it back.
    #[must_use]
    pub fn reset_bytes(self) -> &'static [u8] {
        RESET_ORDER
            .iter()
            .find(|(bit, _)| *bit == self.bit())
            .map_or(&[], |(_, bytes)| *bytes)
    }
}

/// Each mode's reset bytes, in the order a restore writes them: the drawing state first, the kitty stack popped while the alternate screen it belongs to is still up, the alternate screen last.
pub(crate) const RESET_ORDER: [(Modes, &[u8]); 7] = [
    (Modes::CURSOR, b"\x1b[?2026l\x1b[m\x1b[0 q\x1b[?25h"),
    (Modes::IN_BAND_RESIZE, b"\x1b[?2048l"),
    (Modes::FOCUS, b"\x1b[?1004l"),
    (Modes::PASTE, b"\x1b[?2004l"),
    (Modes::MOUSE, b"\x1b[?1006l\x1b[?1002l"),
    (Modes::KITTY_KEYS, b"\x1b[<u"),
    (Modes::ALT_SCREEN, b"\x1b[?1049l"),
];

/// The order modes are set in again after a suspension: the reverse of [`RESET_ORDER`].
pub(crate) fn set_order(modes: Modes, kitty: u8) -> impl Iterator<Item = Mode> {
    [
        Mode::AltScreen,
        Mode::KittyKeys(kitty),
        Mode::Mouse,
        Mode::Paste,
        Mode::Focus,
        Mode::InBandResize,
        Mode::Cursor,
    ]
    .into_iter()
    .filter(move |mode| modes.contains(mode.bit()))
}

/// The bytes that set `mode` on a terminal where the modes in `set` (which lacks it) are set, a kitty entry among them carrying `kitty`'s flags: entering the alternate screen carries that entry onto it (see the module docs).
pub(crate) fn enable_bytes(mode: Mode, set: Modes, kitty: u8) -> Vec<u8> {
    carry_kitty(mode, set, kitty, mode.set_bytes())
}

/// The bytes that put `mode` back where the modes in `set` (which hold it) are set: leaving the alternate screen carries a kitty entry back to the main screen.
pub(crate) fn disable_bytes(mode: Mode, set: Modes, kitty: u8) -> Vec<u8> {
    carry_kitty(mode, set, kitty, mode.reset_bytes().to_vec())
}

/// `switch`, the bytes that enter or leave the alternate screen, between popping the
/// kitty entry from the screen left and pushing it onto the one entered, when one is
/// set; any other mode's bytes as they are.
fn carry_kitty(mode: Mode, set: Modes, kitty: u8, switch: Vec<u8>) -> Vec<u8> {
    if mode != Mode::AltScreen || !set.contains(Modes::KITTY_KEYS) {
        return switch;
    }
    let entry = Mode::KittyKeys(kitty);
    let mut out = entry.reset_bytes().to_vec();
    out.extend(switch);
    out.extend(entry.set_bytes());
    out
}

/// The bytes that put back every mode in `modes`, in the one order every restore uses (see the module docs).
#[must_use]
pub fn reset_bytes(modes: Modes) -> Vec<u8> {
    RESET_ORDER
        .iter()
        .filter(|(bit, _)| modes.contains(*bit))
        .flat_map(|(_, bytes)| bytes.iter().copied())
        .collect()
}

/// A terminal a program reads keys from and draws on.
pub trait Tty {
    /// Wait until input can be read, a signal arrives or `deadline` passes (`None`: wait for input or a signal), and read what can be read into `buf`.
    /// # Errors
    /// The read's or the wait's OS error.
    fn read(&mut self, buf: &mut [u8], deadline: Option<Instant>) -> io::Result<Readiness>;

    /// Write all of `bytes`.
    /// # Errors
    /// The write's OS error.
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// The terminal's size now.
    /// # Errors
    /// The size query's OS error.
    fn size(&self) -> io::Result<Size>;

    /// The clock [`Self::read`]'s deadlines and the decoder's timeouts are measured on.
    fn now(&self) -> Instant;

    /// The signals that arrived since the last call.
    /// # Errors
    /// Setting raw mode again failed.
    fn take_signals(&mut self) -> io::Result<Signals>;

    /// Set `mode`, and put it back however the program leaves.
    /// # Errors
    /// The write's OS error.
    fn enable(&mut self, mode: Mode) -> io::Result<()>;

    /// Put `mode` back now.
    /// # Errors
    /// The write's OS error.
    fn disable(&mut self, mode: Mode) -> io::Result<()>;

    /// Stop the process as SIGTSTP's default does (C-z), the terminal put back first.
    /// # Errors
    /// Putting the terminal back or setting it raw again failed.
    fn suspend(&mut self) -> io::Result<()>;
}

#[cfg(unix)]
pub use dev::DevTty;

#[cfg(unix)]
mod dev {
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd as _, OwnedFd, RawFd};
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::time::Instant;

    use super::{Mode, Readiness, Size, Tty};
    use crate::raw::RawGuard;
    use crate::signals::{self, SignalWake, Signals};

    /// The controlling terminal, raw, with the signal wake installed.
    pub struct DevTty {
        guard: RawGuard,
        signals: SignalWake,
        file: File,
    }

    impl DevTty {
        /// Open `/dev/tty` (`O_RDWR | O_CLOEXEC`), not stdio, and set it raw.
        /// # Errors
        /// There is no controlling terminal, or it cannot be set raw, or a terminal is already raw in this process.
        pub fn open() -> io::Result<Self> {
            let file = File::options()
                .read(true)
                .write(true)
                .custom_flags(libc::O_CLOEXEC)
                .open("/dev/tty")?;
            Self::from_file(file)
        }

        /// A terminal already open, set raw: a pty's slave in a test.
        /// # Errors
        /// As [`Self::open`].
        pub fn from_fd(fd: OwnedFd) -> io::Result<Self> {
            Self::from_file(File::from(fd))
        }

        fn from_file(file: File) -> io::Result<Self> {
            let signals = SignalWake::install()?;
            let guard = RawGuard::enter(file.as_raw_fd())?;
            Ok(Self {
                guard,
                signals,
                file,
            })
        }

        fn fd(&self) -> RawFd {
            self.file.as_raw_fd()
        }
    }

    impl Tty for DevTty {
        fn read(&mut self, buf: &mut [u8], deadline: Option<Instant>) -> io::Result<Readiness> {
            loop {
                // Pending signals are reported until taken, whoever drained the wake.
                if !self.signals.pending().is_empty() {
                    return Ok(Readiness::Signal);
                }
                let timeout = match deadline {
                    None => -1,
                    Some(deadline) => {
                        let now = Instant::now();
                        if now >= deadline {
                            return Ok(Readiness::Timeout);
                        }
                        let left = deadline.duration_since(now);
                        let ms = left.as_micros().div_ceil(1000);
                        libc::c_int::try_from(ms).unwrap_or(libc::c_int::MAX)
                    }
                };
                let mut fds = [
                    libc::pollfd {
                        fd: self.fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: self.signals.fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                // SAFETY: `fds` is a live array of two initialized pollfds.
                let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if fds[1].revents != 0 {
                    self.signals.drain();
                    if !self.signals.pending().is_empty() {
                        return Ok(Readiness::Signal);
                    }
                }
                if fds[0].revents != 0 {
                    // SAFETY: `buf` is a live, writable buffer of `buf.len()` bytes.
                    let n = unsafe { libc::read(self.fd(), buf.as_mut_ptr().cast(), buf.len()) };
                    if n > 0 {
                        return Ok(Readiness::Bytes(n.unsigned_abs()));
                    }
                    if n == 0 {
                        return Ok(Readiness::Closed);
                    }
                    match signals::errno() {
                        libc::EINTR | libc::EAGAIN => continue,
                        libc::EIO => return Ok(Readiness::Closed),
                        _ => return Err(io::Error::last_os_error()),
                    }
                }
            }
        }

        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            io::Write::write_all(&mut self.file, bytes)
        }

        fn size(&self) -> io::Result<Size> {
            size_of_fd(self.fd())
        }

        fn now(&self) -> Instant {
            Instant::now()
        }

        fn take_signals(&mut self) -> io::Result<Signals> {
            let taken = self.signals.take();
            if taken.contains(Signals::CONT) {
                self.guard.reassert()?;
            }
            Ok(taken)
        }

        fn enable(&mut self, mode: Mode) -> io::Result<()> {
            self.guard.enable(mode)
        }

        fn disable(&mut self, mode: Mode) -> io::Result<()> {
            self.guard.disable(mode)
        }

        fn suspend(&mut self) -> io::Result<()> {
            signals::suspend(&mut self.guard)
        }
    }

    /// The size the kernel holds for the terminal on `fd`; 80x24 when it holds none.
    pub(crate) fn size_of_fd(fd: RawFd) -> io::Result<Size> {
        // SAFETY: `winsize` is plain data for which all-zero is a valid value.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: TIOCGWINSZ fills exactly one `winsize` through the pointer.
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &raw mut ws) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if ws.ws_col == 0 || ws.ws_row == 0 {
            return Ok(Size { cols: 80, rows: 24 });
        }
        Ok(Size {
            cols: ws.ws_col,
            rows: ws.ws_row,
        })
    }
}

#[cfg(any(test, debug_assertions))]
pub use mem::{MemPeer, MemTty};

/// The in-memory terminal: the test seam other crates' tests drive a terminal program
/// through (amacs's TUI loop), `#[cfg(any(test, debug_assertions))]` as AGENTS.md has
/// cross-crate seams.
#[cfg(any(test, debug_assertions))]
mod mem {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use super::{
        Mode, Modes, Readiness, Size, Tty, disable_bytes, enable_bytes, reset_bytes, set_order,
    };
    use crate::signals::Signals;

    /// One scripted thing the terminal does, at a moment on the virtual clock.
    #[derive(Debug)]
    enum Scripted {
        Bytes(Vec<u8>),
        Resize(Size),
        Signal(Signals),
    }

    #[derive(Debug)]
    struct State {
        now: Instant,
        /// When the last scripted item arrives: the next `*_after` counts from it.
        last: Instant,
        script: VecDeque<(Instant, Scripted)>,
        output: Vec<u8>,
        size: Size,
        signals: Signals,
        modes: Modes,
        kitty: u8,
        suspended: usize,
    }

    /// A terminal program's side of an in-memory terminal: a script of input on a virtual clock, and the bytes the program wrote.
    #[derive(Debug)]
    pub struct MemTty {
        state: Arc<Mutex<State>>,
    }

    /// The terminal's side of a [`MemTty`]: it schedules input and reads the output.
    #[derive(Debug, Clone)]
    pub struct MemPeer {
        state: Arc<Mutex<State>>,
    }

    impl MemTty {
        /// A terminal of `size` and the peer that scripts it, the clock at `Instant::now()`.
        #[must_use]
        pub fn pair(size: Size) -> (Self, MemPeer) {
            let now = Instant::now();
            let state = Arc::new(Mutex::new(State {
                now,
                last: now,
                script: VecDeque::new(),
                output: Vec::new(),
                size,
                signals: Signals::NONE,
                modes: Modes::NONE,
                kitty: 0,
                suspended: 0,
            }));
            (
                Self {
                    state: Arc::clone(&state),
                },
                MemPeer { state },
            )
        }

        fn state(&self) -> std::sync::MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    impl MemPeer {
        fn state(&self) -> std::sync::MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn schedule(&self, after: Duration, item: Scripted) {
            let mut state = self.state();
            let at = state.last + after;
            state.last = at;
            state.script.push_back((at, item));
        }

        /// The virtual clock now.
        #[must_use]
        pub fn now(&self) -> Instant {
            self.state().now
        }

        /// `bytes` arrive with the last scheduled item (at the start, before any).
        pub fn send(&self, bytes: &[u8]) {
            self.send_after(Duration::ZERO, bytes);
        }

        /// `bytes` arrive `after` the last scheduled item.
        pub fn send_after(&self, after: Duration, bytes: &[u8]) {
            self.schedule(after, Scripted::Bytes(bytes.to_vec()));
        }

        /// The window becomes `size`, `after` the last scheduled item, with a SIGWINCH.
        pub fn resize_after(&self, after: Duration, size: Size) {
            self.schedule(after, Scripted::Resize(size));
        }

        /// `signals` arrive `after` the last scheduled item.
        pub fn signal_after(&self, after: Duration, signals: Signals) {
            self.schedule(after, Scripted::Signal(signals));
        }

        /// Everything the program wrote, taken.
        #[must_use]
        pub fn take_output(&self) -> Vec<u8> {
            std::mem::take(&mut self.state().output)
        }

        /// The modes the program has set and not put back.
        #[must_use]
        pub fn modes(&self) -> Modes {
            self.state().modes
        }

        /// How many times the program suspended itself.
        #[must_use]
        pub fn suspensions(&self) -> usize {
            self.state().suspended
        }

        /// Whether every scripted item has been read.
        #[must_use]
        pub fn script_done(&self) -> bool {
            self.state().script.is_empty()
        }
    }

    impl Tty for MemTty {
        fn read(&mut self, buf: &mut [u8], deadline: Option<Instant>) -> io::Result<Readiness> {
            let mut state = self.state();
            if !state.signals.is_empty() {
                return Ok(Readiness::Signal);
            }
            let Some((at, _)) = state.script.front() else {
                return Ok(match deadline {
                    Some(deadline) => {
                        state.now = state.now.max(deadline);
                        Readiness::Timeout
                    }
                    None => Readiness::Closed,
                });
            };
            let at = *at;
            if let Some(deadline) = deadline
                && at > deadline
            {
                state.now = state.now.max(deadline);
                return Ok(Readiness::Timeout);
            }
            state.now = state.now.max(at);
            let Some((at, item)) = state.script.pop_front() else {
                return Ok(Readiness::Closed);
            };
            Ok(match item {
                Scripted::Bytes(bytes) => {
                    let n = bytes.len().min(buf.len());
                    buf[..n].copy_from_slice(&bytes[..n]);
                    if let Some(rest) = bytes.get(n..).filter(|rest| !rest.is_empty()) {
                        state
                            .script
                            .push_front((at, Scripted::Bytes(rest.to_vec())));
                    }
                    Readiness::Bytes(n)
                }
                Scripted::Resize(size) => {
                    state.size = size;
                    state.signals = state.signals.union(Signals::WINCH);
                    Readiness::Signal
                }
                Scripted::Signal(signals) => {
                    state.signals = state.signals.union(signals);
                    Readiness::Signal
                }
            })
        }

        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.state().output.extend_from_slice(bytes);
            Ok(())
        }

        fn size(&self) -> io::Result<Size> {
            Ok(self.state().size)
        }

        fn now(&self) -> Instant {
            self.state().now
        }

        fn take_signals(&mut self) -> io::Result<Signals> {
            let mut state = self.state();
            let taken = std::mem::take(&mut state.signals);
            if taken.contains(Signals::CONT) {
                let (modes, kitty) = (state.modes, state.kitty);
                for mode in set_order(modes, kitty) {
                    state.output.extend(mode.reassert_bytes());
                }
            }
            Ok(taken)
        }

        fn enable(&mut self, mode: Mode) -> io::Result<()> {
            let mut state = self.state();
            if state.modes.contains(mode.bit()) {
                return Ok(());
            }
            if let Mode::KittyKeys(flags) = mode {
                state.kitty = flags;
            }
            let bytes = enable_bytes(mode, state.modes, state.kitty);
            state.modes = state.modes.union(mode.bit());
            state.output.extend(bytes);
            Ok(())
        }

        fn disable(&mut self, mode: Mode) -> io::Result<()> {
            let mut state = self.state();
            if !state.modes.contains(mode.bit()) {
                return Ok(());
            }
            let bytes = disable_bytes(mode, state.modes, state.kitty);
            state.modes = state.modes.difference(mode.bit());
            state.output.extend(bytes);
            Ok(())
        }

        fn suspend(&mut self) -> io::Result<()> {
            let mut state = self.state();
            let (modes, kitty) = (state.modes, state.kitty);
            state.output.extend(reset_bytes(modes));
            for mode in set_order(modes, kitty) {
                state.output.extend(mode.set_bytes());
            }
            state.suspended = state.suspended.saturating_add(1);
            state.signals = state.signals.union(Signals::CONT);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{MemTty, Mode, Modes, Readiness, Size, Tty, reset_bytes};
    use crate::signals::Signals;

    const SIZE: Size = Size { cols: 80, rows: 24 };

    #[test]
    fn mem_tty_reads_its_script_on_a_virtual_clock() {
        let (mut tty, peer) = MemTty::pair(SIZE);
        let start = tty.now();
        peer.send(b"ab");
        peer.send_after(Duration::from_millis(300), b"cdef");
        peer.resize_after(
            Duration::from_millis(5),
            Size {
                cols: 100,
                rows: 30,
            },
        );
        let mut buf = [0u8; 3];
        assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Bytes(2));
        assert_eq!(&buf[..2], b"ab");
        let deadline = start + Duration::from_millis(100);
        assert_eq!(
            tty.read(&mut buf, Some(deadline)).unwrap(),
            Readiness::Timeout
        );
        assert_eq!(tty.now(), deadline);
        assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Bytes(3));
        assert_eq!(&buf, b"cde");
        assert_eq!(tty.now(), start + Duration::from_millis(300));
        assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Bytes(1));
        assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Signal);
        assert_eq!(tty.take_signals().unwrap(), Signals::WINCH);
        assert_eq!(
            tty.size().unwrap(),
            Size {
                cols: 100,
                rows: 30
            }
        );
        assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Closed);
        assert!(peer.script_done());
    }

    #[test]
    fn modes_are_set_once_and_put_back_in_order() {
        let (mut tty, peer) = MemTty::pair(SIZE);
        for mode in [
            Mode::AltScreen,
            Mode::KittyKeys(5),
            Mode::Mouse,
            Mode::Paste,
            Mode::Focus,
            Mode::InBandResize,
            Mode::Cursor,
        ] {
            tty.enable(mode).unwrap();
            tty.enable(mode).unwrap();
        }
        assert_eq!(
            peer.take_output(),
            b"\x1b[?1049h\x1b[>5u\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[?2048h"
        );
        let all = peer.modes();
        assert_eq!(all, Modes::from_bits(0x7f));
        assert_eq!(
            reset_bytes(all),
            b"\x1b[?2026l\x1b[m\x1b[0 q\x1b[?25h\x1b[?2048l\x1b[?1004l\x1b[?2004l\
              \x1b[?1006l\x1b[?1002l\x1b[<u\x1b[?1049l"
        );
        tty.suspend().unwrap();
        let mut expected = reset_bytes(all);
        expected.extend_from_slice(
            b"\x1b[?1049h\x1b[>5u\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[?2048h",
        );
        assert_eq!(peer.take_output(), expected);
        assert_eq!(peer.suspensions(), 1);
        assert_eq!(tty.take_signals().unwrap(), Signals::CONT);
        assert_eq!(
            peer.take_output(),
            b"\x1b[?1049h\x1b[=5;1u\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[?2048h"
        );
        tty.disable(Mode::Paste).unwrap();
        tty.disable(Mode::Paste).unwrap();
        assert_eq!(peer.take_output(), b"\x1b[?2004l");
    }

    /// The kitty flags of the screen aterm's engine shows after `bytes`, and whether it
    /// shows the alternate screen.
    fn shown(bytes: &[u8]) -> (String, bool) {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        term.process(bytes);
        let _ = term.take_response();
        term.process(b"\x1b[?u\x1b[?1049$p");
        let reply = term.take_response().expect("the engine answers");
        let reply = String::from_utf8_lossy(&reply).into_owned();
        let flags = reply
            .split("\x1b[?")
            .find_map(|part| part.strip_suffix('u'))
            .expect("the kitty flags")
            .to_owned();
        (flags, reply.contains("1049;1$y"))
    }

    /// Each screen has its own kitty stack: whichever order the kitty entry and the
    /// alternate screen are set and put back in, the entry is on the screen shown, and
    /// a restore leaves the main screen shown with no flags.
    #[test]
    fn the_kitty_entry_follows_the_screen_shown() {
        for kitty_first in [true, false] {
            let (mut tty, peer) = MemTty::pair(SIZE);
            let order = if kitty_first {
                [Mode::KittyKeys(5), Mode::AltScreen]
            } else {
                [Mode::AltScreen, Mode::KittyKeys(5)]
            };
            let mut seen = Vec::new();
            tty.enable(order[0]).unwrap();
            tty.enable(order[1]).unwrap();
            seen.extend(peer.take_output());
            assert_eq!(shown(&seen), ("5".to_owned(), true), "{order:?}");

            tty.disable(Mode::AltScreen).unwrap();
            seen.extend(peer.take_output());
            assert_eq!(shown(&seen), ("5".to_owned(), false), "{order:?}");
            tty.enable(Mode::AltScreen).unwrap();
            seen.extend(peer.take_output());
            assert_eq!(shown(&seen), ("5".to_owned(), true), "{order:?}");

            seen.extend(reset_bytes(peer.modes()));
            assert_eq!(shown(&seen), ("0".to_owned(), false), "{order:?}");
        }
        let (mut tty, peer) = MemTty::pair(SIZE);
        tty.enable(Mode::KittyKeys(5)).unwrap();
        let mut seen = peer.take_output();
        assert_eq!(shown(&seen), ("5".to_owned(), false));
        seen.extend(reset_bytes(peer.modes()));
        assert_eq!(shown(&seen), ("0".to_owned(), false));
    }

    /// A suspension leaves CONT pending, which the next read reports before any input,
    /// as the real terminal's wake does; so does any signal no read has taken.
    #[test]
    fn a_pending_signal_is_read_before_input() {
        let (mut tty, peer) = MemTty::pair(SIZE);
        peer.send(b"x");
        tty.suspend().unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Signal);
        assert_eq!(
            tty.read(&mut buf, None).unwrap(),
            Readiness::Signal,
            "until taken"
        );
        assert_eq!(tty.take_signals().unwrap(), Signals::CONT);
        assert_eq!(tty.read(&mut buf, None).unwrap(), Readiness::Bytes(1));
    }
}
