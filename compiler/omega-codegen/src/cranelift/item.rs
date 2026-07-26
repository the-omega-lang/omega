//! Declaring and defining every item across every compiled module -- the
//! two-pass sweep (`update_all`) that makes cross-module calls work
//! regardless of import direction.

use super::Codegen;
use crate::mangle;
use cranelift_module::Linkage;
use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::ExternFunctionRef;
use omega_analyzer::resolved_type::ResolvedType;
use omega_mir::MirItem;
use omega_parser::prelude::Ident;

/// `Linkage::Export` (strong) for an ordinary item, `Linkage::Preemptible`
/// (weak) for a generic instantiation -- `Preemptible` maps to a genuine
/// weak ELF/Mach-O/COFF symbol (`cranelift-object`'s `translate_linkage`,
/// `let weak = linkage == Linkage::Preemptible`), empirically confirmed to
/// let a linker silently fold multiple independently-compiled definitions
/// of the *same* symbol name into one, rather than erroring on "multiple
/// definition" the way two strong symbols with the same name always
/// would. Every separate `omgc` invocation that instantiates e.g.
/// `CustomStruct<i32>` still fully regenerates its own copy locally
/// (nothing here skips that -- there is no cross-process build cache),
/// exactly like Rust/C++ generics: the deduplication happens once, at
/// final link time, not at compile time. This is only sound because a
/// generic instantiation's mangled symbol is a pure function of
/// `(module_path, name, type_args)` -- two independent compilations of
/// the exact same instantiation are therefore guaranteed to produce
/// byte-identical bodies under the identical name, which is the actual
/// precondition weak-symbol folding relies on (the linker trusts the
/// name, it doesn't diff the bytes). An ordinary, non-generic symbol
/// keeps strong linkage unconditionally -- two *different* object files
/// defining the same non-generic symbol is always a genuine user error,
/// and should still be a hard link error, not silently tolerated.
fn linkage_for(type_args: &[ResolvedType]) -> Linkage {
    if type_args.is_empty() { Linkage::Export } else { Linkage::Preemptible }
}

impl Codegen {
    /// Declares every function/method/extern in one item -- pass 1 of 2
    /// (see `update_all`).
    fn declare_item(&mut self, item: &MirItem, path: &[Ident], entry: &[Ident]) {
        match item {
            // Externs have no body to split across two passes -- fully
            // handled here, in one shot.
            MirItem::ExternDeclaration(extern_decl) => self.update_extern_decl(extern_decl.clone()),
            MirItem::FunctionDefinition(f) => {
                // A member function can never reach `Disabled` here --
                // `omega_analyzer::annotations::resolve` hard-rejects
                // `@mangling(disabled)` on a method (and on a generic
                // function) before a `CheckedModule` (and therefore the
                // `MirModule` lowered from it) can exist at all, so only a
                // top-level, non-generic function ever gets here with
                // `Disabled`.
                //
                // The program's literal entry point (`main`, in the entry
                // module) keeps the bare, unmangled symbol the OS/linker
                // looks for -- checked here, before a `Symbol` is even
                // built, rather than inside `mangle::free_function_symbol`,
                // which only ever needs to know how to name a real symbol.
                // `main` is never itself generic, so `linkage_for` already
                // gives it `Export`, same as today, with no special case
                // needed beyond the name.
                // `extension_target` -- `Some` for a method attached via
                // `spec Name : Deps for Target { ... }` (see
                // `MirFunctionDef::extension_target`'s doc comment) --
                // mangles like a struct/enum/union method (owned, via
                // `method_symbol`) rather than an ordinary free function:
                // the target's own `Display` stands in for the owner name a
                // primitive doesn't otherwise have, avoiding a collision
                // with an unrelated, same-named, same-`type_args`-shaped
                // free function elsewhere in the same module.
                let mangled = match (f.mangling, &f.extension_target) {
                    (ManglingMode::Disabled, _) => f.name.as_ref().to_string(),
                    (ManglingMode::Enabled, _) if path == entry && f.name.as_ref() == "main" => "main".to_string(),
                    (ManglingMode::Enabled, Some(target)) => {
                        let owner = Ident(target.to_string());
                        mangle::encode(&mangle::method_symbol(path, &owner, &[], &f.name, &f.fn_type()))
                    }
                    (ManglingMode::Enabled, None) => {
                        mangle::encode(&mangle::free_function_symbol(path, &f.name, &f.type_args, &f.fn_type()))
                    }
                };
                self.declare_function_def(f, mangled, linkage_for(&f.type_args));
            }
            MirItem::Struct(s) => {
                for f in &s.functions {
                    let mangled =
                        mangle::encode(&mangle::method_symbol(path, &s.name, &s.type_args, &f.name, &f.fn_type()));
                    self.declare_function_def(f, mangled, linkage_for(&s.type_args));
                }
            }
            MirItem::Enum(e) => {
                for f in &e.functions {
                    let mangled =
                        mangle::encode(&mangle::method_symbol(path, &e.name, &e.type_args, &f.name, &f.fn_type()));
                    self.declare_function_def(f, mangled, linkage_for(&e.type_args));
                }
            }
            MirItem::Union(u) => {
                for f in &u.functions {
                    let mangled =
                        mangle::encode(&mangle::method_symbol(path, &u.name, &u.type_args, &f.name, &f.fn_type()));
                    self.declare_function_def(f, mangled, linkage_for(&u.type_args));
                }
            }
            MirItem::Declaration(_) => {
                todo!("global data declarations are not yet implemented");
            }
        }
    }

    /// Defines every function/method body in one item -- pass 2 of 2, run
    /// only after every item across every module has already been
    /// declared.
    fn define_item(&mut self, item: MirItem) {
        match item {
            // Already fully handled by `declare_item` -- an extern has no
            // body to define.
            MirItem::ExternDeclaration(_) => {}
            MirItem::FunctionDefinition(f) => self.define_function_def(f),
            MirItem::Struct(s) => {
                for f in s.functions {
                    self.define_function_def(f);
                }
            }
            MirItem::Enum(e) => {
                for f in e.functions {
                    self.define_function_def(f);
                }
            }
            MirItem::Union(u) => {
                for f in u.functions {
                    self.define_function_def(f);
                }
            }
            MirItem::Declaration(_) => {
                todo!("global data declarations are not yet implemented");
            }
        }
    }

    /// Two full passes over every item across every compiled module: first
    /// declare everything (so any `FuncId` a cross-module call needs
    /// already exists, regardless of import direction -- see
    /// `declare_function_def`'s doc comment), then define every body.
    /// Mirrors the identical signature/body split `omega_analyzer::
    /// Analyzer` does for the same underlying reason.
    pub(super) fn update_all(
        &mut self,
        modules: Vec<(Vec<Ident>, omega_mir::MirModule)>,
        entry: &[Ident],
        extern_functions: Vec<ExternFunctionRef>,
    ) {
        for (path, module) in &modules {
            for item in &module.items {
                self.declare_item(item, path, entry);
            }
        }
        for extern_fn in &extern_functions {
            self.declare_extern_function(extern_fn);
        }
        for (_, module) in modules {
            for item in module.items {
                self.define_item(item);
            }
        }
    }
}
