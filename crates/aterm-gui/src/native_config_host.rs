// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded off-thread config analysis for Settings ▸ Manual.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

use aterm_window::event_loop::EventLoopProxy;

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
pub(crate) struct HostDiagnosticsLane {
    request_tx: Option<SyncSender<Request>>,
    proxy: Option<EventLoopProxy<Wake>>,
    pending: Option<Request>,
}

impl HostDiagnosticsLane {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "callers keep the fallible startup signature while the worker spawns on first demand"
    )]
    pub(crate) fn spawn(proxy: EventLoopProxy<Wake>) -> Result<Self, String> {
        Ok(Self {
            request_tx: None,
            proxy: Some(proxy),
            pending: None,
        })
    }

    fn ensure_worker(&mut self) -> Option<&SyncSender<Request>> {
        if self.request_tx.is_none() {
            let proxy = self.proxy.take()?;
            let (request_tx, request_rx) = sync_channel(REQUEST_CAPACITY);
            match crate::qos::spawn_lane_thread(
                "aterm-config-diagnostics",
                crate::qos::Role::Background,
                move || {
                    worker_loop(request_rx, proxy);
                },
            ) {
                Ok(_) => self.request_tx = Some(request_tx),
                Err(error) => {
                    crate::logging::stderr_line!(
                        "aterm-gui: could not start Manual diagnostics worker: {error}"
                    );
                }
            }
        }
        self.request_tx.as_ref()
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
        let Some(request_tx) = self.ensure_worker() else {
            return;
        };
        match request_tx.try_send(request) {
            Ok(()) => {}
            Err(TrySendError::Full(request)) => self.pending = Some(request),
            Err(TrySendError::Disconnected(_)) => {
                self.pending = None;
            }
        }
    }

    #[cfg(test)]
    fn test_pair() -> (Self, Receiver<Request>) {
        let (request_tx, request_rx) = sync_channel(REQUEST_CAPACITY);
        (
            Self {
                request_tx: Some(request_tx),
                proxy: None,
                pending: None,
            },
            request_rx,
        )
    }
}

fn worker_loop(request_rx: Receiver<Request>, proxy: EventLoopProxy<Wake>) {
    while let Ok(mut request) = request_rx.recv() {
        while let Ok(newer) = request_rx.try_recv() {
            request = newer;
        }
        let analysis = std::panic::catch_unwind(|| {
            let mut analysis = crate::native_config_language::analyze(&request.source);
            analysis.adopt_theme_names(&request.assets.themes);
            if !analysis.has_errors() {
                let diagnostics = crate::native_config_language::analyze_host_with_assets(
                    &request.source,
                    request.backend_gpu,
                    &request.assets,
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

        let mut analysis = if self.proxy.is_none() {
            let mut analysis = crate::native_config_language::analyze(&snapshot.text);
            analysis.adopt_theme_names(&config_assets.themes);
            analysis
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

        let mut views = Vec::new();
        for (&window, state) in &self.windows {
            for tab in state.tab_set.tabs() {
                tab.root.visit(&mut |&view| {
                    let Some(crate::tab_model::View::Native(native)) =
                        self.view_store.get(view).copied()
                    else {
                        return;
                    };
                    if self.native_runtime.document_id(native.instance) == Some(completion.document)
                    {
                        views.push((window, native.instance, view));
                    }
                });
            }
        }
        for (window, instance, view) in views {
            self.invalidate_native_view_cache(window, view, crate::native_app::DamageRegion::All);
            self.refresh_native_presentation(window, instance, view);
        }
        self.request_redraw_all_windows();
        true
    }
}

#[cfg(test)]
#[path = "native_config_host_tests.rs"]
mod tests;
