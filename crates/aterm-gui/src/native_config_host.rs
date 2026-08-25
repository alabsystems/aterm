// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded off-thread config analysis for Settings ▸ Manual.
//!
//! TOML/schema analysis and filesystem/font-backed semantics share one worker,
//! a one-entry request channel, and one latest-wins pending slot. Rendering only
//! consumes a completed immutable projection. Completions carry both document
//! and environment generations and are accepted only by the matching editor.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

use winit::event_loop::EventLoopProxy;

use crate::document_store::DocumentId;
use crate::native_config_language::MAX_CONFIG_ANALYSIS_BYTES;
use crate::{App, Wake};

const REQUEST_CAPACITY: usize = 1;

#[derive(Clone, Debug)]
struct Request {
    document: DocumentId,
    revision: u64,
    analysis_generation: u64,
    backend_gpu: bool,
    source: Arc<str>,
    assets: Arc<crate::app_config::ConfigAssetCatalog>,
}

#[derive(Debug)]
pub(crate) struct Completion {
    pub(crate) document: DocumentId,
    pub(crate) revision: u64,
    pub(crate) analysis_generation: u64,
    pub(crate) analysis: crate::native_config_language::ConfigAnalysis,
}

/// UI-thread half of the Manual host-diagnostics lane.
///
/// `request` and `worker_drained` are strictly nonblocking. If the worker's
/// bounded channel is occupied, `pending` is replaced with the newest document
/// revision, so an arbitrarily fast edit stream retains constant memory and the
/// final revision is never lost.
pub(crate) struct HostDiagnosticsLane {
    request_tx: SyncSender<Request>,
    pending: Option<Request>,
}

impl HostDiagnosticsLane {
    pub(crate) fn spawn(proxy: EventLoopProxy<Wake>) -> Result<Self, String> {
        let (request_tx, request_rx) = sync_channel(REQUEST_CAPACITY);
        std::thread::Builder::new()
            .name("aterm-config-diagnostics".into())
            .spawn(move || worker_loop(request_rx, proxy))
            .map_err(|error| format!("could not start Manual diagnostics worker: {error}"))?;
        Ok(Self {
            request_tx,
            pending: None,
        })
    }

    /// Queue the exact immutable source revision. Oversized files already have
    /// a pure hard diagnostic and deliberately do not consume worker capacity.
    pub(crate) fn request(
        &mut self,
        document: DocumentId,
        revision: u64,
        analysis_generation: u64,
        backend_gpu: bool,
        source: Arc<str>,
        assets: Arc<crate::app_config::ConfigAssetCatalog>,
    ) {
        if source.len() > MAX_CONFIG_ANALYSIS_BYTES {
            self.pending = None;
            return;
        }
        self.pending = Some(Request {
            document,
            revision,
            analysis_generation,
            backend_gpu,
            source,
            assets,
        });
        self.try_dispatch_pending();
    }

    /// One completion means the worker advanced far enough that its request
    /// slot may be free. Retry the retained latest revision without waiting.
    pub(crate) fn worker_drained(&mut self) {
        self.try_dispatch_pending();
    }

    fn try_dispatch_pending(&mut self) {
        let Some(request) = self.pending.take() else {
            return;
        };
        match self.request_tx.try_send(request) {
            Ok(()) => {}
            Err(TrySendError::Full(request)) => self.pending = Some(request),
            Err(TrySendError::Disconnected(_)) => {
                // The event loop is closing or the worker is gone. The editor
                // keeps Save closed rather than treating unvalidated bytes as
                // safe.
                self.pending = None;
            }
        }
    }

    #[cfg(test)]
    fn test_pair() -> (Self, Receiver<Request>) {
        let (request_tx, request_rx) = sync_channel(REQUEST_CAPACITY);
        (
            Self {
                request_tx,
                pending: None,
            },
            request_rx,
        )
    }
}

fn worker_loop(request_rx: Receiver<Request>, proxy: EventLoopProxy<Wake>) {
    while let Ok(mut request) = request_rx.recv() {
        // If the UI filled the one-entry channel before this turn began, skip
        // intermediate text and resolve only its most recent queued revision.
        while let Ok(newer) = request_rx.try_recv() {
            request = newer;
        }
        let analysis = std::panic::catch_unwind(|| {
            let mut analysis = crate::native_config_language::analyze(&request.source);
            if !analysis.has_errors() {
                let diagnostics = crate::native_config_language::analyze_host_with_assets(
                    &request.source,
                    request.backend_gpu,
                    &request.assets,
                    crate::net_listen::launched_inside_aterm(),
                );
                let _ = analysis.merge_host_diagnostics(diagnostics);
            }
            analysis
        })
        .unwrap_or_else(|_| {
            crate::native_config_language::ConfigAnalysis::pending_failure(
                "background config validation failed; edit the file to retry",
            )
        });
        if proxy
            .send_event(Wake::NativeConfigDiagnosticsFinished(Completion {
                document: request.document,
                revision: request.revision,
                analysis_generation: request.analysis_generation,
                analysis,
            }))
            .is_err()
        {
            return;
        }
    }
}

impl App {
    /// Admit the current Manual source revision to the host lane exactly once.
    /// Unit-test Apps have no worker and execute the same function inline;
    /// visible and headless production windows never parse or probe on the
    /// event loop.
    pub(crate) fn request_config_host_diagnostics(&mut self, document: DocumentId) {
        let Some(snapshot) = self.document_store.snapshot(document) else {
            return;
        };
        let Some(revision) = self.document_store.revision(document) else {
            return;
        };
        let analysis_generation = self.native_config_service.analysis_generation();
        let config_assets = Arc::clone(&self.native_config_service.snapshot().assets);
        if !self
            .native_runtime
            .begin_config_host_analysis(document, revision, analysis_generation)
        {
            return;
        }
        // `begin_config_host_analysis` fail-closes every Save face immediately,
        // including same-document-revision environment refreshes. Publish that
        // pending state now instead of leaving a cached valid modeline/button
        // visible until the worker happens to complete.
        for (window, instance, view) in self.document_native_views(document) {
            self.invalidate_native_view_cache(window, view, crate::native_app::DamageRegion::All);
            self.refresh_native_presentation(window, instance, view);
        }
        self.request_redraw_all_windows();

        if snapshot.text.len() > MAX_CONFIG_ANALYSIS_BYTES {
            self.finish_config_host_diagnostics(Completion {
                document,
                revision,
                analysis_generation,
                analysis: crate::native_config_language::ConfigAnalysis::too_large(),
            });
            return;
        }

        // A headless launch's UNREDEEMED GPU intent counts as GPU here: the
        // capability warnings answer "can this run do it", and `ensure_pixel_backend`
        // may still install the device on the first pixel demand. Reading the live
        // CPU backend alone would report a denial the same launch would not have
        // reported before the deferral existed.
        let backend_gpu = self.backend.is_gpu() || self.backend_kind_undecided();
        if let Some(lane) = self.native_config_host.as_mut() {
            lane.request(
                document,
                revision,
                analysis_generation,
                backend_gpu,
                Arc::clone(&snapshot.text),
                Arc::clone(&config_assets),
            );
            return;
        }

        // Unit-test Apps intentionally have no event-loop proxy/worker. Execute
        // the same complete analysis inline there; a production worker startup
        // failure is fail-visible and blocks Save instead of moving parse/I/O
        // work back onto the UI thread.
        let mut analysis = if self.proxy.is_none() {
            crate::native_config_language::analyze(&snapshot.text)
        } else {
            crate::native_config_language::ConfigAnalysis::pending_failure(
                "background config validator is unavailable; restart aterm to retry",
            )
        };
        if self.proxy.is_none() && !analysis.has_errors() {
            let diagnostics = crate::native_config_language::analyze_host_with_assets(
                &snapshot.text,
                backend_gpu,
                &config_assets,
                crate::net_listen::launched_inside_aterm(),
            );
            let _ = analysis.merge_host_diagnostics(diagnostics);
        }
        self.finish_config_host_diagnostics(Completion {
            document,
            revision,
            analysis_generation,
            analysis,
        });
    }

    /// Reduce one worker publication. The lane is pumped even when the result
    /// is stale; only an exact current document revision can alter presentation.
    pub(crate) fn finish_config_host_diagnostics(&mut self, completion: Completion) -> bool {
        if let Some(lane) = self.native_config_host.as_mut() {
            lane.worker_drained();
        }
        if !self.native_runtime.finish_config_host_analysis(
            completion.document,
            completion.revision,
            completion.analysis_generation,
            completion.analysis,
        ) {
            return false;
        }

        let views = {
            let windows = &self.windows;
            let view_store = &self.view_store;
            let runtime = &self.native_runtime;
            windows
                .iter()
                .flat_map(|(window, state)| {
                    state.tab_set.tabs().iter().flat_map(move |tab| {
                        tab.root.leaves().into_iter().filter_map(move |view| {
                            let crate::tab_model::View::Native(native) =
                                view_store.get(view).copied()?
                            else {
                                return None;
                            };
                            (runtime.document_id(native.instance) == Some(completion.document))
                                .then_some((*window, native.instance, view))
                        })
                    })
                })
                .collect::<Vec<_>>()
        };
        for (window, instance, view) in views {
            self.invalidate_native_view_cache(window, view, crate::native_app::DamageRegion::All);
            self.refresh_native_presentation(window, instance, view);
        }
        self.request_redraw_all_windows();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_store::DocumentStore;

    fn document() -> DocumentId {
        let mut store = DocumentStore::new();
        store.open(
            "document:test".to_string(),
            "manual diagnostics".to_string(),
        )
    }

    fn assets() -> Arc<crate::app_config::ConfigAssetCatalog> {
        crate::app_config::ConfigAssetCatalog::empty()
    }

    #[test]
    fn shipping_lane_conforms_to_latest_revision_model() {
        let document = document();
        let (mut lane, receiver) = HostDiagnosticsLane::test_pair();
        let model = aterm_spec::derive::manual_config_diagnostics_lane_model();
        let mut state = model.init_state();

        // Abstract generation 1: document 1 under host environment 1.
        lane.request(
            document,
            1,
            1,
            true,
            Arc::from("theme = \"One\"\n"),
            assets(),
        );
        assert!(model.fire("RequestFirst", &mut state));
        // Generation 2 changes only the environment. This is the byte-equal
        // asset/font-refresh case the original revision-only latch lost.
        lane.request(
            document,
            1,
            2,
            true,
            Arc::from("theme = \"One\"\n"),
            assets(),
        );
        assert!(model.fire("RequestSecond", &mut state));
        // Generation 3 is a later document edit under that environment.
        lane.request(
            document,
            2,
            2,
            true,
            Arc::from("theme = \"Three\"\n"),
            assets(),
        );
        assert!(model.fire("RequestThird", &mut state));
        assert_eq!(
            lane.pending
                .as_ref()
                .map(|request| (request.revision, request.analysis_generation)),
            state
                .get("pending_revision")
                .copied()
                .filter(|revision| *revision > 0)
                .map(|_| (2, 2))
        );

        assert_eq!(receiver.recv().unwrap().revision, 1);
        assert!(model.fire("WorkerTakes", &mut state));
        assert!(model.fire("WorkerCompletes", &mut state));
        lane.worker_drained();
        assert!(model.fire("DispatchLatestPending", &mut state));
        assert!(model.fire("RejectStale", &mut state));
        let latest = receiver.recv().unwrap();
        assert!(model.fire("WorkerTakes", &mut state));
        assert_eq!((latest.revision, latest.analysis_generation), (2, 2));
        assert_eq!(latest.source.as_ref(), "theme = \"Three\"\n");
        assert!(model.fire("WorkerCompletes", &mut state));
        assert!(model.fire("AcceptCurrent", &mut state));
        assert_eq!(state.get("published_revision"), Some(&3));
        assert!(receiver.try_recv().is_err());

        // Negative control: the model's non-replacing slot loses revision 3
        // under this same burst and must violate both latest-retention guards.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut lost = buggy.init_state();
        for action in ["RequestFirst", "RequestSecond", "RequestThird"] {
            assert!(buggy.fire(action, &mut lost));
        }
        assert!(!buggy.check_invariant("LatestRequestRemainsRepresented", &lost));
        assert!(!buggy.check_invariant("PendingSlotNamesLatest", &lost));
    }

    #[test]
    fn oversized_revision_drops_obsolete_pending_host_work() {
        let document = document();
        let (mut lane, receiver) = HostDiagnosticsLane::test_pair();
        lane.request(
            document,
            1,
            1,
            true,
            Arc::from("theme = \"One\"\n"),
            assets(),
        );
        lane.request(
            document,
            2,
            1,
            true,
            Arc::from("theme = \"Two\"\n"),
            assets(),
        );
        lane.request(
            document,
            3,
            1,
            true,
            Arc::<str>::from("x".repeat(MAX_CONFIG_ANALYSIS_BYTES + 1)),
            assets(),
        );

        assert_eq!(receiver.recv().unwrap().revision, 1);
        lane.worker_drained();
        assert!(receiver.try_recv().is_err());
    }
}
