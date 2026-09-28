//! A collection of Simple Note Format notes: its root, configuration and
//! cross-note index.
//!
//! The notes root is the nearest directory up from where you are with a
//! [`CONFIG_FILE`] in it ([`find_root`]). [`Workspace::load`] reads every note
//! under it, skipping hidden and ignored files, and indexes them: each note's
//! parse, the links into each note, and where each metadata key and value is
//! used. Notes are named by their path from the root, with `/` separators and
//! without the extension, as links spell them (NOTE_SPEC 7.2).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{fmt, fs, io};

use rayon::prelude::*;
use serde::Deserialize;
use snot_syntax::{Document, Target, parse};

/// The configuration file, which also marks the notes root.
pub const CONFIG_FILE: &str = ".snot.toml";

/// A collection's settings, from its [`CONFIG_FILE`].
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The note file extension, with its dot.
    pub extension: String,
    /// The column `snot fmt` aligns trailing metadata to.
    pub width: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            extension: ".snot".to_owned(),
            width: 79,
        }
    }
}

impl Config {
    /// The configuration in `root`, or the default if it has no
    /// [`CONFIG_FILE`]. An extension written without its dot gets one.
    pub fn load(root: &Path) -> Result<Config, Error> {
        let path = root.join(CONFIG_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(Error::Io(path, e)),
        };
        let mut config: Config = toml::from_str(&text).map_err(|e| Error::Config(path, e))?;
        if !config.extension.starts_with('.') {
            config.extension.insert(0, '.');
        }
        Ok(config)
    }
}

/// The nearest directory at or above `start` (a file or directory) holding a
/// [`CONFIG_FILE`].
pub fn find_root(start: &Path) -> Option<PathBuf> {
    let start = if start.is_file() {
        start.parent()?
    } else {
        start
    };
    start
        .ancestors()
        .find(|dir| dir.join(CONFIG_FILE).is_file())
        .map(Path::to_path_buf)
}

/// The files under `dir` ending in `extension`, sorted, skipping hidden
/// files and directories and those ignored by `.gitignore` or `.ignore`.
pub fn walk(dir: &Path, extension: &str) -> Result<Vec<PathBuf>, Error> {
    let mut files = Vec::new();
    for entry in ignore::WalkBuilder::new(dir).build() {
        let entry = entry.map_err(|e| Error::Walk(dir.to_path_buf(), e))?;
        let is_file = entry.file_type().is_some_and(|t| t.is_file());
        if is_file && entry.file_name().to_string_lossy().ends_with(extension) {
            files.push(entry.into_path());
        }
    }
    files.sort();
    Ok(files)
}

/// Something that went wrong reading a workspace.
#[derive(Debug)]
pub enum Error {
    /// A file or directory couldn't be read.
    Io(PathBuf, io::Error),
    /// A directory couldn't be walked.
    Walk(PathBuf, ignore::Error),
    /// The configuration file is invalid.
    Config(PathBuf, toml::de::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(path, e) => write!(f, "{}: {e}", path.display()),
            Error::Walk(path, e) => write!(f, "{}: {e}", path.display()),
            Error::Config(path, e) => write!(f, "{}: {e}", path.display()),
        }
    }
}

impl std::error::Error for Error {}

/// A note in the workspace.
#[derive(Clone, Debug)]
pub struct Note {
    /// The note's file.
    pub path: PathBuf,
    /// The note's text.
    pub source: String,
    /// The parse of `source`.
    pub doc: Document,
    /// The file as it was when read, or `None` for a note added with
    /// [`Workspace::insert`], such as an editor's open buffer, which
    /// [`Workspace::refresh`] leaves alone.
    pub stamp: Option<Stamp>,
}

/// A file's modification time and size, to tell when it has changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl Stamp {
    fn of(meta: &fs::Metadata) -> Self {
        Stamp {
            modified: meta.modified().ok(),
            len: meta.len(),
        }
    }
}

/// Read and parse the note at `path`.
fn read_note(path: PathBuf) -> Result<Note, Error> {
    let read = || -> io::Result<_> {
        let stamp = Stamp::of(&fs::metadata(&path)?);
        Ok((fs::read_to_string(&path)?, stamp))
    };
    match read() {
        Ok((source, stamp)) => Ok(Note {
            doc: parse(&source),
            source,
            stamp: Some(stamp),
            path,
        }),
        Err(e) => Err(Error::Io(path, e)),
    }
}

/// A link: the note it is in, and its index in that note's
/// [`Document::links`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LinkRef {
    /// The note the link is in.
    pub note: String,
    /// Index into the note's links.
    pub link: usize,
}

/// A metadata token: the note it is in, and its index in that note's
/// [`Document::tokens`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TokenRef {
    /// The note the token is in.
    pub note: String,
    /// Index into the note's tokens.
    pub token: usize,
}

/// The notes under a root, and indexes across them.
#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
    config: Config,
    notes: BTreeMap<String, Note>,
    /// Note name → the links to it, anchored or not, from other notes and
    /// itself.
    backlinks: HashMap<String, Vec<LinkRef>>,
    /// Key → value → the tokens giving the key that value.
    keys: HashMap<String, BTreeMap<String, Vec<TokenRef>>>,
}

impl Workspace {
    /// An empty workspace.
    pub fn new(root: PathBuf, config: Config) -> Self {
        Workspace {
            root,
            config,
            notes: BTreeMap::new(),
            backlinks: HashMap::new(),
            keys: HashMap::new(),
        }
    }

    /// Read and index every note under `root`, in parallel. Notes that can't
    /// be read are left out and returned as errors.
    pub fn load(root: PathBuf, config: Config) -> Result<(Workspace, Vec<Error>), Error> {
        let files = walk(&root, &config.extension)?;
        let mut ws = Workspace::new(root, config);
        let files = files
            .into_iter()
            .filter_map(|path| Some((ws.name_of(&path)?, path)))
            .collect();
        let errors = ws.read(files);
        Ok((ws, errors))
    }

    /// Bring the notes read from disk up to date: read new and changed note
    /// files, and drop notes whose files are gone. Notes added with
    /// [`insert`](Self::insert) are left alone. Returns the names of the
    /// notes added, changed or dropped, and the files that couldn't be read.
    pub fn refresh(&mut self) -> Result<(Vec<String>, Vec<Error>), Error> {
        let mut seen = HashSet::new();
        let mut stale = Vec::new();
        for path in walk(&self.root, &self.config.extension)? {
            let Some(name) = self.name_of(&path) else {
                continue;
            };
            let current = match self.notes.get(&name) {
                Some(note) if note.stamp.is_none() => true,
                Some(note) => fs::metadata(&path).is_ok_and(|m| note.stamp == Some(Stamp::of(&m))),
                None => false,
            };
            if !current {
                stale.push((name.clone(), path));
            }
            seen.insert(name);
        }
        let gone: Vec<String> = self
            .notes
            .iter()
            .filter(|(name, note)| note.stamp.is_some() && !seen.contains(*name))
            .map(|(name, _)| name.clone())
            .collect();
        for name in &gone {
            self.remove(name);
        }
        let mut changed: Vec<String> = stale.iter().map(|(name, _)| name.clone()).collect();
        let errors = self.read(stale);
        changed.extend(gone);
        Ok((changed, errors))
    }

    /// Read the note with this name from its file again, or drop it if the
    /// file is gone, as when an editor closes it.
    pub fn reload(&mut self, name: &str) -> Result<(), Error> {
        let path = self.path_of(name);
        if !path.is_file() {
            self.remove(name);
            return Ok(());
        }
        let note = read_note(path)?;
        self.insert_parsed(name.to_owned(), note);
        Ok(())
    }

    /// Read and index these notes, in parallel, returning the files that
    /// couldn't be read.
    fn read(&mut self, files: Vec<(String, PathBuf)>) -> Vec<Error> {
        let read: Vec<_> = files
            .into_par_iter()
            .map(|(name, path)| read_note(path).map(|note| (name, note)))
            .collect();
        let mut errors = Vec::new();
        for result in read {
            match result {
                Ok((name, note)) => self.insert_parsed(name, note),
                Err(e) => errors.push(e),
            }
        }
        errors
    }

    /// The notes root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Every note, by name, in name order.
    pub fn notes(&self) -> impl Iterator<Item = (&str, &Note)> {
        self.notes.iter().map(|(name, note)| (name.as_str(), note))
    }

    /// The note with this name.
    pub fn note(&self, name: &str) -> Option<&Note> {
        self.notes.get(name)
    }

    /// The name of the note at `path`: its path from the root without the
    /// extension, with `/` separators. `None` if it isn't a note file under
    /// the root, or its path isn't UTF-8.
    pub fn name_of(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let parts = rel
            .components()
            .map(|c| match c {
                std::path::Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let name = parts.join("/");
        let name = name.strip_suffix(&self.config.extension)?;
        (!name.is_empty() && !name.ends_with('/')).then(|| name.to_owned())
    }

    /// The file a note with this name is in, whether or not it exists.
    pub fn path_of(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}{}", self.config.extension))
    }

    /// True if a link to the file at `path` (relative to the root, as a link
    /// spells it) points at a file that exists.
    pub fn file_exists(&self, path: &str) -> bool {
        self.root.join(path).is_file()
    }

    /// The links to the note with this name.
    pub fn backlinks(&self, name: &str) -> &[LinkRef] {
        self.backlinks.get(name).map_or(&[], Vec::as_slice)
    }

    /// Every value `key` is given, with the tokens giving it, in value order.
    pub fn values(&self, key: &str) -> Option<&BTreeMap<String, Vec<TokenRef>>> {
        self.keys.get(key)
    }

    /// Every key used, in no particular order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }

    /// Add or replace the note with this name, as the language server does
    /// for an open buffer.
    pub fn insert(&mut self, name: String, path: PathBuf, source: String) {
        let doc = parse(&source);
        let note = Note {
            path,
            source,
            doc,
            stamp: None,
        };
        self.insert_parsed(name, note);
    }

    /// Remove the note with this name.
    pub fn remove(&mut self, name: &str) -> Option<Note> {
        let note = self.notes.remove(name)?;
        for target in note_targets(&note.doc) {
            if let Some(refs) = self.backlinks.get_mut(target) {
                refs.retain(|r| r.note != name);
            }
        }
        for t in &note.doc.tokens {
            if let Some(values) = self.keys.get_mut(&t.key) {
                for value in &t.values {
                    if let Some(refs) = values.get_mut(value) {
                        refs.retain(|r| r.note != name);
                        if refs.is_empty() {
                            values.remove(value);
                        }
                    }
                }
                if values.is_empty() {
                    self.keys.remove(&t.key);
                }
            }
        }
        Some(note)
    }

    fn insert_parsed(&mut self, name: String, note: Note) {
        self.remove(&name);
        for (i, link) in note.doc.links.iter().enumerate() {
            if let Target::Note { path, .. } = &link.target {
                self.backlinks
                    .entry(path.clone())
                    .or_default()
                    .push(LinkRef {
                        note: name.clone(),
                        link: i,
                    });
            }
        }
        for (i, t) in note.doc.tokens.iter().enumerate() {
            let values = self.keys.entry(t.key.clone()).or_default();
            for value in &t.values {
                values.entry(value.clone()).or_default().push(TokenRef {
                    note: name.clone(),
                    token: i,
                });
            }
        }
        self.notes.insert(name, note);
    }
}

/// The names of the notes `doc` links to, with repeats.
fn note_targets(doc: &Document) -> impl Iterator<Item = &str> {
    doc.links.iter().filter_map(|l| match &l.target {
        Target::Note { path, .. } => Some(path.as_str()),
        _ => None,
    })
}
