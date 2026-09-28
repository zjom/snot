//! Language server for Simple Note Format.
//!
//! Synchronous, on `lsp-server`: one thread reads messages and answers them in
//! order. The whole workspace is indexed at start-up (10k notes take well
//! under a second) and open buffers overlay the notes on disk. Before
//! answering anything that looks across notes, the server checks the notes
//! root for files changed outside the editor, since not every client can
//! watch files for it.
//!
//! Besides the standard requests, the server answers two of its own:
//!
//! - `snot/backlinks` `{ "note": "<name>" }`: the links to a note from other
//!   notes, as `Location[]`.
//! - `snot/tags`: every key used as a flag (`@key`), sorted, as `string[]`.

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
    DidSaveTextDocument, Notification as _, PublishDiagnostics,
};
use lsp_types::request::{
    Completion, DocumentLinkRequest, DocumentSymbolRequest, Formatting, GotoDefinition,
    RangeFormatting, References, RegisterCapability, Request as _, WorkspaceSymbolRequest,
};
use lsp_types::{
    CompletionOptions, DidChangeWatchedFilesRegistrationOptions, DocumentLinkOptions,
    FileSystemWatcher, GlobPattern, InitializeParams, NumberOrString, OneOf, PositionEncodingKind,
    PublishDiagnosticsParams, Registration, RegistrationParams, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions, Url,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use snot_syntax::{Document, Encoding, LineIndex, Severity, Span, parse};
use snot_workspace::{Config, Workspace, find_root};

mod completion;
mod features;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Serve on stdin and stdout until the client says to exit.
pub fn stdio() -> Result<()> {
    let (connection, io) = Connection::stdio();
    run(connection)?;
    io.join()?;
    Ok(())
}

/// What a client can pass as `initializationOptions`, overriding the root's
/// `.snot.toml`.
#[derive(Default, Deserialize)]
#[serde(default)]
struct InitOptions {
    /// The notes root.
    root: Option<PathBuf>,
    /// The note extension.
    extension: Option<String>,
    /// The column `fmt` aligns metadata to.
    width: Option<usize>,
}

/// Serve on `connection` until the client says to exit.
pub fn run(connection: Connection) -> Result<()> {
    let (id, params) = connection.initialize_start()?;
    let params: InitializeParams = serde_json::from_value(params)?;
    let utf8 = params
        .capabilities
        .general
        .as_ref()
        .and_then(|g| g.position_encodings.as_ref())
        .is_some_and(|e| e.contains(&PositionEncodingKind::UTF8));
    let encoding = if utf8 {
        Encoding::Utf8
    } else {
        Encoding::Utf16
    };
    let can_watch = params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|w| w.did_change_watched_files.as_ref())
        .and_then(|w| w.dynamic_registration)
        .unwrap_or(false);

    let caps = ServerCapabilities {
        position_encoding: Some(if utf8 {
            PositionEncodingKind::UTF8
        } else {
            PositionEncodingKind::UTF16
        }),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                ..Default::default()
            },
        )),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        document_link_provider: Some(DocumentLinkOptions {
            resolve_provider: Some(false),
            work_done_progress_options: Default::default(),
        }),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(["[", "#", "@", ":"].map(String::from).to_vec()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let result = serde_json::json!({
        "capabilities": caps,
        "serverInfo": { "name": "snot", "version": env!("CARGO_PKG_VERSION") },
    });
    connection.initialize_finish(id, result)?;

    let ws = workspace(&params);
    if can_watch {
        let glob = format!("**/*{}", ws.config().extension);
        let register = RegistrationParams {
            registrations: vec![Registration {
                id: "snot-notes".into(),
                method: DidChangeWatchedFiles::METHOD.into(),
                register_options: Some(serde_json::to_value(
                    DidChangeWatchedFilesRegistrationOptions {
                        watchers: vec![FileSystemWatcher {
                            glob_pattern: GlobPattern::String(glob),
                            kind: None,
                        }],
                    },
                )?),
            }],
        };
        let req = Request::new(
            RequestId::from("snot/watch".to_owned()),
            RegisterCapability::METHOD.into(),
            register,
        );
        connection.sender.send(req.into())?;
    }

    let mut server = Server {
        sender: connection.sender.clone(),
        ws,
        encoding,
        docs: HashMap::new(),
    };
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                let response = server.request(req);
                connection.sender.send(response.into())?;
            }
            Message::Notification(n) => server.notification(n)?,
            Message::Response(_) => {}
        }
    }
    Ok(())
}

/// The workspace the client asked for: the root from the initialization
/// options, else the first workspace folder, else the root URI, else the
/// notes root around the working directory.
fn workspace(params: &InitializeParams) -> Workspace {
    let opts: InitOptions = params
        .initialization_options
        .clone()
        .and_then(|o| serde_json::from_value(o).ok())
        .unwrap_or_default();
    let folder = params
        .workspace_folders
        .as_ref()
        .and_then(|f| f.first())
        .and_then(|f| f.uri.to_file_path().ok());
    #[allow(deprecated)]
    let root_uri = params.root_uri.as_ref().and_then(|u| u.to_file_path().ok());
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = opts
        .root
        .or(folder)
        .or(root_uri)
        .unwrap_or_else(|| find_root(&cwd).unwrap_or(cwd));

    let mut config = Config::load(&root).unwrap_or_else(|e| {
        eprintln!("snot: {e}; using the default configuration");
        Config::default()
    });
    if let Some(mut extension) = opts.extension {
        if !extension.starts_with('.') {
            extension.insert(0, '.');
        }
        config.extension = extension;
    }
    if let Some(width) = opts.width {
        config.width = width;
    }
    match Workspace::load(root.clone(), config.clone()) {
        Ok((ws, errors)) => {
            for e in errors {
                eprintln!("snot: {e}");
            }
            ws
        }
        Err(e) => {
            eprintln!("snot: {e}");
            Workspace::new(root, config)
        }
    }
}

/// An open document.
struct Doc {
    text: String,
    doc: Document,
    index: LineIndex,
    /// The note's name, if the document is a note under the root.
    name: Option<String>,
    version: i32,
}

struct Server {
    sender: crossbeam_channel::Sender<Message>,
    ws: Workspace,
    encoding: Encoding,
    docs: HashMap<Url, Doc>,
}

impl Server {
    fn request(&mut self, req: Request) -> Response {
        let id = req.id.clone();
        let result = match req.method.as_str() {
            Formatting::METHOD => self.call(req, Self::formatting),
            RangeFormatting::METHOD => self.call(req, Self::range_formatting),
            GotoDefinition::METHOD => self.call(req, Self::definition),
            References::METHOD => self.call(req, Self::references),
            DocumentLinkRequest::METHOD => self.call(req, Self::document_links),
            DocumentSymbolRequest::METHOD => self.call(req, Self::document_symbols),
            WorkspaceSymbolRequest::METHOD => self.call(req, Self::workspace_symbols),
            Completion::METHOD => self.call(req, Self::completion),
            "snot/backlinks" => self.call(req, Self::backlinks),
            "snot/tags" => self.call(req, |s, (): ()| s.tags()),
            _ => {
                let message = format!("unknown request {}", req.method);
                return Response::new_err(id, ErrorCode::MethodNotFound as i32, message);
            }
        };
        match result {
            Ok(value) => Response::new_ok(id, value),
            Err(e) => Response::new_err(id, ErrorCode::InvalidParams as i32, e),
        }
    }

    fn call<P: DeserializeOwned, R: serde::Serialize>(
        &mut self,
        req: Request,
        f: impl FnOnce(&mut Self, P) -> R,
    ) -> std::result::Result<serde_json::Value, String> {
        // `null` params stand for none, as for `snot/tags`.
        let params = serde_json::from_value(req.params).map_err(|e| e.to_string())?;
        serde_json::to_value(f(self, params)).map_err(|e| e.to_string())
    }

    fn notification(&mut self, n: Notification) -> Result<()> {
        match n.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let params: lsp_types::DidOpenTextDocumentParams =
                    serde_json::from_value(n.params)?;
                let doc = params.text_document;
                self.update(doc.uri, doc.text, doc.version)?;
            }
            DidChangeTextDocument::METHOD => {
                let params: lsp_types::DidChangeTextDocumentParams =
                    serde_json::from_value(n.params)?;
                // Full sync: the last change is the whole text.
                if let Some(change) = params.content_changes.into_iter().last() {
                    let doc = params.text_document;
                    self.update(doc.uri, change.text, doc.version)?;
                }
            }
            DidSaveTextDocument::METHOD | DidChangeWatchedFiles::METHOD => {
                self.refresh();
                self.publish_all()?;
            }
            DidCloseTextDocument::METHOD => {
                let params: lsp_types::DidCloseTextDocumentParams =
                    serde_json::from_value(n.params)?;
                let uri = params.text_document.uri;
                if let Some(Doc {
                    name: Some(name), ..
                }) = self.docs.remove(&uri)
                    && let Err(e) = self.ws.reload(&name)
                {
                    eprintln!("snot: {e}");
                }
                self.send_diagnostics(uri, Vec::new(), None)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Take the new text of an open document, and report its problems.
    fn update(&mut self, uri: Url, text: String, version: i32) -> Result<()> {
        let name = uri.to_file_path().ok().and_then(|p| self.ws.name_of(&p));
        if let Some(name) = &name {
            let path = uri.to_file_path().expect("notes are files");
            self.ws.insert(name.clone(), path, text.clone());
        }
        let doc = Doc {
            doc: parse(&text),
            index: LineIndex::new(&text),
            text,
            name,
            version,
        };
        self.docs.insert(uri.clone(), doc);
        self.publish(&uri)
    }

    /// Read notes changed on disk since last time.
    fn refresh(&mut self) {
        match self.ws.refresh() {
            Ok((_, errors)) => errors.iter().for_each(|e| eprintln!("snot: {e}")),
            Err(e) => eprintln!("snot: {e}"),
        }
    }

    fn publish_all(&self) -> Result<()> {
        for uri in self.docs.keys() {
            self.publish(uri)?;
        }
        Ok(())
    }

    fn publish(&self, uri: &Url) -> Result<()> {
        let Some(doc) = self.docs.get(uri) else {
            return Ok(());
        };
        let name = doc.name.as_deref().unwrap_or("");
        let diagnostics = snot_check::check(&self.ws, name, &doc.doc)
            .into_iter()
            .map(|d| lsp_types::Diagnostic {
                range: self.range(&doc.text, &doc.index, &d.span),
                severity: Some(match d.severity() {
                    Severity::Error => lsp_types::DiagnosticSeverity::ERROR,
                    Severity::Warning => lsp_types::DiagnosticSeverity::WARNING,
                }),
                code: Some(NumberOrString::String(d.code.as_str().to_owned())),
                source: Some("snot".to_owned()),
                message: d.message,
                ..Default::default()
            })
            .collect();
        self.send_diagnostics(uri.clone(), diagnostics, Some(doc.version))
    }

    fn send_diagnostics(
        &self,
        uri: Url,
        diagnostics: Vec<lsp_types::Diagnostic>,
        version: Option<i32>,
    ) -> Result<()> {
        let params = PublishDiagnosticsParams {
            uri,
            diagnostics,
            version,
        };
        let n = Notification::new(PublishDiagnostics::METHOD.into(), params);
        self.sender.send(n.into())?;
        Ok(())
    }

    fn position(&self, text: &str, index: &LineIndex, offset: usize) -> lsp_types::Position {
        let p = index.position(text, offset, self.encoding);
        lsp_types::Position::new(p.line, p.column)
    }

    fn range(&self, text: &str, index: &LineIndex, span: &Span) -> lsp_types::Range {
        lsp_types::Range::new(
            self.position(text, index, span.start),
            self.position(text, index, span.end),
        )
    }

    fn offset(&self, doc: &Doc, pos: lsp_types::Position) -> usize {
        let pos = snot_syntax::Position {
            line: pos.line,
            column: pos.character,
        };
        doc.index.offset(&doc.text, pos, self.encoding)
    }
}
