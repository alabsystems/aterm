// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use amacs::protocol::doc::{DiskState, DocRef, DocRequest};
use amacs::protocol::read::{DOC_READ_CAP, LineRange, format_lines};
use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;
use crate::app_doc::{DiskProbe, DocAnswer, DocCall, DocFacts, DocNames, DocQuery, DocText};
use crate::control::control_media::call_main;

/// `doc <rest>`, asked of the main thread through `proxy`.
pub(crate) fn cmd_doc(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    serve_doc(rest, &mut |query| ask_main(proxy, query))
}

/// `doc read`'s refusal of a private edit's document
/// ([`crate::native_document_journal::private_uri`]), before any of its text.
const REFUSED_PRIVATE: &str = "ERR refused private doc\n";

/// `doc <rest>`'s reply, the main thread asked through `ask`.
pub(crate) fn serve_doc(
    rest: &str,
    ask: &mut dyn FnMut(DocQuery) -> Result<DocAnswer, String>,
) -> String {
    let request = match DocRequest::parse(rest) {
        Ok(request) => request,
        Err(error) => return format!("ERR {error}\n"),
    };
    match request {
        DocRequest::Ls { uri } => {
            let names = match uri.as_deref().map(spellings).transpose() {
                Ok(names) => names,
                Err(error) => return format!("ERR {error}\n"),
            };
            match ask(DocQuery::Ls(names)) {
                Ok(DocAnswer::Listed(documents)) => ls_reply(documents),
                Ok(other) => format!("ERR doc ls answered {other:?}\n"),
                Err(error) => format!("ERR {error}\n"),
            }
        }
        DocRequest::Read { doc, lines } => {
            let names = match &doc {
                DocRef::Id(id) => DocNames::Id(*id),
                DocRef::Uri(uri) => match spellings(uri) {
                    Ok(names) => DocNames::Uri(names),
                    Err(error) => return format!("ERR {error}\n"),
                },
            };
            if let DocNames::Uri(names) = &names
                && names
                    .iter()
                    .any(|name| crate::native_document_journal::private_uri(name))
            {
                return REFUSED_PRIVATE.to_string();
            }
            match ask(DocQuery::Read(names)) {
                Ok(DocAnswer::Text(text)) => read_reply(&text, lines),
                Ok(DocAnswer::Refused) => REFUSED_PRIVATE.to_string(),
                Ok(DocAnswer::Unknown) => format!("ERR unknown doc {doc}\n"),
                Ok(other) => format!("ERR doc read answered {other:?}\n"),
                Err(error) => format!("ERR {error}\n"),
            }
        }
        DocRequest::Later(form) => {
            format!("ERR unsupported doc {}: not served yet\n", form.as_str())
        }
    }
}

fn ask_main(proxy: &EventLoopProxy<Wake>, query: DocQuery) -> Result<DocAnswer, String> {
    let (answer, answered) = std::sync::mpsc::channel();
    let request = crate::app_control::InspectRequest::Doc(DocCall { query, answer });
    match call_main(proxy, |reply| Wake::InspectApp { request, reply }) {
        Ok(Ok(_)) => answered
            .try_recv()
            .map_err(|_| "doc: the main thread sent no answer".to_string()),
        Ok(Err(error)) => Err(error),
        Err(error) => Err(error.to_string()),
    }
}

fn spellings(uri: &str) -> Result<Vec<String>, String> {
    let path = amacs::protocol::uri::file_uri_to_path(uri)
        .map_err(|error| format!("bad uri={uri} ({error})"))?;
    let mut names = vec![uri.to_string()];
    let spelled = amacs::protocol::uri::path_to_file_uri(&path).ok();
    let real = real_spelling(&path);
    for name in [spelled, real].into_iter().flatten() {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(names)
}

fn real_spelling(path: &std::path::Path) -> Option<String> {
    let mut missing = Vec::new();
    let mut cursor = path;
    let mut real = loop {
        match std::fs::canonicalize(cursor) {
            Ok(real) => break real,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(cursor.file_name()?);
                cursor = cursor.parent()?;
            }
            Err(_) => return None,
        }
    };
    real.extend(missing.iter().rev());
    amacs::protocol::uri::path_to_file_uri(&real).ok()
}

fn ls_reply(documents: Vec<DocFacts>) -> String {
    let rows: Vec<String> = documents
        .into_iter()
        .filter_map(|mut facts| {
            facts.place();
            let DocFacts { mut row, disk, .. } = facts;
            row.disk = disk_state(disk.as_ref());
            row.format()
                .inspect_err(|error| aterm_log::warn!("doc ls: doc {} left out: {error}", row.doc))
                .ok()
        })
        .collect();
    let mut out = format!("OK {}\n", rows.len());
    for row in rows {
        out.push_str(&row);
        out.push('\n');
    }
    out
}

fn disk_state(probe: Option<&DiskProbe>) -> DiskState {
    let Some(DiskProbe { path, baseline }) = probe else {
        return DiskState::Unknown;
    };
    let limit = usize::try_from(baseline.len)
        .unwrap_or(crate::native_document_host::DEFAULT_DOCUMENT_LIMIT)
        .min(crate::native_document_host::DEFAULT_DOCUMENT_LIMIT);
    match crate::native_document_host::observe_file(path, limit) {
        Err(_) => DiskState::Unknown,
        Ok(now) => DiskState::by_content(
            now.exists,
            baseline.exists,
            now.len == baseline.len,
            now.content == baseline.content,
        ),
    }
}

fn read_reply(text: &DocText, lines: LineRange) -> String {
    let body = format_lines(&text.text, text.seq, lines, DOC_READ_CAP);
    format!("OK {} doc={}\n{body}", body.len(), text.doc)
}

#[cfg(test)]
#[path = "control_doc_tests.rs"]
mod tests;
