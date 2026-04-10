pub mod prelude;
pub mod syntax;

use std::cell::RefCell;

use chumsky::prelude::*;
use prelude::*;

pub type NodeId = u64;
fn next_node_id() -> NodeId {
    thread_local! {
        static LOCAL_ID_COUNTER: RefCell<NodeId> = RefCell::new(1);
    }

    LOCAL_ID_COUNTER.with(|counter| {
        let current = *counter.borrow();
        *counter.borrow_mut() += 1;
        current
    })
}

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
