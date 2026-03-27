pub mod node;
pub mod prelude;
pub mod syntax;

use chumsky::prelude::*;
use prelude::*;

pub struct OmegaParser;

impl OmegaParser {
    pub fn parse_module(source_code: &str) -> Result<Vec<RootStatement>, Vec<Rich<'_, char>>> {
        RootStatement::parser()
            .repeated()
            .collect::<Vec<_>>()
            .parse(source_code)
            .into_result()
    }
}
