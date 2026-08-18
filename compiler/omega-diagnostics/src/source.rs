pub struct SourceFile {
    name: String,
    source: String,
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

    pub fn line_of(&self, offset: usize) -> usize {
        self.line_col(offset).0
    }

    pub fn line_text(&self, line: usize) -> &str {
        let Some(&start) = self.line_starts.get(line.wrapping_sub(1)) else { return "" };
        let end = self
            .line_starts
            .get(line)
            .map(|&next| next.saturating_sub(1))
            .unwrap_or(self.source.len());
        self.source[start..end.max(start)].trim_end_matches('\r')
    }

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
        assert_eq!(f.line_col(3), (1, 3));
    }
}
