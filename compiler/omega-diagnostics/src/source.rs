use std::rc::Rc;

pub(crate) const TAB_WIDTH: usize = 4;

pub(crate) fn display_column(text: &str, byte_offset: usize) -> usize {
    let mut end = byte_offset.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end]
        .chars()
        .map(|ch| if ch == '\t' { TAB_WIDTH } else { 1 })
        .sum()
}

/// Compilation-local identity of one retained source file. It is opaque on
/// purpose: a diagnostic consumer may compare and render ids, but only the
/// registry that handed them out can turn one back into text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(u32);

/// The retained source text of one compilation, addressed by `SourceId`.
///
/// Rendering resolves every label through this registry, which is what makes
/// a diagnostic whose labels live in different files representable without
/// ever interpreting one file's byte offsets against another's.
#[derive(Default)]
pub struct SourceRegistry {
    files: Vec<Rc<SourceFile>>,
}

impl SourceRegistry {
    pub fn add(&mut self, file: SourceFile) -> SourceId {
        let id = SourceId(self.files.len() as u32);
        self.files.push(Rc::new(file));
        id
    }

    pub fn get(&self, id: SourceId) -> Option<&SourceFile> {
        self.files.get(id.0 as usize).map(Rc::as_ref)
    }

    pub fn shared(&self, id: SourceId) -> Option<Rc<SourceFile>> {
        self.files.get(id.0 as usize).cloned()
    }
}

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
        Self {
            name: name.into(),
            source,
            line_starts,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let mut offset = offset.min(self.source.len());
        while !self.source.is_char_boundary(offset) {
            offset -= 1;
        }
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(exact) => exact,
            Err(insert_at) => insert_at - 1,
        };
        let line_start = self.line_starts[line_idx];
        let column = display_column(&self.source[line_start..offset], offset - line_start) + 1;
        (line_idx + 1, column)
    }

    pub fn line_of(&self, offset: usize) -> usize {
        self.line_col(offset).0
    }

    pub fn line_text(&self, line: usize) -> &str {
        let Some(&start) = self.line_starts.get(line.wrapping_sub(1)) else {
            return "";
        };
        let end = self
            .line_starts
            .get(line)
            .map(|&next| next.saturating_sub(1))
            .unwrap_or(self.source.len());
        self.source[start..end.max(start)].trim_end_matches('\r')
    }

    pub(crate) fn line_start(&self, line: usize) -> usize {
        self.line_starts
            .get(line.wrapping_sub(1))
            .copied()
            .unwrap_or(self.source.len())
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
    fn tab_columns_match_rendered_width() {
        let f = SourceFile::new("t", "\tvalue");
        assert_eq!(f.line_col(1), (1, TAB_WIDTH + 1));
    }

    #[test]
    fn multibyte_columns_count_chars() {
        let f = SourceFile::new("t", "é = 1;");
        assert_eq!(f.line_col(3), (1, 3));
    }

    #[test]
    fn line_col_tolerates_offsets_inside_multibyte_characters() {
        let f = SourceFile::new("t", "éx");
        assert_eq!(f.line_col(1), (1, 1));
    }

    #[test]
    fn registry_ids_address_distinct_files() {
        let mut registry = SourceRegistry::default();
        let a = registry.add(SourceFile::new("a.omg", "a"));
        let b = registry.add(SourceFile::new("b.omg", "bb"));
        assert_ne!(a, b);
        assert_eq!(registry.get(a).unwrap().name(), "a.omg");
        assert_eq!(registry.get(b).unwrap().source(), "bb");
    }
}
