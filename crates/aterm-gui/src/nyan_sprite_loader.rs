// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, asynchronous loading for the optional cursor-Nyan PNG.
//!
//! The redraw thread owns only [`NyanSpriteLoader`]'s nonblocking control half.
//! A single pre-armed worker owns path expansion, file open/read, and PNG decode;
//! decoded pixels return through a one-slot channel as an `Arc`, so publishing a
//! sprite to several windows never copies a multi-megabyte image on the UI thread.

use std::fs::{File, OpenOptions};
use std::io::{Read, Result as IoResult};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

use winit::event_loop::EventLoopProxy;

use crate::{App, Wake};

/// PNG bytes admitted from disk. The published 1024x1024 RGBA sprite is at most
/// 4 MiB; this separate cap leaves room for ordinary PNG metadata while keeping
/// the pre-decode read allocation strictly bounded.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_SPRITE_DIMENSION: usize = 1024;
const REQUEST_CAPACITY: usize = 1;
const RESULT_CAPACITY: usize = 1;

pub(crate) type SharedSprite = (u16, u16, Arc<[u8]>);

#[derive(Clone, Debug)]
struct Request {
    generation: u64,
    raw_path: Arc<str>,
}

#[derive(Debug)]
struct Loaded {
    generation: u64,
    raw_path: Arc<str>,
    sprite: Option<SharedSprite>,
}

/// One accepted publication. The outer `Option` returned by `poll` means
/// "nothing current arrived"; this inner option means custom sprite vs built-in.
pub(crate) struct SpriteInstall(pub(crate) Option<SharedSprite>);

/// UI-thread half of the sprite loader. Every hot-path operation is bounded and
/// nonblocking: an atomic readiness probe, `try_recv`, and `try_send` only.
pub(crate) struct NyanSpriteLoader {
    request_tx: Option<SyncSender<Request>>,
    result_rx: Receiver<Loaded>,
    #[cfg(test)]
    test_result_tx: Option<SyncSender<Loaded>>,
    result_ready: Arc<AtomicBool>,
    current_generation: Arc<AtomicU64>,
    generation: u64,
    desired_raw: Option<Arc<str>>,
    pending: Option<Request>,
    installed: Option<SharedSprite>,
}

impl NyanSpriteLoader {
    /// Pre-arm the sole worker during App construction, before the first redraw.
    pub(crate) fn spawn(proxy: EventLoopProxy<Wake>) -> Self {
        let (request_tx, request_rx) = sync_channel(REQUEST_CAPACITY);
        let (result_tx, result_rx) = sync_channel(RESULT_CAPACITY);
        #[cfg(test)]
        let test_result_tx = result_tx.clone();
        let result_ready = Arc::new(AtomicBool::new(false));
        let current_generation = Arc::new(AtomicU64::new(0));
        let worker_ready = Arc::clone(&result_ready);
        let worker_generation = Arc::clone(&current_generation);
        // Bench instrument (ATERM_FLOOD_QUIET=1): skip the worker spawn so flood
        // measurements see no effect threads; the loader stays inert exactly as
        // on a spawn failure.
        let spawned = if crate::bench_knobs::flood_quiet() {
            false
        } else {
            std::thread::Builder::new()
                .name("aterm-nyan-sprite".into())
                .spawn(move || {
                    worker_loop(
                        request_rx,
                        result_tx,
                        worker_ready,
                        worker_generation,
                        proxy,
                    );
                })
                .is_ok()
        };

        Self {
            request_tx: spawned.then_some(request_tx),
            result_rx,
            #[cfg(test)]
            test_result_tx: Some(test_result_tx),
            result_ready,
            current_generation,
            generation: 0,
            desired_raw: None,
            pending: None,
            installed: None,
        }
    }

    /// Inert loader for unit-test Apps that have no event loop. Tests of this
    /// module use [`Self::test_pair`] to exercise publication explicitly.
    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        let (result_tx, result_rx) = sync_channel(RESULT_CAPACITY);
        Self {
            request_tx: None,
            result_rx,
            test_result_tx: Some(result_tx),
            result_ready: Arc::new(AtomicBool::new(false)),
            current_generation: Arc::new(AtomicU64::new(0)),
            generation: 0,
            desired_raw: None,
            pending: None,
            installed: None,
        }
    }

    /// Publish a changed config value to the worker without waiting. Returning
    /// `true` tells the host to clear any old custom art immediately. An invalid
    /// (oversized) path is treated as a missing sprite and never reaches the OS.
    pub(crate) fn request_if_changed(&mut self, raw_path: Option<&str>) -> bool {
        if self.desired_raw.as_deref() == raw_path {
            return false;
        }

        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.generation = 1;
        }
        self.current_generation
            .store(self.generation, Ordering::Release);
        self.desired_raw = raw_path.map(Arc::<str>::from);
        self.installed = None;
        self.pending = None;

        if let Some(raw_path) = self.desired_raw.as_ref()
            && raw_path.len() <= MAX_PATH_BYTES
        {
            self.pending = Some(Request {
                generation: self.generation,
                raw_path: Arc::clone(raw_path),
            });
            self.try_dispatch_pending();
        }
        true
    }

    /// Redraw-safe result poll. The common path is one atomic load; the ready
    /// path drains only with `try_recv`. File and decoder capabilities exist only
    /// in `worker_loop`/`load_sprite`, never in this control object.
    pub(crate) fn poll(&mut self) -> Option<SpriteInstall> {
        // Keep the overwhelmingly common redraw path read-only. An unconditional
        // `swap(false)` dirtied this cache line on every presented frame even
        // though sprite publications are rare; the worker's one-slot result
        // channel means the confirming swap cannot race a second publication
        // ahead of draining the first.
        if !self.result_ready.load(Ordering::Acquire) {
            return None;
        }
        if !self.result_ready.swap(false, Ordering::AcqRel) {
            return None;
        }

        let mut accepted = None;
        while let Ok(loaded) = self.result_rx.try_recv() {
            if result_is_current(
                loaded.generation,
                &loaded.raw_path,
                self.generation,
                self.desired_raw.as_deref(),
            ) {
                self.installed = loaded.sprite;
                accepted = Some(SpriteInstall(self.installed.clone()));
            }
        }
        // A newer config request may have coalesced while the one-slot request
        // channel was occupied. Completion frees capacity; retry latest only.
        self.try_dispatch_pending();
        accepted
    }

    pub(crate) fn installed(&self) -> Option<SharedSprite> {
        self.installed.clone()
    }

    #[cfg(test)]
    pub(crate) fn set_installed_for_test(&mut self, sprite: SharedSprite) {
        self.installed = Some(sprite);
    }

    fn try_dispatch_pending(&mut self) {
        let (Some(tx), Some(request)) = (self.request_tx.as_ref(), self.pending.take()) else {
            return;
        };
        match tx.try_send(request) {
            Ok(()) => {}
            Err(TrySendError::Full(request)) => self.pending = Some(request),
            Err(TrySendError::Disconnected(_)) => self.request_tx = None,
        }
    }

    /// Inject one exact current publication through the real one-slot result
    /// channel without blocking. App-level tests use this to deterministically
    /// race a redraw poll against the still-queued worker wake.
    #[cfg(test)]
    pub(crate) fn publish_current_for_test(&mut self, sprite: Option<SharedSprite>) -> bool {
        if self.generation == 0 {
            self.generation = 1;
            self.desired_raw = Some(Arc::from("__aterm_nyan_test__.png"));
            self.current_generation.store(1, Ordering::Release);
        }
        let Some(raw_path) = self.desired_raw.as_ref() else {
            return false;
        };
        let Some(tx) = self.test_result_tx.as_ref() else {
            return false;
        };
        let loaded = Loaded {
            generation: self.generation,
            raw_path: Arc::clone(raw_path),
            sprite,
        };
        if tx.try_send(loaded).is_err() {
            return false;
        }
        self.result_ready.store(true, Ordering::Release);
        true
    }

    #[cfg(test)]
    fn test_pair() -> (Self, SyncSender<Loaded>, Arc<AtomicBool>) {
        let (_request_tx, request_rx) = sync_channel::<Request>(REQUEST_CAPACITY);
        drop(request_rx);
        let (result_tx, result_rx) = sync_channel(RESULT_CAPACITY);
        let result_ready = Arc::new(AtomicBool::new(false));
        (
            Self {
                request_tx: None,
                result_rx,
                test_result_tx: Some(result_tx.clone()),
                result_ready: Arc::clone(&result_ready),
                current_generation: Arc::new(AtomicU64::new(0)),
                generation: 0,
                desired_raw: None,
                pending: None,
                installed: None,
            },
            result_tx,
            result_ready,
        )
    }
}

/// Apply a new config value outside redraw (startup/config reload). Clearing the
/// previous custom sprite is O(window-count) and O(1) per window (`Arc` only).
pub(crate) fn sync_config(app: &mut App) {
    let changed = app
        .nyan_sprite_loader
        .request_if_changed(app.config.cursor_nyan_sprite_raw());
    if changed {
        for ws in app.windows.values_mut() {
            ws.word_decos.set_nyan_sprite_shared(None);
        }
        app.request_redraw_all_windows();
    }
}

/// Poll from a redraw or worker wake. Returns whether a current result was
/// installed; stale generations are consumed but never touch visible state.
pub(crate) fn poll_and_install(app: &mut App) -> bool {
    let Some(SpriteInstall(sprite)) = app.nyan_sprite_loader.poll() else {
        return false;
    };
    for ws in app.windows.values_mut() {
        ws.word_decos.set_nyan_sprite_shared(sprite.clone());
    }
    true
}

fn result_is_current(
    result_generation: u64,
    result_raw: &str,
    current_generation: u64,
    desired_raw: Option<&str>,
) -> bool {
    result_generation == current_generation && desired_raw == Some(result_raw)
}

fn worker_loop(
    request_rx: Receiver<Request>,
    result_tx: SyncSender<Loaded>,
    result_ready: Arc<AtomicBool>,
    current_generation: Arc<AtomicU64>,
    proxy: EventLoopProxy<Wake>,
) {
    while let Ok(mut request) = request_rx.recv() {
        // Coalesce queued config churn before touching the filesystem.
        while let Ok(newer) = request_rx.try_recv() {
            request = newer;
        }

        let sprite = if current_generation.load(Ordering::Acquire) == request.generation {
            load_sprite(
                &request.raw_path,
                request.generation,
                &current_generation,
                aterm_render::decode_png_rgba8,
            )
        } else {
            None
        };
        let loaded = Loaded {
            generation: request.generation,
            raw_path: request.raw_path,
            sprite,
        };
        // Blocking is allowed here, off the UI thread. Capacity one bounds the
        // decoded publication even if the event loop is temporarily busy.
        if result_tx.send(loaded).is_err() {
            return;
        }
        result_ready.store(true, Ordering::Release);
        let _ = proxy.send_event(Wake::NyanSpriteReady);
    }
}

fn load_sprite<F>(
    raw_path: &str,
    generation: u64,
    current_generation: &AtomicU64,
    decode: F,
) -> Option<SharedSprite>
where
    F: FnOnce(&[u8]) -> Option<(Vec<u8>, usize, usize)>,
{
    let path = expand_path(raw_path);
    let bytes = read_bounded_regular(&path).ok()?;
    // A newer config arrived during IO: skip even the off-thread decode.
    if current_generation.load(Ordering::Acquire) != generation {
        return None;
    }
    // Parse the fixed PNG signature + IHDR dimensions before invoking the PNG
    // decoder, so its output allocation is bounded to our 4 MiB publication cap.
    if !png_header_within_bounds(&bytes) {
        return None;
    }
    let (rgba, width, height) = decode(&bytes)?;
    if current_generation.load(Ordering::Acquire) != generation
        || width == 0
        || height == 0
        || width > MAX_SPRITE_DIMENSION
        || height > MAX_SPRITE_DIMENSION
    {
        return None;
    }
    let expected = width.checked_mul(height)?.checked_mul(4)?;
    if rgba.len() != expected {
        return None;
    }
    Some((
        u16::try_from(width).ok()?,
        u16::try_from(height).ok()?,
        Arc::from(rgba.into_boxed_slice()),
    ))
}

fn expand_path(raw_path: &str) -> PathBuf {
    raw_path.strip_prefix("~/").map_or_else(
        || PathBuf::from(raw_path),
        |rest| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(rest))
                .unwrap_or_else(|| PathBuf::from(raw_path))
        },
    )
}

fn open_nonblocking(path: &Path) -> IoResult<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    options.open(path)
}

/// Read exactly the regular-file size observed before allocation. A growth race
/// is rejected by the one-byte probe; a shrink race fails `read_exact`. No read
/// can grow the Vec beyond the already-checked metadata length.
fn read_bounded_regular(path: &Path) -> IoResult<Vec<u8>> {
    let mut file = open_nonblocking(path)?;
    let metadata = file.metadata()?;
    let length = metadata.len();
    if !metadata.is_file() || length == 0 || length > MAX_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Nyan sprite must be a bounded regular file",
        ));
    }
    let length = usize::try_from(length).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "sprite length overflow")
    })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::OutOfMemory, "sprite allocation refused")
    })?;
    bytes.resize(length, 0);
    file.read_exact(&mut bytes)?;
    let mut extra = [0u8; 1];
    if file.read(&mut extra)? != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Nyan sprite changed while loading",
        ));
    }
    Ok(bytes)
}

fn png_header_within_bounds(bytes: &[u8]) -> bool {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24
        || &bytes[..8] != PNG_SIGNATURE
        || u32::from_be_bytes(bytes[8..12].try_into().expect("four-byte slice")) != 13
        || &bytes[12..16] != b"IHDR"
    {
        return false;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("four-byte slice"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("four-byte slice"));
    width > 0
        && height > 0
        && width <= MAX_SPRITE_DIMENSION as u32
        && height <= MAX_SPRITE_DIMENSION as u32
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn model_step(
        model: &aterm_spec::derive::Model,
        state: &mut aterm_spec::interp::State,
        action: &'static str,
    ) {
        let successors = model.successors(action, state);
        assert_eq!(successors.len(), 1, "{action}: {state:?}");
        *state = successors[0].clone();
        for invariant in &model.invariants {
            assert!(model.check_invariant(invariant.name, state));
        }
    }

    #[test]
    fn redraw_poll_is_nonblocking_and_has_no_decoder_capability() {
        let (mut loader, result_tx, ready) = NyanSpriteLoader::test_pair();
        // Empty live channel: the redraw-side poll returns through its atomic
        // fast path, rather than waiting for the retained sender.
        assert!(loader.poll().is_none());

        loader.generation = 7;
        loader.desired_raw = Some(Arc::from("sprite.png"));
        result_tx
            .send(Loaded {
                generation: 7,
                raw_path: Arc::from("sprite.png"),
                sprite: None,
            })
            .expect("one-slot test publication");
        ready.store(true, Ordering::Release);
        assert!(matches!(loader.poll(), Some(SpriteInstall(None))));
        // `poll` cannot decode by construction: neither a path nor a decoder is
        // stored in the UI control half. This assertion also pins one-shot drain.
        assert!(loader.poll().is_none());
    }

    #[test]
    fn oversized_regular_file_is_rejected_before_decoder() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aterm-nyan-oversize-{}-{unique}.png",
            std::process::id()
        ));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("unique sparse test file");
        file.set_len(MAX_FILE_BYTES + 1)
            .expect("make sparse oversized file");
        drop(file);

        let calls = AtomicUsize::new(0);
        let current = AtomicU64::new(1);
        let sprite = load_sprite(path.to_str().expect("UTF-8 temp path"), 1, &current, |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            None
        });
        let _ = std::fs::remove_file(&path);
        assert!(sprite.is_none());
        assert_eq!(calls.load(Ordering::Relaxed), 0, "decode must not run");
    }

    #[test]
    fn bounded_valid_header_publishes_exact_decoded_sprite() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aterm-nyan-valid-{}-{unique}.png",
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("unique bounded test file");
        let mut header = Vec::from(*b"\x89PNG\r\n\x1a\n");
        header.extend_from_slice(&13u32.to_be_bytes());
        header.extend_from_slice(b"IHDR");
        header.extend_from_slice(&2u32.to_be_bytes());
        header.extend_from_slice(&3u32.to_be_bytes());
        file.write_all(&header).expect("write bounded PNG header");
        drop(file);

        let calls = AtomicUsize::new(0);
        let current = AtomicU64::new(9);
        let sprite = load_sprite(path.to_str().expect("UTF-8 temp path"), 9, &current, |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            Some((vec![0xabu8; 2 * 3 * 4], 2, 3))
        })
        .expect("bounded current sprite publishes");
        let _ = std::fs::remove_file(&path);
        assert_eq!((sprite.0, sprite.1, sprite.2.len()), (2, 3, 24));
        assert!(sprite.2.iter().all(|byte| *byte == 0xab));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn saturated_request_slot_coalesces_to_latest_path() {
        let model = aterm_spec::derive::nyan_sprite_loader_model();
        let mut state = model.init_state();
        let (request_tx, request_rx) = sync_channel(REQUEST_CAPACITY);
        let (_result_tx, result_rx) = sync_channel(RESULT_CAPACITY);
        let mut loader = NyanSpriteLoader {
            request_tx: Some(request_tx),
            result_rx,
            test_result_tx: None,
            result_ready: Arc::new(AtomicBool::new(false)),
            current_generation: Arc::new(AtomicU64::new(0)),
            generation: 0,
            desired_raw: None,
            pending: None,
            installed: None,
        };

        assert!(loader.request_if_changed(Some("first.png")));
        model_step(&model, &mut state, "RequestFirst");
        assert!(loader.request_if_changed(Some("superseded.png")));
        model_step(&model, &mut state, "RequestSecondWhileFull");
        assert!(loader.request_if_changed(Some("latest.png")));
        model_step(&model, &mut state, "RequestThirdReplacesPending");
        let first = request_rx.try_recv().expect("one bounded queued request");
        model_step(&model, &mut state, "WorkerTakesQueued");
        assert_eq!(&*first.raw_path, "first.png");
        loader.try_dispatch_pending();
        model_step(&model, &mut state, "RetryLatestPending");
        let latest = request_rx.try_recv().expect("latest coalesced request");
        assert_eq!(&*latest.raw_path, "latest.png");
        assert_eq!(latest.generation, loader.generation);
        assert_eq!(state["request_generation"], 3);
        assert_eq!(state["pending_count"], 0);
        assert!(
            request_rx.try_recv().is_err(),
            "superseded path never queues"
        );
    }

    #[test]
    fn stale_config_result_cannot_replace_newer_sprite() {
        let model = aterm_spec::derive::nyan_sprite_loader_model();
        let mut state = model.init_state();
        for action in [
            "RequestFirst",
            "RequestSecondWhileFull",
            "WorkerTakesQueued",
            "WorkerCompletes",
        ] {
            model_step(&model, &mut state, action);
        }
        let (mut loader, result_tx, ready) = NyanSpriteLoader::test_pair();
        loader.generation = 2;
        loader.desired_raw = Some(Arc::from("new.png"));
        result_tx
            .send(Loaded {
                generation: 1,
                raw_path: Arc::from("old.png"),
                sprite: None,
            })
            .expect("one-slot test publication");
        ready.store(true, Ordering::Release);
        assert!(loader.poll().is_none());
        model_step(&model, &mut state, "RejectStaleGeneration");
        assert!(loader.installed().is_none());
        assert_eq!(state["installed_generation"], 0);
    }

    #[test]
    fn currentness_guard_requires_generation_and_exact_path() {
        let model = aterm_spec::derive::nyan_sprite_loader_model();
        assert!(result_is_current(3, "a.png", 3, Some("a.png")));
        assert!(!result_is_current(2, "a.png", 3, Some("a.png")));
        assert!(!result_is_current(3, "old.png", 3, Some("new.png")));
        assert!(!result_is_current(3, "a.png", 3, None));

        let mut exact = model.init_state();
        for action in [
            "RequestFirst",
            "WorkerTakesQueued",
            "WorkerCompletes",
            "AcceptCurrentResult",
        ] {
            model_step(&model, &mut exact, action);
        }
        assert_eq!(exact["installed_generation"], 1);
        assert_eq!(exact["fanout_requested"], 1);

        let mut wrong_path = model.init_state();
        for action in [
            "RequestFirst",
            "WorkerTakesQueued",
            "WorkerCompletes",
            "CorruptCurrentResultPath",
            "RejectWrongPath",
        ] {
            model_step(&model, &mut wrong_path, action);
        }
        assert_eq!(wrong_path["installed_generation"], 0);
    }
}
