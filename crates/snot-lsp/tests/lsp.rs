//! The server, driven over an in-memory connection as an editor would.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::thread::JoinHandle;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use serde_json::{Value, json};

struct Client {
    conn: Connection,
    server: Option<JoinHandle<()>>,
    next_id: i32,
    /// The last diagnostics published for each URI.
    diagnostics: HashMap<String, Value>,
    /// Server requests seen, by method.
    server_requests: Vec<String>,
    dir: tempfile::TempDir,
}

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

impl Client {
    fn start(files: &[(&str, &str)]) -> Client {
        Client::start_with(
            files,
            json!({ "general": { "positionEncodings": ["utf-8", "utf-16"] } }),
        )
    }

    fn start_with(files: &[(&str, &str)], capabilities: Value) -> Client {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), files);
        let (client, server) = Connection::memory();
        let server = std::thread::spawn(move || snot_lsp::run(server).unwrap());
        let mut c = Client {
            conn: client,
            server: Some(server),
            next_id: 0,
            diagnostics: HashMap::new(),
            server_requests: Vec::new(),
            dir,
        };
        let root = c.dir.path().to_str().unwrap().to_owned();
        c.request(
            "initialize",
            json!({
                "capabilities": capabilities,
                "initializationOptions": { "root": root, "width": 20 },
            }),
        );
        c.notify("initialized", json!({}));
        c
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn uri(&self, path: &str) -> String {
        url(&self.root().join(path))
    }

    fn notify(&self, method: &str, params: Value) {
        let n = Notification::new(method.to_owned(), params);
        self.conn.sender.send(n.into()).unwrap();
    }

    /// Send a request and wait for its result, keeping what the server sends
    /// meanwhile.
    fn request(&mut self, method: &str, params: Value) -> Value {
        match self.try_request(method, params) {
            Ok(result) => result,
            Err(e) => panic!("{method}: {e}"),
        }
    }

    /// Send a request and wait for its result or error message.
    fn try_request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        let req = Request::new(id.clone(), method.to_owned(), params);
        self.conn.sender.send(req.into()).unwrap();
        loop {
            match self.conn.receiver.recv().unwrap() {
                Message::Response(r) if r.id == id => {
                    return r.response_result.map_err(|e| e.message);
                }
                Message::Response(_) => {}
                Message::Notification(n) => {
                    if n.method == "textDocument/publishDiagnostics" {
                        let uri = n.params["uri"].as_str().unwrap().to_owned();
                        self.diagnostics
                            .insert(uri, n.params["diagnostics"].clone());
                    }
                }
                Message::Request(r) => {
                    self.server_requests.push(r.method.clone());
                    let response = Response::new_ok(r.id, Value::Null);
                    self.conn.sender.send(response.into()).unwrap();
                }
            }
        }
    }

    /// Open a note, with its text on disk or `text`.
    fn open(&mut self, path: &str, text: Option<&str>) -> String {
        let uri = self.uri(path);
        let text = match text {
            Some(text) => text.to_owned(),
            None => fs::read_to_string(self.root().join(path)).unwrap(),
        };
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "snot", "version": 1, "text": text } }),
        );
        uri
    }

    fn change(&mut self, uri: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }],
            }),
        );
    }

    /// The codes of the diagnostics last published for `uri`.
    fn codes(&mut self, uri: &str) -> Vec<String> {
        self.request("snot/tags", Value::Null); // Wait for the server to catch up.
        self.diagnostics[uri]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["code"].as_str().unwrap().to_owned())
            .collect()
    }

    /// `(file, line, start column, end column)` of each location.
    fn at(&self, locations: &Value) -> Vec<(String, u64, u64, u64)> {
        let root = format!("{}/", url(self.root()));
        locations
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                let file = l["uri"]
                    .as_str()
                    .unwrap()
                    .strip_prefix(&root)
                    .unwrap()
                    .to_owned();
                let r = &l["range"];
                let n = |v: &Value| v.as_u64().unwrap();
                (
                    file,
                    n(&r["start"]["line"]),
                    n(&r["start"]["character"]),
                    n(&r["end"]["character"]),
                )
            })
            .collect()
    }

    fn position(&mut self, method: &str, uri: &str, line: u32, character: u32) -> Value {
        self.request(
            method,
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "context": { "includeDeclaration": false },
            }),
        )
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        if let Some(server) = self.server.take() {
            server.join().unwrap();
        }
    }
}

fn url(path: &Path) -> String {
    lsp_types::Url::from_file_path(path).unwrap().to_string()
}

fn at(file: &str, line: u64, start: u64, end: u64) -> (String, u64, u64, u64) {
    (file.to_owned(), line, start, end)
}

#[test]
fn publishes_diagnostics_as_the_note_changes() {
    let mut c = Client::start(&[("a.snot", "[[b]]\n\t- x\n"), ("b.snot", "# B\n")]);
    let uri = c.open("a.snot", None);
    assert_eq!(c.codes(&uri), ["S001", "S002"]);
    c.change(&uri, 2, "[[c]] [[b#b]] [[b#nope]]\n");
    assert_eq!(c.codes(&uri), ["L003", "L004"]);
    let d = &c.diagnostics[&uri][0];
    assert_eq!(
        d["range"],
        json!({ "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 3 } })
    );
    assert_eq!(d["severity"], 2);
    assert_eq!(d["source"], "snot");

    // Creating `c` on disk and saving: the link resolves.
    write(c.root(), &[("c.snot", "")]);
    c.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(c.codes(&uri), ["L004"]);

    c.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(c.codes(&uri), Vec::<String>::new());
}

#[test]
fn formats_a_document_or_a_range() {
    let mut c = Client::start(&[]);
    let uri = c.open("a.snot", Some("# A @b  \n# C @d\n\n\n"));
    let edits = c.request(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 8, "insertSpaces": true } }),
    );
    assert_eq!(
        edits,
        json!([
            { "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 8 } },
              "newText": "# A               @b" },
            { "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 6 } },
              "newText": "# C               @d" },
            { "range": { "start": { "line": 1, "character": 6 }, "end": { "line": 4, "character": 0 } },
              "newText": "\n" },
        ])
    );

    // A width option wins over the configured width.
    let edits = c.request(
        "textDocument/rangeFormatting",
        json!({
            "textDocument": { "uri": uri },
            "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 0 } },
            "options": { "tabSize": 8, "insertSpaces": true, "width": 8 },
        }),
    );
    assert_eq!(edits.as_array().unwrap().len(), 2);
    assert_eq!(edits[0]["newText"], "# C   @d");
}

fn linked() -> Client {
    Client::start(&[
        (
            "a.snot",
            "# Own @id:own\n[[b#two]] [[#own]] [[img/x.png]] [[b]]\n- @due:2026 @urgent\n",
        ),
        (
            "b.snot",
            "# One\n\ntext [[b#two]]\n\n## Two @due:2027\n- item @id:it\n",
        ),
        ("sub/c.snot", "[[b]] [[b#it]] @urgent\n"),
        ("img/x.png", ""),
    ])
}

#[test]
fn goes_to_definitions() {
    let mut c = linked();
    let a = c.open("a.snot", None);
    let def = |c: &mut Client, character| {
        let d = c.position("textDocument/definition", &a, 1, character);
        if d.is_null() {
            vec![]
        } else {
            c.at(&json!([d]))
        }
    };
    assert_eq!(def(&mut c, 2), [at("b.snot", 4, 0, 16)]);
    assert_eq!(def(&mut c, 12), [at("a.snot", 0, 0, 13)]);
    assert_eq!(def(&mut c, 22), [at("img/x.png", 0, 0, 0)]);
    assert_eq!(def(&mut c, 35), [at("b.snot", 0, 0, 0)]);
    // Not on a link.
    let d = c.position("textDocument/definition", &a, 0, 3);
    assert!(d.is_null());
}

#[test]
fn lists_document_links() {
    let mut c = linked();
    let a = c.open(
        "a.snot",
        Some("[[b]] [[#x]] [[https://example.com/a]] [[img/x.png]] [[nope]]\n"),
    );
    let links = c.request(
        "textDocument/documentLink",
        json!({ "textDocument": { "uri": a } }),
    );
    let targets: Vec<&str> = links
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["target"].as_str().unwrap())
        .collect();
    assert_eq!(
        targets,
        [
            c.uri("b.snot").as_str(),
            "https://example.com/a",
            c.uri("img/x.png").as_str()
        ]
    );
}

#[test]
fn finds_references() {
    let mut c = linked();
    let a = c.open("a.snot", None);
    let b = c.open("b.snot", None);
    let refs = |c: &mut Client, uri: &str, line, character| {
        let r = c.position("textDocument/references", uri, line, character);
        c.at(&r)
    };
    // Anywhere else: the links to the note, anchored or not.
    assert_eq!(
        refs(&mut c, &b, 1, 0),
        [
            at("a.snot", 1, 0, 9),
            at("a.snot", 1, 33, 38),
            at("b.snot", 2, 5, 14),
            at("sub/c.snot", 0, 0, 5),
            at("sub/c.snot", 0, 6, 14),
        ]
    );
    // A heading: the links to it.
    assert_eq!(
        refs(&mut c, &b, 4, 2),
        [at("a.snot", 1, 0, 9), at("b.snot", 2, 5, 14)]
    );
    // An `@id`: the links to its scope.
    assert_eq!(refs(&mut c, &b, 5, 10), [at("sub/c.snot", 0, 6, 14)]);
    // A link: what the link points at, here the heading `## Two`.
    assert_eq!(
        refs(&mut c, &a, 1, 3),
        [at("a.snot", 1, 0, 9), at("b.snot", 2, 5, 14)]
    );
    // A same-note anchor.
    assert_eq!(refs(&mut c, &a, 1, 12), [at("a.snot", 1, 10, 18)]);
    // Another token: every token with its key.
    assert_eq!(
        refs(&mut c, &a, 2, 13),
        [at("a.snot", 2, 12, 19), at("sub/c.snot", 0, 15, 22)]
    );
    assert_eq!(
        refs(&mut c, &a, 2, 3),
        [at("a.snot", 2, 2, 11), at("b.snot", 4, 7, 16)]
    );

    // With the declaration.
    let r = c.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": b },
            "position": { "line": 5, "character": 10 },
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(
        c.at(&r),
        [at("b.snot", 5, 0, 13), at("sub/c.snot", 0, 6, 14)]
    );
}

#[test]
fn lists_document_symbols() {
    let mut c = linked();
    let b = c.open(
        "b.snot",
        Some("# One @x\n- a @id:first\n## Two\n### Three @id:three\n# Four\n"),
    );
    let symbols = c.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": b } }),
    );
    fn tree(v: &Value) -> Vec<Value> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| {
                let children = s.get("children").map(tree).unwrap_or_default();
                json!([s["name"], s["detail"], s["range"]["end"]["line"], children])
            })
            .collect()
    }
    assert_eq!(
        tree(&symbols),
        [
            json!([
                "One",
                null,
                3,
                [
                    ["first", "item", 1, []],
                    ["Two", null, 3, [["Three", "#three", 3, []]]],
                ]
            ]),
            json!(["Four", null, 4, []]),
        ]
    );
}

#[test]
fn searches_workspace_symbols() {
    let mut c = linked();
    let search = |c: &mut Client, query: &str| -> Vec<(String, String)> {
        let found = c.request("workspace/symbol", json!({ "query": query }));
        found
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["name"].as_str().unwrap().to_owned(),
                    s["containerName"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let pairs = |v: &[(&str, &str)]| -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    };
    assert_eq!(
        search(&mut c, "@due"),
        pairs(&[("@due:2026", "a"), ("@due:2027", "b")])
    );
    assert_eq!(search(&mut c, "@due:2027"), pairs(&[("@due:2027", "b")]));
    assert_eq!(
        search(&mut c, "@urgent:true"),
        pairs(&[("@urgent", "a"), ("@urgent", "sub/c")])
    );
    assert_eq!(search(&mut c, "TW"), pairs(&[("Two", "b")]));
    assert_eq!(search(&mut c, "own"), pairs(&[("Own", "a"), ("#own", "a")]));
}

#[test]
fn answers_backlinks_and_tags() {
    let mut c = linked();
    let r = c.request("snot/backlinks", json!({ "note": "b" }));
    // Not the link from `b` to itself.
    assert_eq!(
        c.at(&r),
        [
            at("a.snot", 1, 0, 9),
            at("a.snot", 1, 33, 38),
            at("sub/c.snot", 0, 0, 5),
            at("sub/c.snot", 0, 6, 14)
        ]
    );
    assert_eq!(c.request("snot/tags", Value::Null), json!(["urgent"]));

    // An unsaved buffer counts, until it's closed.
    let d = c.open("d.snot", Some("[[b]] @draft\n"));
    let r = c.request("snot/backlinks", json!({ "note": "b" }));
    assert_eq!(c.at(&r).len(), 5);
    assert_eq!(
        c.request("snot/tags", Value::Null),
        json!(["draft", "urgent"])
    );
    c.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": d } }),
    );
    assert_eq!(c.request("snot/tags", Value::Null), json!(["urgent"]));

    // Notes written outside the editor are seen.
    write(c.root(), &[("e.snot", "[[b]] @new\n")]);
    fs::remove_file(c.root().join("sub/c.snot")).unwrap();
    assert_eq!(
        c.request("snot/tags", Value::Null),
        json!(["new", "urgent"])
    );
    let r = c.request("snot/backlinks", json!({ "note": "b" }));
    assert_eq!(c.at(&r).len(), 3);
}

#[test]
fn completes_links_keys_and_values() {
    let mut c = linked();
    let d = c.open(
        "d.snot",
        Some("see [[su\n[[b#t\nx @u\n@due:2\n`@du`\n@id:\n"),
    );
    let complete = |c: &mut Client, line, character| {
        let r = c.request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": d },
                "position": { "line": line, "character": character },
                "context": { "triggerKind": 1 },
            }),
        );
        if r.is_null() {
            return vec![];
        }
        r.as_array()
            .unwrap()
            .iter()
            .map(|i| {
                let label = i["label"].as_str().unwrap().to_owned();
                let detail = i["detail"].as_str().map(str::to_owned);
                let r = &i["textEdit"]["range"];
                assert_eq!(i["textEdit"]["newText"], label.as_str());
                assert_eq!(r["start"]["line"], line);
                assert_eq!(r["end"]["character"], character);
                (label, detail, r["start"]["character"].as_u64().unwrap())
            })
            .collect()
    };
    let item = |label: &str, detail: Option<&str>, start| {
        (label.to_owned(), detail.map(str::to_owned), start)
    };

    // Note paths, replacing what follows `[[`, with each note's first heading.
    assert_eq!(
        complete(&mut c, 0, 8),
        [
            item("a", Some("Own"), 6),
            item("b", Some("One"), 6),
            item("d", None, 6),
            item("sub/c", None, 6)
        ]
    );
    // Anchors: `@id`s first, as they resolve first.
    assert_eq!(
        complete(&mut c, 1, 5),
        [
            item("it", Some("item"), 4),
            item("one", Some("One"), 4),
            item("two", Some("Two"), 4)
        ]
    );
    // Keys, but not the half-typed `@u` itself.
    let labels = |items: Vec<(String, Option<String>, u64)>| {
        items.into_iter().map(|(l, _, _)| l).collect::<Vec<_>>()
    };
    assert_eq!(labels(complete(&mut c, 2, 4)), ["due", "id", "urgent"]);
    // Values given to the key elsewhere.
    assert_eq!(
        complete(&mut c, 3, 6),
        [item("2026", None, 5), item("2027", None, 5)]
    );
    // Nothing in code, or for `@id`.
    assert!(complete(&mut c, 4, 4).is_empty());
    assert!(complete(&mut c, 5, 4).is_empty());
}

#[test]
fn hovers_on_links() {
    let mut c = linked();
    let a = c.open("a.snot", None);
    let hover = |c: &mut Client, uri: &str, line, character| {
        let h = c.position("textDocument/hover", uri, line, character);
        if h.is_null() {
            return None;
        }
        assert_eq!(h["contents"]["kind"], "markdown");
        Some(h["contents"]["value"].as_str().unwrap().to_owned())
    };
    // An anchored heading, with its metadata.
    assert_eq!(
        hover(&mut c, &a, 1, 2).as_deref(),
        Some("**Two** · `b`\n\n`@due:2027`")
    );
    let h = c.position("textDocument/hover", &a, 1, 2);
    assert_eq!(
        h["range"],
        json!({ "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 9 } })
    );
    // An anchor in this file.
    assert_eq!(
        hover(&mut c, &a, 1, 12).as_deref(),
        Some("**Own**\n\n`@id:own`")
    );
    // A note as a whole: its first heading.
    assert_eq!(hover(&mut c, &a, 1, 35).as_deref(), Some("**One** · `b`"));
    // Nothing on a file link, or off a link.
    assert_eq!(hover(&mut c, &a, 1, 22), None);
    assert_eq!(hover(&mut c, &a, 0, 3), None);

    // An anchored item shows its line.
    let sub = c.open("sub/c.snot", None);
    assert_eq!(
        hover(&mut c, &sub, 0, 8).as_deref(),
        Some("`- item @id:it` · `b`\n\n`@id:it`")
    );
}

/// A client that can rename files, over notes linking to each other.
fn renaming() -> Client {
    Client::start_with(
        &[
            ("a.snot", "# A @id:top\n[[#top]] [[b]] [[b#sec]]\n"),
            ("b.snot", "# B\n## Sec @id:sec\n[[b#sec]] [[a#top]]\n"),
        ],
        json!({
            "general": { "positionEncodings": ["utf-8"] },
            "workspace": {
                "workspaceEdit": { "documentChanges": true, "resourceOperations": ["rename"] },
            },
        }),
    )
}

/// A workspace edit's operations: `(file, version, [(line, start, end, text)])`
/// for edits, `("rename", old, new)` for renames.
fn operations(c: &Client, edit: &Value) -> Vec<Value> {
    let root = format!("{}/", url(c.root()));
    let rel = |v: &Value| v.as_str().unwrap().strip_prefix(&root).unwrap().to_owned();
    edit["documentChanges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op| {
            if op["kind"] == "rename" {
                return json!(["rename", rel(&op["oldUri"]), rel(&op["newUri"])]);
            }
            let edits: Vec<Value> = op["edits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    let r = &e["range"];
                    assert_eq!(r["start"]["line"], r["end"]["line"]);
                    json!([
                        r["start"]["line"],
                        r["start"]["character"],
                        r["end"]["character"],
                        e["newText"]
                    ])
                })
                .collect();
            json!([
                rel(&op["textDocument"]["uri"]),
                op["textDocument"]["version"],
                edits
            ])
        })
        .collect()
}

fn rename(
    c: &mut Client,
    uri: &str,
    line: u32,
    character: u32,
    name: &str,
) -> Result<Value, String> {
    c.try_request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "newName": name,
        }),
    )
}

#[test]
fn renames_notes() {
    let mut c = renaming();
    let a = c.open("a.snot", None);
    let prepared = c.position("textDocument/prepareRename", &a, 1, 11);
    assert_eq!(
        prepared,
        json!({
            "range": { "start": { "line": 1, "character": 11 }, "end": { "line": 1, "character": 12 } },
            "placeholder": "b",
        })
    );
    // Every link to the note, then the file itself; the note's own links too.
    let edit = rename(&mut c, &a, 1, 11, "notes/bee.snot").unwrap();
    assert_eq!(
        operations(&c, &edit),
        [
            json!([
                "a.snot",
                1,
                [[1, 11, 12, "notes/bee"], [1, 17, 18, "notes/bee"]]
            ]),
            json!(["b.snot", null, [[2, 2, 3, "notes/bee"]]]),
            json!(["rename", "b.snot", "notes/bee.snot"]),
        ]
    );
    // Off a link, the note the cursor is in.
    let edit = rename(&mut c, &a, 0, 1, "z").unwrap();
    assert_eq!(
        operations(&c, &edit),
        [
            json!(["b.snot", null, [[2, 12, 13, "z"]]]),
            json!(["rename", "a.snot", "z.snot"]),
        ]
    );

    for (name, error) in [
        ("a", "`a` already exists"),
        (
            "x.png",
            "can't rename to `x.png`: links would read a name with an extension as a file",
        ),
        ("../x", "can't rename to `../x`: empty path segment or `..`"),
    ] {
        assert_eq!(rename(&mut c, &a, 1, 11, name), Err(error.to_owned()));
    }

    // Not when the editor can't rename files.
    let mut c = linked();
    let a = c.open("a.snot", None);
    assert_eq!(
        rename(&mut c, &a, 1, 35, "x"),
        Err("the editor can't rename files through the language server".to_owned())
    );
}

#[test]
fn renames_ids() {
    let mut c = renaming();
    let a = c.open("a.snot", None);
    // From a link's anchor, or the token itself: the token and every link
    // anchored on it, here and in other notes.
    let expected = [
        json!(["a.snot", 1, [[0, 8, 11, "head"], [1, 3, 6, "head"]]]),
        json!(["b.snot", null, [[2, 14, 17, "head"]]]),
    ];
    for (line, character) in [(1, 4), (0, 9)] {
        let prepared = c.position("textDocument/prepareRename", &a, line, character);
        assert_eq!(prepared["placeholder"], "top");
        let edit = rename(&mut c, &a, line, character, "head").unwrap();
        assert_eq!(operations(&c, &edit), expected);
    }
    // An anchor in another note.
    let edit = rename(&mut c, &a, 1, 20, "part").unwrap();
    assert_eq!(
        operations(&c, &edit),
        [
            json!(["a.snot", 1, [[1, 19, 22, "part"]]]),
            json!(["b.snot", null, [[1, 11, 14, "part"], [2, 4, 7, "part"]]]),
        ]
    );

    assert_eq!(
        rename(&mut c, &a, 1, 4, "1x"),
        Err(
            "can't rename to `1x`: an `@id` is a letter, then letters, digits, `-` or `_`"
                .to_owned()
        )
    );
    let b = c.open(
        "b.snot",
        Some("# B\n## Sec @id:sec\n[[b#sec]] [[a#top]] [[#b]]\n"),
    );
    assert_eq!(
        rename(&mut c, &b, 2, 23, "x"),
        Err("`#b` isn't an `@id`; edit the heading to change its anchor".to_owned())
    );
    c.change(&a, 2, "# A @id:top\n## Two @id:two\n");
    assert_eq!(
        rename(&mut c, &a, 0, 9, "two"),
        Err("`@id:two` is already used in this note".to_owned())
    );
}

#[test]
fn counts_columns_in_utf16_unless_offered_utf8() {
    let mut c = Client::start_with(&[("b.snot", "")], json!({}));
    let a = c.open("a.snot", Some("é𝄞 [[b]] [[nope]]\n"));
    assert_eq!(c.codes(&a), ["L003"]);
    assert_eq!(c.diagnostics[&a][0]["range"]["start"]["character"], 12);
    let d = c.position("textDocument/definition", &a, 0, 6);
    assert_eq!(c.at(&json!([d])), [at("b.snot", 0, 0, 0)]);

    let mut c = Client::start(&[("b.snot", "")]);
    let a = c.open("a.snot", Some("é𝄞 [[b]] [[nope]]\n"));
    assert_eq!(c.codes(&a), ["L003"]);
    assert_eq!(c.diagnostics[&a][0]["range"]["start"]["character"], 15);
}

#[test]
fn watches_notes_when_the_client_can() {
    let mut c = Client::start_with(
        &[],
        json!({ "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } } }),
    );
    c.request("snot/tags", Value::Null);
    assert_eq!(c.server_requests, ["client/registerCapability"]);
    let c = Client::start(&[]);
    assert_eq!(c.server_requests, Vec::<String>::new());
}

fn code_actions(c: &mut Client, uri: &str, line: u32, start: u32, end: u32) -> Value {
    let diagnostics = c.diagnostics.get(uri).cloned().unwrap_or(json!([]));
    c.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": line, "character": start },
                "end": { "line": line, "character": end },
            },
            "context": { "diagnostics": diagnostics },
        }),
    )
}

#[test]
fn offers_to_create_missing_notes() {
    let mut c = Client::start_with(
        &[("b.snot", "")],
        json!({
            "general": { "positionEncodings": ["utf-8"] },
            "workspace": {
                "workspaceEdit": { "documentChanges": true, "resourceOperations": ["create"] },
            },
        }),
    );
    let a = c.open("a.snot", Some("[[b]] [[sub/new#x]] [[sub/new]] [[a]]\n"));
    assert_eq!(c.codes(&a), ["L003", "L003"]);

    // On a link to a missing note: one action, fixing both its diagnostics.
    let actions = code_actions(&mut c, &a, 0, 8, 8);
    let actions = actions.as_array().unwrap();
    assert_eq!(actions.len(), 1);
    let action = &actions[0];
    assert_eq!(action["title"], "Create note `sub/new`");
    assert_eq!(action["kind"], "quickfix");
    assert_eq!(action["diagnostics"].as_array().unwrap().len(), 2);
    assert_eq!(
        action["edit"]["documentChanges"],
        json!([{
            "kind": "create",
            "uri": c.uri("sub/new.snot"),
            "options": { "overwrite": false, "ignoreIfExists": true },
        }])
    );
    // Not on links to notes that exist, or to the note itself.
    assert_eq!(code_actions(&mut c, &a, 0, 1, 1), json!([]));
    assert_eq!(code_actions(&mut c, &a, 0, 34, 34), json!([]));

    // Not when the editor can't create files.
    let mut c = Client::start(&[]);
    let a = c.open("a.snot", Some("[[new]]\n"));
    assert_eq!(code_actions(&mut c, &a, 0, 2, 2), Value::Null);
}
