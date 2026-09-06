use crate::symbol::SymbolRegistry;
use omega_analyzer::checked::ExternFunctionRef;
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_hir::HirId;
use omega_mir::{EmissionUnit, MirFunctionDef, MirItem};
use std::collections::HashSet;

/// A function any emitted object may reference. Every unit declares it; only
/// the owning unit attaches a body and owner linkage.
pub(crate) struct FunctionDecl {
    pub(crate) id: HirId,
    pub(crate) symbol: String,
    pub(crate) fn_type: ResolvedFunctionType,
}

/// A data symbol any emitted object may reference. Only the source that
/// declares it gives it storage; a foreign binding's storage lives in another
/// object entirely and is never emitted here at all.
pub(crate) struct GlobalDecl {
    pub(crate) id: HirId,
    pub(crate) symbol: String,
    pub(crate) r#type: ResolvedType,
}

/// The compilation-wide set of names the emitted objects can refer to across
/// each other, built once and shared unchanged by every unit. It deliberately
/// carries no bodies: a unit needs types and symbols to declare a reference,
/// not the definition it refers to.
pub(crate) struct Catalog {
    pub(crate) functions: Vec<FunctionDecl>,
    pub(crate) globals: Vec<GlobalDecl>,
}

impl Catalog {
    /// Collision checking happens here, above the per-unit LLVM modules: two
    /// definitions that force the same linker symbol are rejected the same way
    /// whether or not they share a source file.
    pub(crate) fn build(
        units: &[EmissionUnit],
        extern_functions: &[ExternFunctionRef],
    ) -> Result<Self, String> {
        let mut catalog = Self {
            functions: Vec::new(),
            globals: Vec::new(),
        };
        let mut symbols = SymbolRegistry::default();

        for unit in units {
            for item in &unit.items {
                catalog.collect_item(item, &mut symbols)?;
            }
        }
        for extern_fn in extern_functions {
            catalog.functions.push(FunctionDecl {
                id: extern_fn.decl_id,
                symbol: omega_mir::mangle::extern_function_ref_symbol(extern_fn),
                fn_type: extern_fn.fn_type.clone(),
            });
        }
        Ok(catalog)
    }

    /// The globals one unit initializes itself, so its own declaration pass can
    /// leave them to the owner pass, which knows their initializer's type.
    pub(crate) fn owned_globals(unit: &EmissionUnit) -> HashSet<HirId> {
        unit.items
            .iter()
            .filter_map(|item| match item {
                MirItem::Declaration(declaration) => Some(declaration.id),
                _ => None,
            })
            .collect()
    }

    fn collect_item(&mut self, item: &MirItem, symbols: &mut SymbolRegistry) -> Result<(), String> {
        match item {
            MirItem::Declaration(declaration) => {
                symbols.register(&declaration.symbol, declaration.id)?;
                self.globals.push(GlobalDecl {
                    id: declaration.id,
                    symbol: declaration.symbol.clone(),
                    r#type: declaration.r#type.clone(),
                });
            }
            // A gap declaration and its glue definition intentionally mangle to
            // one symbol under two `HirId`s, so a function-typed binding is
            // registered by whatever defines it, never here.
            MirItem::ForeignBinding(binding) => match &binding.r#type {
                ResolvedType::Function(fn_type) => self.functions.push(FunctionDecl {
                    id: binding.id,
                    symbol: binding.symbol.clone(),
                    fn_type: fn_type.clone(),
                }),
                r#type => {
                    symbols.register(&binding.symbol, binding.id)?;
                    self.globals.push(GlobalDecl {
                        id: binding.id,
                        symbol: binding.symbol.clone(),
                        r#type: r#type.clone(),
                    });
                }
            },
            MirItem::ForeignFunction(function) => {
                symbols.register(&function.symbol, function.id)?;
                self.functions.push(FunctionDecl {
                    id: function.id,
                    symbol: function.symbol.clone(),
                    fn_type: function.fn_type(),
                });
            }
            MirItem::FunctionDefinition(function) => self.collect_function(function, symbols)?,
            MirItem::Struct(definition) => {
                for function in &definition.functions {
                    self.collect_function(function, symbols)?;
                }
            }
            MirItem::Enum(definition) => {
                for function in &definition.functions {
                    self.collect_function(function, symbols)?;
                }
            }
            MirItem::Union(definition) => {
                for function in &definition.functions {
                    self.collect_function(function, symbols)?;
                }
            }
        }
        Ok(())
    }

    fn collect_function(
        &mut self,
        function: &MirFunctionDef,
        symbols: &mut SymbolRegistry,
    ) -> Result<(), String> {
        symbols.register_function(&function.symbol, function.id)?;
        self.functions.push(FunctionDecl {
            id: function.id,
            symbol: function.symbol.clone(),
            fn_type: function.fn_type(),
        });
        Ok(())
    }
}
