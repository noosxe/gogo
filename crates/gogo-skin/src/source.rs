//! Source files and spans: where in the package a finding points.

use std::sync::Arc;

/// A byte range in one source file of the package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    /// Index into [`Sources`].
    pub file: usize,
    /// Byte offset of the first byte.
    pub offset: usize,
    /// Length in bytes.
    pub len: usize,
}

impl Span {
    pub const fn new(file: usize, offset: usize, len: usize) -> Self {
        Span { file, offset, len }
    }

    /// The part of this span starting `by` bytes in, `len` bytes long.
    pub const fn inner(self, by: usize, len: usize) -> Self {
        Span {
            file: self.file,
            offset: self.offset.saturating_add(by),
            len,
        }
    }
}

/// One KDL file of the package.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// The path relative to the skin root, with `/` separators: `routes/game.kdl`.
    pub path: String,
    pub text: Arc<str>,
}

/// The KDL files of a package, in the order they were read. A [`Span`]'s `file` indexes
/// them.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    files: Vec<SourceFile>,
}

impl Sources {
    pub(crate) fn add(&mut self, path: &str, text: &str) -> usize {
        self.files.push(SourceFile {
            path: path.to_owned(),
            text: Arc::from(text),
        });
        self.files.len() - 1
    }

    pub fn get(&self, file: usize) -> Option<&SourceFile> {
        self.files.get(file)
    }

    pub fn iter(&self) -> impl Iterator<Item = &SourceFile> {
        self.files.iter()
    }

    /// The text a span covers, if it is in range and on character boundaries.
    pub fn slice(&self, span: Span) -> Option<&str> {
        let f = self.files.get(span.file)?;
        f.text.get(span.offset..span.offset.checked_add(span.len)?)
    }

    /// `path:line:column` (1-based; the column counts characters) of a span's start.
    pub fn location(&self, span: Span) -> String {
        let Some(f) = self.files.get(span.file) else {
            return String::new();
        };
        let mut end = span.offset.min(f.text.len());
        while !f.text.is_char_boundary(end) {
            end -= 1;
        }
        let before = &f.text[..end];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        format!("{}:{line}:{col}", f.path)
    }
}
