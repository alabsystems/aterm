// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File admission owned by one resumable restore leaf, before any tab is installed.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::{AdmittedDocument, App, AppKind, Placement, WindowId};

#[cfg(test)]
std::thread_local! {
    static HEADLESS_ADMISSION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static READ_GATE: std::cell::RefCell<Option<Box<dyn FnOnce() + Send>>> = const { std::cell::RefCell::new(None) };
}

/// Exercise the production reader/preparation pipeline in a headless cursor test.
#[cfg(test)]
pub(crate) fn with_headless_restore_admission_for_test<R>(body: impl FnOnce() -> R) -> R {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            HEADLESS_ADMISSION.set(self.0);
        }
    }
    let _reset = Reset(HEADLESS_ADMISSION.replace(true));
    body()
}

/// Hold the next actual restore reader without changing its admission or publication path.
#[cfg(all(test, unix))]
pub(crate) fn with_restore_read_gate_for_test<R>(
    gate: impl FnOnce() + Send + 'static,
    body: impl FnOnce() -> R,
) -> R {
    struct Reset(Option<Box<dyn FnOnce() + Send>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            READ_GATE.set(self.0.take());
        }
    }
    let _reset = Reset(READ_GATE.replace(Some(Box::new(gate))));
    body()
}

/// A completed file read with no published document or view.
#[derive(Debug)]
pub(crate) struct RestoreAdmitted {
    window: WindowId,
    kind: AppKind,
    uri: String,
    config_editor: bool,
    document: Box<AdmittedDocument>,
    prepared: Option<super::PreparedDocument>,
}

/// One leaf's admission. Dropping it abandons the result; it never owns a tab.
#[derive(Debug)]
pub(crate) struct RestoreDocumentAdmission {
    receiver: Receiver<Result<RestoreAdmitted, String>>,
    canceled: Arc<AtomicBool>,
    finished: bool,
    preparation: Option<super::preparation::PreparationLease>,
    admitted: Option<RestoreAdmitted>,
}

impl RestoreDocumentAdmission {
    /// Take a completed read once. A still-running read returns `None`.
    pub(crate) fn poll(&mut self) -> Option<Result<RestoreAdmitted, String>> {
        if self.finished {
            return None;
        }
        let result = match self.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err("document restore admission ended without a result".to_string())
            }
        };
        self.finished = true;
        Some(result)
    }

    fn spawn(
        read: impl FnOnce(&AtomicBool) -> Result<RestoreAdmitted, String> + Send + 'static,
        wake: impl FnOnce() + Send + 'static,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel();
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&canceled);
        std::thread::Builder::new()
            .name("aterm-restore-document-admit".to_string())
            .spawn(move || {
                if worker_cancel.load(Ordering::Acquire) {
                    return;
                }
                if let Err(errno) = aterm_diskwalk::set_thread_materialize_policy(
                    aterm_diskwalk::MaterializePolicy::On,
                ) {
                    crate::logging::stderr_line!(
                        "aterm-gui: restore document admission worker could not enable \
                         dataless-file materialization (errno {errno}); an evicted \
                         file will be reported as not downloaded"
                    );
                }
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(&worker_cancel)))
                        .unwrap_or_else(|_| {
                            Err("document restore admission worker panicked".to_string())
                        });
                if !worker_cancel.load(Ordering::Acquire) && sender.send(result).is_ok() {
                    // The cursor can poll the result as soon as it observes this wake.
                    wake();
                }
            })
            .map_err(|error| format!("could not start document restore admission: {error}"))?;
        Ok(Self {
            receiver,
            canceled,
            finished: false,
            preparation: None,
            admitted: None,
        })
    }
}

impl Drop for RestoreDocumentAdmission {
    fn drop(&mut self) {
        self.canceled.store(true, Ordering::Release);
    }
}

#[derive(Debug)]
struct Request {
    window: WindowId,
    kind: AppKind,
    uri: String,
    config_path: Option<PathBuf>,
}

impl Request {
    fn read(self, canceled: &AtomicBool) -> Result<RestoreAdmitted, String> {
        let check = || {
            if canceled.load(Ordering::Acquire) {
                Err("document restore admission was canceled".to_string())
            } else {
                Ok(())
            }
        };
        check()?;
        let config_editor = self.config_path.is_some();
        let uri = if let Some(path) = self.config_path {
            // Ensuring a missing config includes disk reads and an atomic create. All of it
            // belongs to the admission worker, just like opening an existing document.
            let path = super::ensure_config_file(&path)?;
            crate::native_document_host::path_to_file_uri(&path)
                .map_err(|error| format!("could not address aterm.toml: {error}"))?
        } else {
            self.uri
        };
        check()?;
        #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
        crate::restore_measurement::wait_for_detached_read(&uri);
        let document = App::admit_document_for_worker(&uri, config_editor).map_err(String::from)?;
        check()?;
        Ok(RestoreAdmitted {
            window: self.window,
            kind: self.kind,
            uri,
            config_editor,
            document: Box::new(document),
            prepared: None,
        })
    }
}

impl App {
    /// Poll file admission and journal preparation without blocking the event loop.
    pub(crate) fn poll_restore_document_admission(
        &mut self,
        ticket: &mut RestoreDocumentAdmission,
    ) -> Option<Result<RestoreAdmitted, String>> {
        self.advance_document_preparations(false);
        if let Some(preparation) = &ticket.preparation {
            let result = preparation.poll()?;
            ticket.preparation = None;
            let mut admitted = ticket
                .admitted
                .take()
                .expect("a preparing restore retains its admitted file");
            return Some(
                result
                    .map(|prepared| {
                        admitted.prepared = Some(prepared);
                        admitted
                    })
                    .map_err(String::from),
            );
        }
        match ticket.poll()? {
            Err(error) => Some(Err(error)),
            Ok(admitted) => {
                ticket.preparation = Some(
                    self.prepare_restore_document_open((*admitted.document).clone(), &admitted.uri),
                );
                ticket.admitted = Some(admitted);
                None
            }
        }
    }

    /// Begin one cold-restore file read for the cursor-owned window ID. The window need not
    /// be published yet; this neither installs a tab nor reads the file on the caller.
    pub(crate) fn begin_restore_document_admission(
        &self,
        wid: WindowId,
        descriptor: &crate::restore::NativeLeafRestore,
    ) -> Result<RestoreDocumentAdmission, String> {
        let kind = match descriptor.restore_tag.as_str() {
            "markdown" => AppKind::Markdown,
            "editor" => AppKind::Editor,
            _ => return Err("only document restore leaves admit a file".to_string()),
        };
        Self::validate_document_request(kind, descriptor.config_editor)?;
        let uri = descriptor
            .uri
            .clone()
            .ok_or_else(|| "document URI is missing".to_string())?;
        let proxy = self.proxy.clone();
        if proxy.is_none() {
            #[cfg(test)]
            let headless_test = self.headless && HEADLESS_ADMISSION.get();
            #[cfg(not(test))]
            let headless_test = false;
            if !headless_test {
                return Err("document restore admission has no event-loop wake".into());
            }
        }
        let config_path = if descriptor.config_editor {
            Some(crate::app_config::config_path().ok_or_else(|| {
                "could not resolve the config path (HOME/XDG_CONFIG_HOME unset)".to_string()
            })?)
        } else {
            None
        };
        let request = Request {
            window: wid,
            kind,
            uri,
            config_path,
        };
        #[cfg(test)]
        let read_gate = READ_GATE.take();
        RestoreDocumentAdmission::spawn(
            move |canceled| {
                #[cfg(test)]
                if let Some(gate) = read_gate {
                    gate();
                }
                request.read(canceled)
            },
            move || {
                if let Some(proxy) = proxy {
                    let _ = proxy.send_event(crate::Wake::RestoreDocumentReady);
                }
            },
        )
    }

    /// Install the admitted leaf in a fresh staging tab. The restore cursor owns its detach.
    pub(crate) fn install_restore_admitted_document(
        &mut self,
        wid: WindowId,
        admitted: RestoreAdmitted,
    ) -> Result<(), String> {
        let RestoreAdmitted {
            window,
            kind,
            uri,
            config_editor,
            document,
            prepared,
        } = admitted;
        if wid != window {
            return Err("document admission belongs to another restore window".to_string());
        }
        let target = crate::app_tabs::NativeInstallTarget::DetachedRestore(wid);
        let installed = if let Some(prepared) = prepared {
            self.install_prepared_document(target, kind, &uri, prepared, Placement::NewTab)?
        } else {
            #[cfg(test)]
            {
                self.install_admitted_document(target, kind, &uri, *document, Placement::NewTab)?
            }
            #[cfg(not(test))]
            {
                let _ = document;
                return Err("document restore has not completed journal preparation".into());
            }
        };
        if config_editor
            && let Err(error) = self.finish_config_editor_open(target, installed.document, None)
        {
            self.rollback_document_tab(target, installed.tab);
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "app_restore_document_tests.rs"]
mod tests;
