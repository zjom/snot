//! Answers to requests.

use std::collections::{BTreeSet, HashMap};

use lsp_types::{
    DocumentFormattingParams, DocumentLink, DocumentLinkParams, DocumentRangeFormattingParams,
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, FormattingOptions,
    FormattingProperty, GotoDefinitionParams, GotoDefinitionResponse, Location, ReferenceParams,
    SymbolInformation, SymbolKind, TextEdit, Url, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use serde::Deserialize;
use snot_fmt::FmtOptions;
use snot_syntax::{Document, LineIndex, LineKind, ScopeId, ScopeKind, Span, Target};

use crate::{Doc, Server};

/// What `references` finds the uses of.
enum Subject {
    /// The links to a note.
    Note(String),
    /// The links to a scope: in the note with this name, or in the open
    /// document when it isn't a note.
    Scope(Option<String>, ScopeId),
    /// The tokens giving a key.
    Key(String),
}

#[derive(Deserialize)]
pub(crate) struct BacklinksParams {
    note: String,
}

impl Server {
    pub(crate) fn formatting(&mut self, p: DocumentFormattingParams) -> Option<Vec<TextEdit>> {
        self.format(&p.text_document.uri, &p.options, None)
    }

    pub(crate) fn range_formatting(
        &mut self,
        p: DocumentRangeFormattingParams,
    ) -> Option<Vec<TextEdit>> {
        let lines = p.range.start.line..=p.range.end.line;
        self.format(&p.text_document.uri, &p.options, Some(lines))
    }

    /// The edits formatting a document, or only those starting on `lines`.
    /// The width is the `width` formatting option, else the configured one.
    fn format(
        &self,
        uri: &Url,
        options: &FormattingOptions,
        lines: Option<std::ops::RangeInclusive<u32>>,
    ) -> Option<Vec<TextEdit>> {
        let doc = self.docs.get(uri)?;
        let width = match options.properties.get("width") {
            Some(FormattingProperty::Number(n)) if *n > 0 => *n as usize,
            _ => self.ws.config().width,
        };
        let edits = snot_fmt::fmt(&doc.text, &doc.doc, &FmtOptions { width });
        Some(
            edits
                .into_iter()
                .map(|e| TextEdit {
                    range: self.range(&doc.text, &doc.index, &e.range),
                    new_text: e.new_text,
                })
                .filter(|e| {
                    lines
                        .as_ref()
                        .is_none_or(|l| l.contains(&e.range.start.line))
                })
                .collect(),
        )
    }

    pub(crate) fn definition(&mut self, p: GotoDefinitionParams) -> Option<GotoDefinitionResponse> {
        self.refresh();
        let pos = p.text_document_position_params;
        let uri = pos.text_document.uri;
        let doc = self.docs.get(&uri)?;
        let offset = self.offset(doc, pos.position);
        let link = doc.doc.links.iter().find(|l| contains(&l.span, offset))?;
        let location = match &link.target {
            Target::Url(_) => return None,
            Target::Anchor(anchor) => {
                let scope = doc.doc.resolve_anchor(anchor)?;
                Location::new(uri.clone(), self.scope_range(doc, scope))
            }
            Target::Note { path, anchor } => {
                let note = self.ws.note(path)?;
                let uri = Url::from_file_path(&note.path).ok()?;
                let scope = anchor.as_deref().and_then(|a| note.doc.resolve_anchor(a));
                let range = match scope {
                    Some(scope) => {
                        let index = LineIndex::new(&note.source);
                        let line = note.doc.scope(scope).line.unwrap_or(0);
                        let span = note.doc.lines.get(line).map_or(0..0, |l| l.span.clone());
                        self.range(&note.source, &index, &span)
                    }
                    None => lsp_types::Range::default(),
                };
                Location::new(uri, range)
            }
            Target::File { path, .. } => {
                let path = self.ws.root().join(path);
                if !path.is_file() {
                    return None;
                }
                Location::new(Url::from_file_path(path).ok()?, Default::default())
            }
        };
        Some(GotoDefinitionResponse::Scalar(location))
    }

    pub(crate) fn document_links(&mut self, p: DocumentLinkParams) -> Option<Vec<DocumentLink>> {
        let doc = self.docs.get(&p.text_document.uri)?;
        let links = doc.doc.links.iter().filter_map(|link| {
            let target = match &link.target {
                Target::Url(url) => Url::parse(url).ok()?,
                Target::Anchor(_) => return None,
                Target::Note { path, .. } => {
                    Url::from_file_path(self.ws.note(path).map(|n| n.path.clone())?).ok()?
                }
                Target::File { path, .. } => {
                    let path = self.ws.root().join(path);
                    Url::from_file_path(path.is_file().then_some(path)?).ok()?
                }
            };
            Some(DocumentLink {
                range: self.range(&doc.text, &doc.index, &link.span),
                target: Some(target),
                tooltip: None,
                data: None,
            })
        });
        Some(links.collect())
    }

    /// References to what is under the cursor: on a link, to what it links
    /// to; on an `@id` or a heading, the links to that scope; on another
    /// token, the tokens giving its key; anywhere else, the links to the note.
    pub(crate) fn references(&mut self, p: ReferenceParams) -> Option<Vec<Location>> {
        self.refresh();
        let pos = p.text_document_position;
        let uri = pos.text_document.uri;
        let doc = self.docs.get(&uri)?;
        let offset = self.offset(doc, pos.position);
        let subject = self.subject(doc, offset)?;
        let mut out = Locator::new(self);

        match &subject {
            Subject::Note(name) => {
                for r in self.ws.backlinks(name) {
                    let note = self.ws.note(&r.note)?;
                    out.push(&r.note, &note.doc.links[r.link].span);
                }
            }
            Subject::Scope(name, scope) => {
                // The document holding the scope, and the note name it goes by.
                let (target, target_name) = match name {
                    Some(name) => (&self.ws.note(name)?.doc, name.as_str()),
                    None => (&doc.doc, ""),
                };
                if p.context.include_declaration
                    && let Some(line) = target.scope(*scope).line
                {
                    out.push_in(name.as_deref(), &uri, doc, &target.lines[line].span);
                }
                for link in &target.links {
                    if let Target::Anchor(a) = &link.target
                        && target.resolve_anchor(a) == Some(*scope)
                    {
                        out.push_in(name.as_deref(), &uri, doc, &link.span);
                    }
                }
                for r in self.ws.backlinks(target_name) {
                    let from = &self.ws.note(&r.note)?.doc;
                    let link = &from.links[r.link];
                    if let Target::Note {
                        anchor: Some(a), ..
                    } = &link.target
                        && target.resolve_anchor(a) == Some(*scope)
                    {
                        out.push(&r.note, &link.span);
                    }
                }
            }
            Subject::Key(key) => {
                let mut tokens = BTreeSet::new();
                for refs in self.ws.values(key).into_iter().flat_map(|v| v.values()) {
                    tokens.extend(refs.iter().map(|r| (r.note.as_str(), r.token)));
                }
                for (note, token) in tokens {
                    out.push(note, &self.ws.note(note)?.doc.tokens[token].span);
                }
                if doc.name.is_none() {
                    for t in doc.doc.tokens.iter().filter(|t| t.key == *key) {
                        out.push_in(None, &uri, doc, &t.span);
                    }
                }
            }
        }
        Some(out.finish())
    }

    /// What `references` at `offset` in `doc` is about.
    fn subject(&self, doc: &Doc, offset: usize) -> Option<Subject> {
        let name = doc.name.clone();
        if let Some(link) = doc.doc.links.iter().find(|l| contains(&l.span, offset)) {
            return match &link.target {
                Target::Note { path, anchor } => {
                    let target = if Some(path) == name.as_ref() {
                        Some(&doc.doc)
                    } else {
                        self.ws.note(path).map(|n| &n.doc)
                    };
                    let scope = anchor
                        .as_deref()
                        .zip(target)
                        .and_then(|(a, t)| t.resolve_anchor(a));
                    Some(match scope {
                        Some(scope) => Subject::Scope(Some(path.clone()), scope),
                        None => Subject::Note(path.clone()),
                    })
                }
                Target::Anchor(a) => Some(Subject::Scope(name, doc.doc.resolve_anchor(a)?)),
                Target::Url(_) | Target::File { .. } => None,
            };
        }
        if let Some(t) = doc.doc.tokens.iter().find(|t| contains(&t.span, offset)) {
            return Some(if t.key == "id" {
                Subject::Scope(name, t.scope)
            } else {
                Subject::Key(t.key.clone())
            });
        }
        let line = doc.doc.line_at(offset)?;
        match (&doc.doc.lines[line], name) {
            (l, name) if l.kind == LineKind::Heading => Some(Subject::Scope(name, l.owner?)),
            (_, name) => Some(Subject::Note(name?)),
        }
    }

    pub(crate) fn document_symbols(
        &mut self,
        p: DocumentSymbolParams,
    ) -> Option<DocumentSymbolResponse> {
        let doc = self.docs.get(&p.text_document.uri)?;
        let d = &doc.doc;
        // Headings and scopes with an `@id`, each nested in the nearest
        // enclosing one.
        let mut symbols: Vec<Option<DocumentSymbol>> = d
            .scopes
            .iter()
            .enumerate()
            .map(|(i, scope)| {
                let (name, kind, detail) = match &scope.kind {
                    ScopeKind::Heading { .. } => (
                        heading_title(&doc.text, d, ScopeId(i)),
                        SymbolKind::STRING,
                        scope.id.as_ref().map(|id| format!("#{id}")),
                    ),
                    ScopeKind::Item(_) | ScopeKind::Row { .. } => {
                        let kind = if matches!(scope.kind, ScopeKind::Item(_)) {
                            "item"
                        } else {
                            "row"
                        };
                        (scope.id.clone()?, SymbolKind::KEY, Some(kind.to_owned()))
                    }
                    ScopeKind::File => return None,
                };
                let line = scope.line?;
                let last = scope.extent.end.max(line + 1) - 1;
                let extent = d.lines[line].span.start..d.lines[last].span.end;
                #[allow(deprecated)]
                Some(DocumentSymbol {
                    name,
                    detail,
                    kind,
                    tags: None,
                    deprecated: None,
                    range: self.range(&doc.text, &doc.index, &extent),
                    selection_range: self.range(&doc.text, &doc.index, &d.lines[line].span),
                    children: None,
                })
            })
            .collect();
        for i in (1..symbols.len()).rev() {
            let Some(symbol) = symbols[i].take() else {
                continue;
            };
            let mut parent = d.scopes[i].parent;
            while let Some(p) = parent.filter(|p| p.0 > 0 && symbols[p.0].is_none()) {
                parent = d.scopes[p.0].parent;
            }
            match parent.filter(|p| p.0 > 0) {
                Some(p) => symbols[p.0]
                    .as_mut()
                    .expect("found above")
                    .children
                    .get_or_insert_with(Vec::new)
                    .insert(0, symbol),
                None => symbols[i] = Some(symbol),
            }
        }
        Some(DocumentSymbolResponse::Nested(
            symbols.into_iter().flatten().collect(),
        ))
    }

    /// `@key` finds the tokens giving `key`, and `@key:value` those giving it
    /// that value. Any other query finds headings and `@id`s containing it,
    /// ignoring case.
    pub(crate) fn workspace_symbols(
        &mut self,
        p: WorkspaceSymbolParams,
    ) -> Option<WorkspaceSymbolResponse> {
        self.refresh();
        let mut found: Vec<(&str, Span, String, SymbolKind)> = Vec::new();
        if let Some(query) = p.query.strip_prefix('@') {
            let (key, value) = match query.split_once(':') {
                Some((key, value)) => (key, Some(value)),
                None => (query, None),
            };
            let mut tokens = BTreeSet::new();
            for (v, refs) in self.ws.values(key).into_iter().flatten() {
                if value.is_none_or(|value| value == v) {
                    tokens.extend(refs.iter().map(|r| (r.note.as_str(), r.token)));
                }
            }
            for (note, token) in tokens {
                let n = self.ws.note(note)?;
                let span = n.doc.tokens[token].span.clone();
                let name = n.source[span.clone()].to_owned();
                found.push((note, span, name, SymbolKind::KEY));
            }
        } else {
            let query = p.query.to_lowercase();
            for (note, n) in self.ws.notes() {
                for (i, scope) in n.doc.scopes.iter().enumerate() {
                    let Some(line) = scope.line else { continue };
                    let span = n.doc.lines[line].span.clone();
                    if let ScopeKind::Heading { .. } = scope.kind {
                        let title = heading_title(&n.source, &n.doc, ScopeId(i));
                        if title.to_lowercase().contains(&query) {
                            found.push((note, span.clone(), title, SymbolKind::STRING));
                        }
                    }
                    if let Some(id) = &scope.id
                        && id.to_lowercase().contains(&query)
                    {
                        found.push((note, span, format!("#{id}"), SymbolKind::KEY));
                    }
                }
            }
        }

        let mut out = Locator::new(self);
        let symbols = found
            .into_iter()
            .filter_map(|(note, span, name, kind)| {
                #[allow(deprecated)]
                Some(SymbolInformation {
                    name,
                    kind,
                    tags: None,
                    deprecated: None,
                    location: out.location(note, &span)?,
                    container_name: Some(note.to_owned()),
                })
            })
            .collect();
        Some(WorkspaceSymbolResponse::Flat(symbols))
    }

    /// The links to a note from other notes.
    pub(crate) fn backlinks(&mut self, p: BacklinksParams) -> Vec<Location> {
        self.refresh();
        let mut out = Locator::new(self);
        for r in self.ws.backlinks(&p.note) {
            if r.note != p.note
                && let Some(note) = self.ws.note(&r.note)
            {
                out.push(&r.note, &note.doc.links[r.link].span);
            }
        }
        out.finish()
    }

    /// Every key used as a flag, sorted.
    pub(crate) fn tags(&mut self) -> Vec<String> {
        self.refresh();
        let mut tags: Vec<String> = self
            .ws
            .keys()
            .filter(|k| self.ws.values(k).is_some_and(|v| v.contains_key("true")))
            .map(str::to_owned)
            .collect();
        tags.sort();
        tags
    }

    /// The range of a scope's line in an open document.
    fn scope_range(&self, doc: &Doc, scope: ScopeId) -> lsp_types::Range {
        let line = doc.doc.scope(scope).line.unwrap_or(0);
        let span = doc.doc.lines.get(line).map_or(0..0, |l| l.span.clone());
        self.range(&doc.text, &doc.index, &span)
    }
}

/// Locations in notes, indexing each note's lines once.
struct Locator<'a> {
    server: &'a Server,
    indexes: HashMap<&'a str, LineIndex>,
    out: Vec<Location>,
}

impl<'a> Locator<'a> {
    fn new(server: &'a Server) -> Self {
        Locator {
            server,
            indexes: HashMap::new(),
            out: Vec::new(),
        }
    }

    fn location(&mut self, note: &'a str, span: &Span) -> Option<Location> {
        let n = self.server.ws.note(note)?;
        let index = self
            .indexes
            .entry(note)
            .or_insert_with(|| LineIndex::new(&n.source));
        let range = self.server.range(&n.source, index, span);
        Some(Location::new(Url::from_file_path(&n.path).ok()?, range))
    }

    fn push(&mut self, note: &'a str, span: &Span) {
        if let Some(location) = self.location(note, span) {
            self.out.push(location);
        }
    }

    /// A location in the note `name`, or in the open document `doc` at `uri`
    /// when it isn't a note.
    fn push_in(&mut self, name: Option<&'a str>, uri: &Url, doc: &Doc, span: &Span) {
        match name {
            Some(name) => self.push(name, span),
            None => {
                let range = self.server.range(&doc.text, &doc.index, span);
                self.out.push(Location::new(uri.clone(), range));
            }
        }
    }

    /// The locations, in file and then position order.
    fn finish(mut self) -> Vec<Location> {
        self.out.sort_by(|a, b| {
            let key = |l: &Location| {
                (
                    l.uri.to_string(),
                    l.range.start.line,
                    l.range.start.character,
                )
            };
            key(a).cmp(&key(b))
        });
        self.out
    }
}

/// `offset` is in `span`, or just after it (where a cursor ends up after
/// typing the span).
pub(crate) fn contains(span: &Span, offset: usize) -> bool {
    span.start <= offset && offset <= span.end
}

/// A heading's text without its tokens, with runs of whitespace collapsed.
pub(crate) fn heading_title(src: &str, doc: &Document, scope: ScopeId) -> String {
    let s = doc.scope(scope);
    let ScopeKind::Heading { text, level, .. } = &s.kind else {
        return String::new();
    };
    let mut title = String::new();
    let mut at = text.start;
    for t in doc.tokens.iter().filter(|t| Some(t.line) == s.line) {
        title.push_str(&src[at..t.span.start]);
        at = t.span.end;
    }
    title.push_str(&src[at..text.end]);
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        "#".repeat(usize::from(*level))
    } else {
        title
    }
}
