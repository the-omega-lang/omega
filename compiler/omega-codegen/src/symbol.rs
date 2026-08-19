use omega_hir::HirId;
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct SymbolRegistry {
    owners: HashMap<String, HirId>,
}

impl SymbolRegistry {
    pub(crate) fn register_function(&mut self, symbol: &str, owner: HirId) -> Result<(), String> {
        if let Some(existing) = self.owners.get(symbol) {
            if *existing == owner {
                return Ok(());
            }
            return Err(format!(
                "two different functions both produce the linker symbol '{symbol}' -- this can \
                 happen when '@mangling(disabled)' is used on more than one function with the same name, \
                 or when '@mangling(force = \"...\")' gives two different functions the same forced name; \
                 give one of them a different name, or re-enable mangling"
            ));
        }

        self.owners.insert(symbol.to_owned(), owner);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omega_hir::ModuleId;

    fn id(local: u32) -> HirId {
        HirId {
            module: ModuleId(0),
            local,
        }
    }

    #[test]
    fn same_owner_can_register_same_symbol_again() {
        let mut registry = SymbolRegistry::default();
        registry.register_function("f", id(0)).unwrap();
        registry.register_function("f", id(0)).unwrap();
    }

    #[test]
    fn different_owners_cannot_share_a_symbol() {
        let mut registry = SymbolRegistry::default();
        registry.register_function("f", id(0)).unwrap();
        let error = registry.register_function("f", id(1)).unwrap_err();
        assert!(error.contains("two different functions"));
        assert!(error.contains("f"));
    }
}
