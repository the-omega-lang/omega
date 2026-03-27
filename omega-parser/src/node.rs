use chumsky::span::{SimpleSpan, Span};

pub type NodeId = u64;

pub trait Node {
    fn id() -> NodeId;
    fn span() -> SimpleSpan;
}
