pub mod hir;
pub mod ids;
pub mod lower;

pub use hir::*;
pub use ids::{HirId, ModuleId, SYNTHETIC_MODULE};
pub use lower::lower_module;
