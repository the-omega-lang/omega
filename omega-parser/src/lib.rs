pub mod prelude;
pub mod syntax;

use std::cell::RefCell;

use chumsky::prelude::*;
use prelude::*;

pub type NodeId = u64;
fn next_node_id() -> NodeId {
    thread_local! {
        static THREAD_ID_COUNTER: RefCell<NodeId> = RefCell::new(1);
    }

    THREAD_ID_COUNTER.with(|counter| {
        let current = *counter.borrow();
        *counter.borrow_mut() += 1;
        current
    })
}

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
