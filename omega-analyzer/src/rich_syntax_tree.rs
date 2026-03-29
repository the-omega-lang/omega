use crate::context::{Context, ScopeContext};
use omega_parser::{NodeId, SourceModule, prelude::Type};
use std::collections::HashMap;

pub struct Module {
    pub expression_types: HashMap<NodeId, Type>,
    pub global_context: Context,
    pub block_context: HashMap<NodeId, ScopeContext>,
}

impl TryFrom<SourceModule> for Module {
    type Error = String;
    fn try_from(source_module: SourceModule) -> Result<Self, Self::Error> {
        Err(String::new())
    }
}
