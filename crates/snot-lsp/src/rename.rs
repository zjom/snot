//! Rename: a note, moving its file and rewriting the links to it, or an
//! `@id`, rewriting the links anchored on it.
//!
//! Where the cursor is decides what is renamed: on a link, the note it points
//! at, or its anchor when the cursor is past the `#`; on an `@id`, that id;
//! anywhere else in a note, the note.

use std::collections::{BTreeMap, HashMap};

use lsp_types::{
    DocumentChangeOperation, DocumentChanges, OneOf, OptionalVersionedTextDocumentIdentifier,
    PrepareRenameResponse, RenameFile, RenameParams, ResourceOp, TextDocumentEdit,
    TextDocumentPositionParams, TextEdit, Url, WorkspaceEdit,
};
use snot_syntax::{Document, LineIndex, Link, Span, Target, TokenForm, is_key};

use crate::Server;
use crate::features::contains;

/// A document edits go to.
#[derive(Clone)]
enum Place {
    /// The note with this name.
    Note(String),
    /// The open document at this URI, which isn't a note.
    Open(Url),
}

/// What a rename renames.
enum Renamed {
    /// The note with this name.
    Note(String),
    /// The `@id` with this value, in a document.
    Id(Place, String),
}

impl Server {
    pub(crate) fn prepare_rename(
        &mut self,
        p: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>, String> {
        self.refresh();
        let doc = self
            .docs
            .get(&p.text_document.uri)
            .ok_or("document not open")?;
        let offset = self.offset(doc, p.position);
        let (renamed, span) = self.renamed(&p.text_document.uri, offset)?;
        let placeholder = match renamed {
            Renamed::Note(name) => name,
            Renamed::Id(_, id) => id,
        };
        Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: self.range(&doc.text, &doc.index, &span),
            placeholder,
        }))
    }

    pub(crate) fn rename(&mut self, p: RenameParams) -> Result<Option<WorkspaceEdit>, String> {
        self.refresh();
        let pos = p.text_document_position;
        let doc = self
            .docs
            .get(&pos.text_document.uri)
            .ok_or("document not open")?;
        let offset = self.offset(doc, pos.position);
        match self.renamed(&pos.text_document.uri, offset)?.0 {
            Renamed::Note(old) => self.rename_note(&old, p.new_name.trim()),
            Renamed::Id(place, old) => self.rename_id(&place, &old, p.new_name.trim()),
        }
    }

    /// What renaming at `offset` in the open document `uri` renames, and the
    /// span there naming it.
    fn renamed(&self, uri: &Url, offset: usize) -> Result<(Renamed, Span), String> {
        let doc = self.docs.get(uri).ok_or("document not open")?;
        let here = || match &doc.name {
            Some(name) => Place::Note(name.clone()),
            None => Place::Open(uri.clone()),
        };
        if let Some(link) = doc.doc.links.iter().find(|l| contains(&l.span, offset)) {
            let (path_span, anchor_span) = parts(&doc.text, link);
            let (place, id, span) = match (&link.target, anchor_span) {
                (Target::Anchor(a), Some(span)) => (here(), a, span),
                // On the anchor, `#` included.
                (
                    Target::Note {
                        path,
                        anchor: Some(a),
                    },
                    Some(span),
                ) if offset >= span.start - 1 => (Place::Note(path.clone()), a, span),
                (Target::Note { path, .. }, _) => {
                    if self.ws.note(path).is_none() {
                        return Err(format!("there is no note `{path}`"));
                    }
                    return Ok((Renamed::Note(path.clone()), path_span));
                }
                _ => return Err("only links to notes and anchors can be renamed".to_owned()),
            };
            let (_, d) = self
                .place_doc(&place)
                .ok_or("the linked note isn't there")?;
            if !d.scopes.iter().any(|s| s.id.as_ref() == Some(id)) {
                return Err(format!(
                    "`#{id}` isn't an `@id`; edit the heading to change its anchor"
                ));
            }
            return Ok((Renamed::Id(place, id.clone()), span));
        }
        if let Some(t) = doc.doc.tokens.iter().find(|t| contains(&t.span, offset)) {
            return match (t.key.as_str(), t.values.as_slice(), &t.value_span) {
                ("id", [id], Some(span)) => Ok((Renamed::Id(here(), id.clone()), span.clone())),
                _ => Err("only notes and `@id`s can be renamed".to_owned()),
            };
        }
        match &doc.name {
            Some(name) => Ok((Renamed::Note(name.clone()), offset..offset)),
            None => Err("this document isn't a note under the notes root".to_owned()),
        }
    }

    fn place_doc(&self, place: &Place) -> Option<(&str, &Document)> {
        match place {
            Place::Note(name) => self.ws.note(name).map(|n| (n.source.as_str(), &n.doc)),
            Place::Open(uri) => self.docs.get(uri).map(|d| (d.text.as_str(), &d.doc)),
        }
    }

    /// Open documents that aren't notes, which the workspace doesn't index.
    fn loose_docs(&self) -> impl Iterator<Item = (Place, &str, &Document)> {
        self.docs
            .iter()
            .filter(|(_, d)| d.name.is_none())
            .map(|(uri, d)| (Place::Open(uri.clone()), d.text.as_str(), &d.doc))
    }

    fn rename_note(&self, old: &str, new: &str) -> Result<Option<WorkspaceEdit>, String> {
        if !self.can_rename_files {
            return Err("the editor can't rename files through the language server".to_owned());
        }
        let new = new.strip_suffix(&self.ws.config().extension).unwrap_or(new);
        if let Some(problem) = name_problem(new) {
            return Err(format!("can't rename to `{new}`: {problem}"));
        }
        if new == old {
            return Ok(None);
        }
        let new_path = self.ws.path_of(new);
        if self.ws.note(new).is_some() || new_path.exists() {
            return Err(format!("`{new}` already exists"));
        }
        let old_path = &self.ws.note(old).ok_or("the note isn't there")?.path;

        let mut edits = Edits::new(self);
        for r in self.ws.backlinks(old) {
            if let Some(note) = self.ws.note(&r.note) {
                let (path, _) = parts(&note.source, &note.doc.links[r.link]);
                edits.push(&Place::Note(r.note.clone()), path, new);
            }
        }
        for (place, src, d) in self.loose_docs() {
            for link in &d.links {
                if matches!(&link.target, Target::Note { path, .. } if path == old) {
                    edits.push(&place, parts(src, link).0, new);
                }
            }
        }

        // The links are rewritten before the file moves, so edits to links
        // in the note itself still find it.
        let mut ops = edits.document_edits();
        let uri = |p: &std::path::Path| Url::from_file_path(p).map_err(|()| "not a file path");
        ops.push(DocumentChangeOperation::Op(ResourceOp::Rename(
            RenameFile {
                old_uri: uri(old_path)?,
                new_uri: uri(&new_path)?,
                options: None,
                annotation_id: None,
            },
        )));
        Ok(Some(WorkspaceEdit {
            document_changes: Some(DocumentChanges::Operations(ops)),
            ..Default::default()
        }))
    }

    fn rename_id(
        &self,
        place: &Place,
        old: &str,
        new: &str,
    ) -> Result<Option<WorkspaceEdit>, String> {
        let new = new.strip_prefix('#').unwrap_or(new);
        if !is_key(new) {
            return Err(format!(
                "can't rename to `{new}`: an `@id` is a letter, then letters, digits, `-` or `_`"
            ));
        }
        if new == old {
            return Ok(None);
        }
        let (src, d) = self.place_doc(place).ok_or("the note isn't there")?;
        if d.scopes.iter().any(|s| s.id.as_deref() == Some(new)) {
            return Err(format!("`@id:{new}` is already used in this note"));
        }

        let mut edits = Edits::new(self);
        for t in &d.tokens {
            if t.key == "id"
                && t.values == [old]
                && let Some(span) = &t.value_span
            {
                let text = match t.form {
                    TokenForm::List => format!("[{new}]"),
                    TokenForm::Scalar | TokenForm::Flag => new.to_owned(),
                };
                edits.push(place, span.clone(), &text);
            }
        }
        for link in &d.links {
            if matches!(&link.target, Target::Anchor(a) if a == old)
                && let (_, Some(span)) = parts(src, link)
            {
                edits.push(place, span, new);
            }
        }
        // Links from notes, and from open documents that aren't, to this note.
        if let Place::Note(name) = place {
            let anchored = |link: &Link| {
                matches!(&link.target,
                    Target::Note { path, anchor: Some(a) } if path == name && a == old)
            };
            for r in self.ws.backlinks(name) {
                if let Some(note) = self.ws.note(&r.note)
                    && let link = &note.doc.links[r.link]
                    && anchored(link)
                    && let (_, Some(span)) = parts(&note.source, link)
                {
                    edits.push(&Place::Note(r.note.clone()), span, new);
                }
            }
            for (from, src, d) in self.loose_docs() {
                for link in d.links.iter().filter(|l| anchored(l)) {
                    if let (_, Some(span)) = parts(src, link) {
                        edits.push(&from, span, new);
                    }
                }
            }
        }
        Ok(Some(WorkspaceEdit {
            document_changes: Some(DocumentChanges::Operations(edits.document_edits())),
            ..Default::default()
        }))
    }
}

/// The spans of a link's path and anchor, trimmed and without the `#`. An
/// anchor in this file has an empty path.
fn parts(src: &str, link: &Link) -> (Span, Option<Span>) {
    let t = &link.target_span;
    let raw = &src[t.clone()];
    let start = t.start + raw.len() - raw.trim_start().len();
    let end = t.start + raw.trim_end().len();
    match src[start..end].find('#') {
        Some(hash) => (start..start + hash, Some(start + hash + 1..end)),
        None => (start..end, None),
    }
}

/// Why `name` can't name a note, if it can't (NOTE_SPEC 7.2). Names needing
/// escapes in a link are refused too.
fn name_problem(name: &str) -> Option<&'static str> {
    let last = name.rsplit('/').next().unwrap_or(name);
    if name.is_empty() {
        Some("the name is empty")
    } else if name.starts_with('/') {
        Some("note names are relative to the notes root")
    } else if name.split('/').any(|s| s.is_empty() || s == "..") {
        Some("empty path segment or `..`")
    } else if name.contains(['#', '|', '[', ']', '\\']) || name.contains(char::is_control) {
        Some("links can't spell `#`, `|`, `[`, `]` or `\\` in a note name")
    } else if name.contains("://") {
        Some("links would read it as a URL")
    } else if last.char_indices().any(|(i, c)| c == '.' && i > 0) {
        Some("links would read a name with an extension as a file")
    } else {
        None
    }
}

/// Text edits across documents.
struct Edits<'a> {
    server: &'a Server,
    indexes: HashMap<String, LineIndex>,
    edits: BTreeMap<Url, Vec<TextEdit>>,
}

impl<'a> Edits<'a> {
    fn new(server: &'a Server) -> Self {
        Edits {
            server,
            indexes: HashMap::new(),
            edits: BTreeMap::new(),
        }
    }

    fn push(&mut self, place: &Place, span: Span, new_text: &str) {
        let s = self.server;
        let (uri, range) = match place {
            Place::Note(name) => {
                let Some(note) = s.ws.note(name) else { return };
                let Ok(uri) = Url::from_file_path(&note.path) else {
                    return;
                };
                let index = self
                    .indexes
                    .entry(name.clone())
                    .or_insert_with(|| LineIndex::new(&note.source));
                (uri, s.range(&note.source, index, &span))
            }
            Place::Open(uri) => {
                let doc = &s.docs[uri];
                (uri.clone(), s.range(&doc.text, &doc.index, &span))
            }
        };
        self.edits.entry(uri).or_default().push(TextEdit {
            range,
            new_text: new_text.to_owned(),
        });
    }

    /// The edits by document, with the version of each open one.
    fn document_edits(self) -> Vec<DocumentChangeOperation> {
        self.edits
            .into_iter()
            .map(|(uri, edits)| {
                let version = self.server.docs.get(&uri).map(|d| d.version);
                DocumentChangeOperation::Edit(TextDocumentEdit {
                    text_document: OptionalVersionedTextDocumentIdentifier { uri, version },
                    edits: edits.into_iter().map(OneOf::Left).collect(),
                })
            })
            .collect()
    }
}
