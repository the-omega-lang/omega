/// One source file's text plus its precomputed newline offsets, so
/// translating a byte offset into a 1-based `(line, column)` pair is a
/// binary search rather than a re-scan of the whole file for every
/// diagnostic. Columns count Unicode scalar values (`char`s), not bytes or
/// grapheme clusters -- good enough for a terminal-caret-style renderer
/// without pulling in a full grapheme-segmentation dependency.
///
/// `name` is display-only (e.g. `"examples/dev/main.omg"`) -- whatever the
/// caller wants printed in a diagnostic's `--> name:line:col` line; this
/// crate never touches the filesystem itself.
pub struct SourceFile {
    name: String,
    source: String,
    /// Byte offset of the start of each line; `line_starts[0]` is always 0.
    line_starts: Vec<usize>,
}

impl SourceFile {
    pub fn new(name: impl Into<String>, source: impl Into<String>) -> Self {
        let source = source.into();
        let mut line_starts = vec![0];
        for (i, c) in source.char_indices() {
            if c == '\n' {
                line_starts.push(i + 1);
            }
        }
        Self { name: name.into(), source, line_starts }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// 1-based `(line, column)` for a byte offset. Offsets past the end of
    /// the source clamp to the last line/column rather than panicking --
    /// diagnostics can legitimately point just past EOF (e.g. "expected `}`
    /// but found end of input").
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.source.len());
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(exact) => exact,
            Err(insert_at) => insert_at - 1,
        };
        let line_start = self.line_starts[line_idx];
        let column = self.source[line_start..offset].chars().count() + 1;
        (line_idx + 1, column)
    }

    /// The 1-based line a byte offset falls on.
    pub fn line_of(&self, offset: usize) -> usize {
        self.line_col(offset).0
    }

    /// The 1-based `line`'s own text, without its trailing newline -- for a
    /// diagnostic snippet. Empty string for an out-of-range line.
    pub fn line_text(&self, line: usize) -> &str {
        let Some(&start) = self.line_starts.get(line.wrapping_sub(1)) else { return "" };
        let end = self
            .line_starts
            .get(line)
            .map(|&next| next.saturating_sub(1))
            .unwrap_or(self.source.len());
        self.source[start..end.max(start)].trim_end_matches('\r')
    }

    /// Byte offset where the 1-based `line` starts -- what the renderer uses
    /// to translate a whole-file byte span into a column within one line's
    /// text. Clamps an out-of-range line to end of source.
    pub(crate) fn line_start(&self, line: usize) -> usize {
        self.line_starts.get(line.wrapping_sub(1)).copied().unwrap_or(self.source.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_basics() {
        let f = SourceFile::new("t", "ab\ncd\n");
        assert_eq!(f.line_col(0), (1, 1));
        assert_eq!(f.line_col(1), (1, 2));
        assert_eq!(f.line_col(3), (2, 1));
        // Past EOF clamps to just after the last character.
        assert_eq!(f.line_col(100), (3, 1));
    }

    #[test]
    fn line_text_strips_newline_and_cr() {
        let f = SourceFile::new("t", "ab\r\ncd");
        assert_eq!(f.line_text(1), "ab");
        assert_eq!(f.line_text(2), "cd");
        assert_eq!(f.line_text(3), "");
    }

    #[test]
    fn multibyte_columns_count_chars() {
        let f = SourceFile::new("t", "é = 1;");
        // 'é' is 2 bytes; the '=' at byte 2 is display column 3.
        assert_eq!(f.line_col(3), (1, 3));
    }
}
