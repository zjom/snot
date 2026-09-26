//! Link targets (NOTE_SPEC 7.2).

use crate::{Code, Target};

/// `scheme://`: a letter, then letters, digits, `+`, `-` or `.`.
fn is_url(target: &str) -> bool {
    let b = target.as_bytes();
    if !b.first().is_some_and(u8::is_ascii_alphabetic) {
        return false;
    }
    let scheme = b
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
        .count();
    b[scheme..].starts_with(b"://")
}

/// What a trimmed, unescaped target points at, and what's wrong with it.
pub(crate) fn classify(target: &str) -> (Target, Option<(Code, String)>) {
    if is_url(target) {
        return (Target::Url(target.to_owned()), None);
    }
    if let Some(anchor) = target.strip_prefix('#') {
        let problem = anchor
            .is_empty()
            .then(|| (Code::InvalidLink, "empty anchor".to_owned()));
        return (Target::Anchor(anchor.to_owned()), problem);
    }

    let (path, anchor) = match target.split_once('#') {
        Some((path, anchor)) => (path, Some(anchor.to_owned())),
        None => (target, None),
    };
    let last = path.rsplit('/').next().unwrap_or(path);
    let has_extension = last.char_indices().any(|(i, c)| c == '.' && i > 0);

    let problem = if path.starts_with('/') {
        Some("link paths are relative to the notes root and must not start with `/`")
    } else if path.split('/').any(|s| s == "..") {
        Some("link paths must not contain `..`")
    } else if path.split('/').any(str::is_empty) {
        Some("empty path segment")
    } else if anchor.as_deref() == Some("") {
        Some("empty anchor")
    } else {
        None
    }
    .map(|m| (Code::InvalidLink, m.to_owned()));

    if has_extension {
        let problem = problem.or_else(|| {
            anchor.is_some().then(|| {
                (
                    Code::AnchorOnFile,
                    "only links to notes can have an anchor".to_owned(),
                )
            })
        });
        let path = path.to_owned();
        (Target::File { path, anchor }, problem)
    } else {
        let path = path.to_owned();
        (Target::Note { path, anchor }, problem)
    }
}
