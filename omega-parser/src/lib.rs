pub mod macros;
pub mod prelude;
pub mod syntax;

use chumsky::prelude::*;
use prelude::*;

#[derive(Debug, Clone)]
pub struct SourceModule {
    pub nodes: Vec<RootStatementNode>,
}

impl SourceModule {
    pub fn parse(source_code: &str) -> Result<Self, Vec<Rich<'_, char>>> {
        let nodes = RootStatementNode::parser()
            .repeated()
            .collect::<Vec<_>>()
            .parse(source_code)
            .into_result()?;

        Ok(Self { nodes })
    }
}
