//! The conformance corpus: each `conformance/*.snot` is parsed and compared
//! with the expected reading in the `.json` beside it (format described in
//! `conformance/README.md`). Run with `SNOT_BLESS=1` to write the `.json`
//! files from the current parser, then review the diff by hand.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use snot_syntax::{
    BlockKind, Document, InlineKind, LineKind, Marker, ScopeKind, Target, Task, parse,
};

fn line_no(line: usize) -> Value {
    json!(line + 1)
}

/// Insert `key: value` unless the value is null or an empty object.
fn put(obj: &mut Map<String, Value>, key: &str, value: Value) {
    let empty = value.is_null() || value.as_object().is_some_and(Map::is_empty);
    if !empty {
        obj.insert(key.to_owned(), value);
    }
}

fn view(src: &str, doc: &Document) -> Value {
    let lines: Vec<Value> = doc
        .lines
        .iter()
        .map(|l| {
            let kind = match l.kind {
                LineKind::Blank => "blank",
                LineKind::Heading => "heading",
                LineKind::Item => "item",
                LineKind::Row => "row",
                LineKind::Separator => "separator",
                LineKind::Text => "text",
                LineKind::Fence => "fence",
                LineKind::Verbatim => "verbatim",
            };
            match l.owner {
                Some(owner) => json!(format!("{kind}:{}", owner.0)),
                None => json!(kind),
            }
        })
        .collect();

    let scopes: Vec<Value> = doc
        .scopes
        .iter()
        .map(|s| {
            let mut o = Map::new();
            match &s.kind {
                ScopeKind::File => {
                    o.insert("kind".into(), json!("file"));
                }
                ScopeKind::Heading { level, slug, .. } => {
                    o.insert("kind".into(), json!("heading"));
                    o.insert("level".into(), json!(level));
                    o.insert("slug".into(), json!(slug));
                }
                ScopeKind::Item(item) => {
                    o.insert("kind".into(), json!("item"));
                    o.insert("depth".into(), json!(item.depth));
                    let marker = match item.marker {
                        Marker::Dash => "-".to_owned(),
                        Marker::Plus => "+".to_owned(),
                        Marker::Number(_) => src[item.marker_span.clone()].to_owned(),
                    };
                    o.insert("marker".into(), json!(marker));
                    put(&mut o, "number", json!(item.number));
                    let task = item.task.map(|t| match t {
                        Task::Open => "open",
                        Task::Done => "done",
                        Task::Cancelled => "cancelled",
                    });
                    put(&mut o, "task", json!(task));
                }
                ScopeKind::Row { table, cells } => {
                    o.insert("kind".into(), json!("row"));
                    o.insert("table".into(), json!(table));
                    let cells: Vec<&str> = cells.iter().map(|c| &src[c.clone()]).collect();
                    o.insert("cells".into(), json!(cells));
                }
            }
            put(&mut o, "line", s.line.map_or(Value::Null, line_no));
            put(&mut o, "parent", json!(s.parent.map(|p| p.0)));
            o.insert("extent".into(), json!([s.extent.start + 1, s.extent.end]));
            let metadata: Map<String, Value> = s
                .metadata
                .iter()
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect();
            put(&mut o, "metadata", Value::Object(metadata));
            put(&mut o, "id", json!(s.id));
            Value::Object(o)
        })
        .collect();

    let links: Vec<Value> = doc
        .links
        .iter()
        .map(|l| {
            let mut o = Map::new();
            o.insert("line".into(), line_no(l.line));
            o.insert("text".into(), json!(&src[l.span.clone()]));
            let (kind, path, anchor) = match &l.target {
                Target::Url(url) => ("url", Some(url), None),
                Target::Anchor(a) => ("anchor", None, Some(a)),
                Target::Note { path, anchor } => ("note", Some(path), anchor.as_ref()),
                Target::File { path, anchor } => ("file", Some(path), anchor.as_ref()),
            };
            o.insert("kind".into(), json!(kind));
            put(
                &mut o,
                if kind == "url" { "url" } else { "path" },
                json!(path),
            );
            put(&mut o, "anchor", json!(anchor));
            put(&mut o, "label", json!(l.label));
            o.insert("scope".into(), json!(l.scope.0));
            Value::Object(o)
        })
        .collect();

    let inlines: Vec<Value> = doc
        .inlines
        .iter()
        .map(|i| {
            let kind = match i.kind {
                InlineKind::Code => "code",
                InlineKind::Math => "math",
                InlineKind::Bold => "bold",
                InlineKind::Underline => "underline",
            };
            json!({ "line": line_no(i.line), "kind": kind, "content": &src[i.content.clone()] })
        })
        .collect();

    let blocks: Vec<Value> = doc
        .blocks
        .iter()
        .map(|b| {
            let mut o = Map::new();
            match &b.kind {
                BlockKind::Code { language } => {
                    o.insert("kind".into(), json!("code"));
                    put(&mut o, "language", json!(language));
                }
                BlockKind::Math => {
                    o.insert("kind".into(), json!("math"));
                }
            }
            o.insert("open".into(), line_no(b.open));
            o.insert("close".into(), json!(b.close.map(|c| c + 1)));
            Value::Object(o)
        })
        .collect();

    let tables: Vec<Value> = doc
        .tables
        .iter()
        .map(|t| {
            let mut o = Map::new();
            o.insert("lines".into(), json!([t.lines.start + 1, t.lines.end]));
            put(&mut o, "header", t.header.map_or(Value::Null, line_no));
            Value::Object(o)
        })
        .collect();

    let diagnostics: Vec<Value> = doc
        .diagnostics
        .iter()
        .map(|d| {
            let line = doc.line_at(d.span.start).unwrap_or(0);
            json!({ "code": d.code.as_str(), "line": line + 1, "text": &src[d.span.clone()] })
        })
        .collect();

    let mut o = Map::new();
    o.insert("lines".into(), json!(lines));
    o.insert("scopes".into(), json!(scopes));
    for (key, list) in [
        ("links", links),
        ("inlines", inlines),
        ("blocks", blocks),
        ("tables", tables),
        ("diagnostics", diagnostics),
    ] {
        if !list.is_empty() {
            o.insert(key.into(), json!(list));
        }
    }
    Value::Object(o)
}

fn corpus() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "snot"))
        .collect();
    files.sort();
    files
}

#[test]
fn conformance() {
    let bless = std::env::var_os("SNOT_BLESS").is_some();
    let mut failures = Vec::new();
    let files = corpus();
    assert!(!files.is_empty(), "no conformance files found");
    for path in files {
        let src = fs::read_to_string(&path).unwrap();
        let actual = serde_json::to_string_pretty(&view(&src, &parse(&src))).unwrap() + "\n";
        let expected_path = path.with_extension("json");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        match fs::read_to_string(&expected_path) {
            Ok(expected) if expected == actual => {}
            _ if bless => fs::write(&expected_path, &actual).unwrap(),
            Ok(expected) => {
                let diff = similar::TextDiff::from_lines(&expected, &actual)
                    .unified_diff()
                    .header("expected", "actual")
                    .to_string();
                failures.push(format!("{name}:\n{diff}"));
            }
            Err(_) => failures.push(format!("{name}: no .json (run with SNOT_BLESS=1)")),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
