pub mod body;
pub mod ids;
mod lower;
pub mod mangle;
pub mod mir;

pub use body::*;
pub use ids::{BlockId, LocalId};
pub use lower::lower_program;
pub use mir::*;
