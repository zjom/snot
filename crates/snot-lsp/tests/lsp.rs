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
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        let req = Request::new(id.clone(), method.to_owned(), params);
        self.conn.sender.send(req.into()).unwrap();
        loop {
            match self.conn.receiver.recv().unwrap() {
                Message::Response(r) if r.id == id => match r.response_result {
                    Ok(result) => return result,
                    Err(e) => panic!("{method}: {e:?}"),
                },
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
