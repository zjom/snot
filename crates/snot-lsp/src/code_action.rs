//! Code actions: creating the note a link points at, when it doesn't exist.

use std::collections::BTreeSet;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CreateFile,
    CreateFileOptions, DocumentChangeOperation, DocumentChanges, NumberOrString, ResourceOp, Url,
    WorkspaceEdit,
};
use snot_check::Code;
use snot_syntax::Target;

use crate::Server;
use crate::rename::name_problem;

impl Server {
    pub(crate) fn code_actions(&mut self, p: CodeActionParams) -> Option<Vec<CodeActionOrCommand>> {
        if !self.can_create_files {
            return None;
        }
        if let Some(only) = &p.context.only
            && !only
                .iter()
                .any(|k| CodeActionKind::QUICKFIX.as_str().starts_with(k.as_str()))
        {
            return None;
        }
        self.refresh();
        let uri = p.text_document.uri;
        let doc = self.docs.get(&uri)?;
        let start = self.offset(doc, p.range.start);
        let end = self.offset(doc, p.range.end);

        // Each missing note once, however many links to it are in range.
        let mut missing = BTreeSet::new();
        for link in &doc.doc.links {
            if link.span.start > end || link.span.end < start {
                continue;
            }
            if let Target::Note { path, .. } = &link.target
                && doc.name.as_deref() != Some(path)
                && self.ws.note(path).is_none()
                && name_problem(path).is_none()
                && !self.ws.path_of(path).exists()
            {
                missing.insert(path.clone());
            }
        }

        let actions = missing.into_iter().filter_map(|name| {
            let new_uri = Url::from_file_path(self.ws.path_of(&name)).ok()?;
            // The broken-link diagnostics this fixes.
            let diagnostics = p
                .context
                .diagnostics
                .iter()
                .filter(|d| {
                    d.code
                        == Some(NumberOrString::String(
                            Code::MissingNote.as_str().to_owned(),
                        ))
                })
                .filter(|d| {
                    doc.doc.links.iter().any(|l| {
                        matches!(&l.target, Target::Note { path, .. } if *path == name)
                            && self.range(&doc.text, &doc.index, &l.target_span) == d.range
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            let create = ResourceOp::Create(CreateFile {
                uri: new_uri,
                options: Some(CreateFileOptions {
                    overwrite: Some(false),
                    ignore_if_exists: Some(true),
                }),
                annotation_id: None,
            });
            Some(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Create note `{name}`"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: (!diagnostics.is_empty()).then_some(diagnostics),
                edit: Some(WorkspaceEdit {
                    document_changes: Some(DocumentChanges::Operations(vec![
                        DocumentChangeOperation::Op(create),
                    ])),
                    ..Default::default()
                }),
                is_preferred: Some(true),
                ..Default::default()
            }))
        });
        Some(actions.collect())
    }
}
