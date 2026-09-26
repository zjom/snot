//! Pass 2: scopes (NOTE_SPEC 3), lists (5), tables (10) and the inline content
//! of every line, assembled into a [`Document`].

use std::collections::HashMap;

use crate::inline::{self, Scanned};
use crate::lines::{self, RawKind, RawLine};
use crate::{
    Code, Diagnostic, Document, Inline, Item, Line, LineKind, Link, Marker, Scope, ScopeId,
    ScopeKind, Table, Token, TokenForm, is_key, link, slug,
};

struct Builder<'s> {
    src: &'s str,
    doc: Document,
    /// Open headings, outermost first, with their levels.
    headings: Vec<(u8, ScopeId)>,
    /// Items whose continuation lines may follow, outermost first, with their
    /// depths. A blank line closes them all.
    continuing: Vec<(usize, ScopeId)>,
    /// The current list's most recent item at each depth, for parents. Blank
    /// lines don't end a list; headings and lines outside any item do.
    list: Vec<ScopeId>,
    /// The number of the previous ordered item at each depth among the
    /// current siblings.
    numbers: Vec<Option<u64>>,
    /// The open table and its rows' indentation.
    table: Option<(usize, usize)>,
}

impl Builder<'_> {
    fn section(&self) -> ScopeId {
        self.headings.last().map_or(ScopeId::FILE, |h| h.1)
    }

    fn end_list(&mut self) {
        self.list.clear();
        self.numbers.clear();
    }

    /// The owner of a line that defines no scope, at `depth`: the innermost
    /// item it continues, or else the section, which ends any list.
    fn owner_at(&mut self, depth: usize) -> ScopeId {
        while self.continuing.last().is_some_and(|&(d, _)| d >= depth) {
            self.continuing.pop();
        }
        match self.continuing.last() {
            Some(&(_, item)) => item,
            None => {
                self.end_list();
                self.section()
            }
        }
    }

    fn add_scope(&mut self, kind: ScopeKind, line: usize, parent: ScopeId) -> ScopeId {
        self.doc.scopes.push(Scope {
            kind,
            line: Some(line),
            parent: Some(parent),
            extent: line..line + 1,
            metadata: Default::default(),
            id: None,
        });
        ScopeId(self.doc.scopes.len() - 1)
    }

    fn push_line(&mut self, raw: &RawLine, kind: LineKind, owner: Option<ScopeId>) {
        self.doc.lines.push(Line {
            span: raw.span.clone(),
            eol: raw.eol,
            depth: raw.depth,
            kind,
            owner,
        });
    }

    /// Record what was scanned on `line`, attaching it to `scope`.
    fn add_inline(&mut self, scanned: Scanned, line: usize, scope: ScopeId) {
        for t in scanned.tokens {
            self.doc.tokens.push(Token {
                key: self.src[t.key_span.clone()].to_owned(),
                span: t.span,
                key_span: t.key_span,
                form: t.form,
                values: t.values,
                value_span: t.value_span,
                line,
                scope,
            });
        }
        for l in scanned.links {
            let (target, problem) = link::classify(&l.target);
            if let Some((code, message)) = problem {
                self.doc
                    .diagnostics
                    .push(Diagnostic::new(code, l.target_span.clone(), message));
            }
            self.doc.links.push(Link {
                span: l.span,
                target,
                target_span: l.target_span,
                label: l.label,
                label_span: l.label_span,
                line,
                scope,
            });
        }
        for (kind, span, content) in scanned.inlines {
            self.doc.inlines.push(Inline {
                kind,
                span,
                content,
                line,
            });
        }
    }

    fn heading(&mut self, raw: &RawLine, idx: usize) {
        let RawKind::Heading { level, ref text } = raw.kind else {
            unreachable!()
        };
        let text = text.clone();
        self.table = None;
        self.continuing.clear();
        self.end_list();
        while self.headings.last().is_some_and(|&(l, _)| l >= level) {
            self.headings.pop();
        }
        let slug = slug::slug(&self.src[text.clone()]);
        let kind = ScopeKind::Heading {
            level,
            text: text.clone(),
            slug,
        };
        let id = self.add_scope(kind, idx, self.section());
        self.headings.push((level, id));
        self.push_line(raw, LineKind::Heading, Some(id));
        let scanned = inline::scan(self.src, raw.span.start, text, false);
        self.add_inline(scanned, idx, id);
    }

    fn item(&mut self, raw: &RawLine, idx: usize) {
        let RawKind::Item {
            marker,
            ref marker_span,
            task,
            ref content,
        } = raw.kind
        else {
            unreachable!()
        };
        let (marker_span, content) = (marker_span.clone(), content.clone());
        self.table = None;
        let mut depth = raw.depth;
        if depth > 0 && self.list.len() < depth {
            self.doc.diagnostics.push(Diagnostic::new(
                Code::OrphanItem,
                marker_span.clone(),
                format!(
                    "no list item at depth {} above this one; read as depth 0",
                    depth - 1
                ),
            ));
            depth = 0;
        }
        while self.continuing.last().is_some_and(|&(d, _)| d >= depth) {
            self.continuing.pop();
        }
        self.list.truncate(depth);
        self.numbers.truncate(depth + 1);
        self.numbers.resize(depth + 1, None);
        let number = match marker {
            Marker::Dash => None,
            Marker::Plus => Some(self.numbers[depth].map_or(1, |n| n.saturating_add(1))),
            Marker::Number(n) => Some(n),
        };
        if number.is_some() {
            self.numbers[depth] = number;
        }
        let parent = if depth > 0 {
            self.list[depth - 1]
        } else {
            self.section()
        };
        let kind = ScopeKind::Item(Item {
            depth,
            marker,
            marker_span,
            number,
            task,
            content: content.clone(),
        });
        let id = self.add_scope(kind, idx, parent);
        self.list.push(id);
        self.continuing.push((depth, id));
        self.push_line(raw, LineKind::Item, Some(id));
        let scanned = inline::scan(self.src, raw.span.start, content, false);
        self.add_inline(scanned, idx, id);
    }

    fn row(&mut self, raw: &RawLine, idx: usize) {
        let parent = self.owner_at(raw.depth);
        let table = match self.table {
            Some((indent, table)) if indent == raw.indent => table,
            _ => {
                self.doc.tables.push(Table {
                    lines: idx..idx,
                    header: None,
                });
                self.doc.tables.len() - 1
            }
        };
        self.table = Some((raw.indent, table));
        self.doc.tables[table].lines.end = idx + 1;

        let scanned = inline::scan(
            self.src,
            raw.span.start,
            raw.span.start + raw.indent..raw.span.end,
            true,
        );
        let is_separator = scanned.cells.iter().all(|c| {
            let cell = &self.src[c.clone()];
            !cell.is_empty() && cell.bytes().all(|b| b == b'-')
        });
        if is_separator {
            self.push_line(raw, LineKind::Separator, Some(parent));
            return;
        }
        let kind = ScopeKind::Row {
            table,
            cells: scanned.cells.clone(),
        };
        let id = self.add_scope(kind, idx, parent);
        self.push_line(raw, LineKind::Row, Some(id));
        self.add_inline(scanned, idx, id);
    }
}

pub(crate) fn build(src: &str) -> Document {
    let mut diagnostics = Vec::new();
    let (raw_lines, blocks) = lines::classify(src, &mut diagnostics);
    let mut b = Builder {
        src,
        doc: Document {
            scopes: vec![Scope {
                kind: ScopeKind::File,
                line: None,
                parent: None,
                extent: 0..raw_lines.len(),
                metadata: Default::default(),
                id: None,
            }],
            diagnostics,
            ..Default::default()
        },
        headings: Vec::new(),
        continuing: Vec::new(),
        list: Vec::new(),
        numbers: Vec::new(),
        table: None,
    };

    let mut idx = 0;
    while idx < raw_lines.len() {
        let raw = &raw_lines[idx];
        match &raw.kind {
            RawKind::Blank => {
                b.table = None;
                b.continuing.clear();
                b.push_line(raw, LineKind::Blank, None);
            }
            RawKind::Heading { .. } => b.heading(raw, idx),
            RawKind::Item { .. } => b.item(raw, idx),
            RawKind::Row => b.row(raw, idx),
            RawKind::Text => {
                b.table = None;
                let owner = b.owner_at(raw.depth);
                b.push_line(raw, LineKind::Text, Some(owner));
                let scanned = inline::scan(src, raw.span.start, raw.span.clone(), false);
                b.add_inline(scanned, idx, owner);
            }
            RawKind::FenceOpen(block) => {
                // The whole block, fences included, belongs to the opening
                // fence's owner; blank lines inside don't end an item.
                b.table = None;
                let owner = b.owner_at(raw.depth);
                let last = blocks[*block].close.unwrap_or(raw_lines.len() - 1);
                for line in &raw_lines[idx..=last] {
                    let kind = match line.kind {
                        RawKind::Verbatim => LineKind::Verbatim,
                        _ => LineKind::Fence,
                    };
                    b.push_line(line, kind, Some(owner));
                }
                idx = last + 1;
                continue;
            }
            RawKind::FenceClose | RawKind::Verbatim => unreachable!("consumed with their block"),
        }
        idx += 1;
    }

    let mut doc = b.doc;
    doc.blocks = blocks;
    finish(&mut doc);
    doc
}

/// Everything that needs the whole document: metadata, ids, extents, headers.
fn finish(doc: &mut Document) {
    for t in &doc.tokens {
        doc.scopes[t.scope.0]
            .metadata
            .entry(t.key.clone())
            .or_default()
            .extend(t.values.iter().cloned());
    }

    // A scope's id is its one `@id` value, if that matches the key syntax. A
    // bare `@id` means `true`, which would, so it is checked by form.
    let id_tokens = || doc.tokens.iter().filter(|t| t.key == "id");
    let has_bare: Vec<ScopeId> = id_tokens()
        .filter(|t| t.form == TokenForm::Flag)
        .map(|t| t.scope)
        .collect();
    for (i, scope) in doc.scopes.iter_mut().enumerate() {
        if has_bare.contains(&ScopeId(i)) {
            continue;
        }
        if let Some([id]) = scope.metadata.get("id").map(Vec::as_slice)
            && is_key(id)
        {
            scope.id = Some(id.clone());
        }
    }
    let mut ids: HashMap<&str, ScopeId> = HashMap::new();
    for t in id_tokens() {
        let message = match (&doc.scopes[t.scope.0].id, t.form) {
            (_, TokenForm::Flag) => "a bare `@id` has no value; write `@id:name`".to_owned(),
            (None, _) => {
                "`@id` must be one value: a letter, then letters, digits, `-` or `_`".to_owned()
            }
            (Some(id), _) => match ids.get(id.as_str()) {
                Some(&first) if first != t.scope => {
                    doc.diagnostics.push(Diagnostic::new(
                        Code::DuplicateId,
                        t.span.clone(),
                        format!("`@id:{id}` is already used in this file"),
                    ));
                    continue;
                }
                _ => {
                    ids.insert(id, t.scope);
                    continue;
                }
            },
        };
        doc.diagnostics
            .push(Diagnostic::new(Code::InvalidId, t.span.clone(), message));
    }

    // A heading's section runs to the next heading of the same or lower level.
    let headings: Vec<(usize, u8, usize)> = doc
        .scopes
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match s.kind {
            ScopeKind::Heading { level, .. } => Some((i, level, s.line?)),
            _ => None,
        })
        .collect();
    for (k, &(i, level, line)) in headings.iter().enumerate() {
        let end = headings[k + 1..]
            .iter()
            .find(|h| h.1 <= level)
            .map_or(doc.lines.len(), |h| h.2);
        doc.scopes[i].extent = line..end;
    }

    // An item covers every line owned by it or by scopes nested in it.
    for (line, l) in doc.lines.iter().enumerate() {
        let mut scope = l.owner;
        while let Some(s) = scope {
            let s = &mut doc.scopes[s.0];
            if let ScopeKind::Item(_) = s.kind {
                s.extent.end = s.extent.end.max(line + 1);
            }
            scope = s.parent;
        }
    }

    for table in &mut doc.tables {
        let second = table.lines.start + 1;
        if second < table.lines.end && doc.lines[second].kind == LineKind::Separator {
            table.header = Some(table.lines.start);
        }
    }

    doc.inlines.sort_by_key(|i| i.span.start);
    doc.diagnostics.sort_by_key(|d| d.span.start);
}
