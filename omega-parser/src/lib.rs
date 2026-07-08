pub mod ast;
pub mod diagnostics;
pub mod lexer;
pub mod macros;
pub mod parser;
pub mod prelude;

use diagnostics::ParseError;
use prelude::*;

#[derive(Debug, Clone)]
pub struct SourceModule {
    pub nodes: Vec<ItemNode>,
}

impl SourceModule {
    pub fn parse(source_code: &str) -> Result<Self, Vec<ParseError>> {
        let (tokens, lex_errors) = lexer::tokenize(source_code);
        let mut parser = parser::Parser::new(&tokens);
        let nodes = parser::item::parse_source_module(&mut parser);

        let mut errors = lex_errors;
        errors.extend(parser.into_errors());
        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(Self { nodes })
    }
}
