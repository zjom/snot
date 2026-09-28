//! Converting byte offsets to and from line and column positions.

/// How columns are counted, as a language server client asks for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Bytes of UTF-8.
    Utf8,
    /// UTF-16 code units, the language server protocol's default.
    Utf16,
}

/// A 0-based line and column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    /// The line.
    pub line: u32,
    /// The column, in the index's [`Encoding`].
    pub column: u32,
}

/// Where each line of a text starts, for converting positions. Lines end at
/// `\n`; a `\r` before it counts as part of the line.
#[derive(Clone, Debug)]
pub struct LineIndex {
    starts: Vec<usize>,
    len: usize,
}

impl LineIndex {
    /// Index `text`.
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        LineIndex {
            starts,
            len: text.len(),
        }
    }

    /// The position of byte `offset` in `text`, the text indexed. Offsets
    /// past the end, or inside a character, move back to the nearest one.
    pub fn position(&self, text: &str, offset: usize, encoding: Encoding) -> Position {
        let mut offset = offset.min(self.len);
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = self.starts.partition_point(|&s| s <= offset) - 1;
        let before = &text[self.starts[line]..offset];
        let column = match encoding {
            Encoding::Utf8 => before.len(),
            Encoding::Utf16 => before.encode_utf16().count(),
        };
        Position {
            line: line as u32,
            column: column as u32,
        }
    }

    /// The byte offset of `pos` in `text`, the text indexed. A column past
    /// the end of its line is the line's end; a line past the last is the end
    /// of the text.
    pub fn offset(&self, text: &str, pos: Position, encoding: Encoding) -> usize {
        let Some(&start) = self.starts.get(pos.line as usize) else {
            return self.len;
        };
        let end = self
            .starts
            .get(pos.line as usize + 1)
            .map_or(self.len, |&next| next - 1);
        let line = &text[start..end];
        let mut units = 0;
        for (i, c) in line.char_indices() {
            if units >= pos.column as usize {
                return start + i;
            }
            units += match encoding {
                Encoding::Utf8 => c.len_utf8(),
                Encoding::Utf16 => c.len_utf16(),
            };
        }
        end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, column: u32) -> Position {
        Position { line, column }
    }

    #[test]
    fn converts_both_ways() {
        let text = "ab\r\né𝄞x\n\nz";
        let index = LineIndex::new(text);
        for (offset, utf8, utf16) in [
            (0, pos(0, 0), pos(0, 0)),
            (2, pos(0, 2), pos(0, 2)),
            (4, pos(1, 0), pos(1, 0)),
            (6, pos(1, 2), pos(1, 1)),
            (10, pos(1, 6), pos(1, 3)),
            (12, pos(2, 0), pos(2, 0)),
            (13, pos(3, 0), pos(3, 0)),
            (14, pos(3, 1), pos(3, 1)),
        ] {
            assert_eq!(
                index.position(text, offset, Encoding::Utf8),
                utf8,
                "{offset}"
            );
            assert_eq!(
                index.position(text, offset, Encoding::Utf16),
                utf16,
                "{offset}"
            );
            assert_eq!(index.offset(text, utf8, Encoding::Utf8), offset);
            assert_eq!(index.offset(text, utf16, Encoding::Utf16), offset);
        }
    }

    #[test]
    fn clamps() {
        let text = "é\nb";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, 1, Encoding::Utf8), pos(0, 0));
        assert_eq!(index.position(text, 99, Encoding::Utf8), pos(1, 1));
        assert_eq!(index.offset(text, pos(0, 99), Encoding::Utf16), 2);
        assert_eq!(index.offset(text, pos(9, 0), Encoding::Utf16), 4);
        // Inside the surrogate pair of a character: after it.
        let text = "𝄞";
        assert_eq!(
            LineIndex::new(text).offset(text, pos(0, 1), Encoding::Utf16),
            4
        );
    }
}
