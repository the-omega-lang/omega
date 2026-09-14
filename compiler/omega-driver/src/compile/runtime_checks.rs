//! Binds the analyzer's runtime checks to the `core` panic contract.
//!
//! This runs once over the final checked bodies: it finds the functions whose
//! bodies reach a checked operation, resolves `core::panic` for them, and
//! hands each one the support its generated panics call. Doing it here rather
//! than inside four separate lowering sites is what keeps MIR free of name
//! resolution and keeps one compilation to one resolution.

use super::*;
use omega_analyzer::checked::{CheckedBlock, CheckedFunctionDef};
use omega_analyzer::runtime_checks::{self, FunctionRuntimeChecks, PanicInfoField, PanicSupport};
use omega_diagnostics::SourceFile;
use omega_hir::HirId;
use omega_parser::prelude::Span;
use std::rc::Rc;

/// The panic contract `core` is required to declare. Resolving the members by
/// these names, against these declarations, is what keeps a compiler-generated
/// panic off anything a program merely spelled the same way.
const PANIC_MODULE: [&str; 2] = ["core", "panic"];
const HANDLER: &str = "PanicHandler";
const HANDLER_FUNCTION: &str = "panic";
const INFO: &str = "PanicInfo";

impl Driver {
    /// Attaches panic support to every emitted body that reaches a checked
    /// operation. A body that reaches none keeps `None`, so nothing about it
    /// -- including its object's reference to the panic gap -- changes, and a
    /// compilation whose bodies reach none never resolves `core::panic` at
    /// all.
    pub(super) fn bind_runtime_checks(&mut self, modules: &mut CheckedModules) {
        let Some((module, decl_id, span)) = first_checked_body(modules) else {
            return;
        };
        let support = match self.resolve_panic_support() {
            Ok(support) => Rc::new(support),
            Err(detail) => {
                // One finding for the compilation: every further body would
                // report the same missing contract at a different line.
                self.diagnostics.error(
                    &module,
                    AnalysisError::new(
                        decl_id,
                        span,
                        AnalysisErrorKind::RuntimeCheckSupportUnavailable { detail },
                    ),
                );
                return;
            }
        };

        let driver = &*self;
        for (module, checked) in modules.iter_mut() {
            for item in &mut checked.items {
                for (decl_id, slot) in checked_bodies_mut(item) {
                    if let Some(source) = driver.body_source(module, decl_id) {
                        *slot = Some(FunctionRuntimeChecks::new(support.clone(), source));
                    }
                }
            }
        }
    }

    /// The file a body's spans index: the one its text was written in, which
    /// for a generic instantiation is still its template's.
    fn body_source(&self, module: &ModulePath, decl_id: HirId) -> Option<Rc<SourceFile>> {
        let id = ModuleResolver::function_source(self, decl_id)
            .or_else(|| self.modules.source_id(module))?;
        self.modules.sources().shared(id)
    }

    /// Resolves and validates `core`'s panic contract exactly once. The gap
    /// member is `shared`, so the compiler's own reference to it bypasses
    /// visibility; nothing about what source may name changes.
    fn resolve_panic_support(&mut self) -> Result<PanicSupport, String> {
        let module: Vec<Ident> = PANIC_MODULE.iter().map(|s| Ident(s.to_string())).collect();
        let options = ResolveItemOptions::DIRECT.bypassing_visibility(true);

        let info_path: Vec<Ident> = module
            .iter()
            .cloned()
            .chain([Ident(INFO.to_string())])
            .collect();
        let info_type = match self.resolve_item(&module, &info_path, &[], options) {
            Ok(ResolvedItem::Type(r#type @ ResolvedType::Struct(_))) => r#type,
            Ok(_) => return Err(format!("'{}' is not a struct", join(&info_path))),
            Err(error) => return Err(error.to_string()),
        };
        let ResolvedType::Struct(cell) = &info_type else {
            unreachable!("just matched a struct")
        };
        let fields = {
            let definition = cell.borrow();
            let field = |name: &str, expected: ResolvedType| {
                let index = definition
                    .fields
                    .iter()
                    .position(|f| f.name.as_ref() == name)
                    .ok_or_else(|| format!("'{}' declares no '{name}' field", join(&info_path)))?;
                let found = definition.fields[index].r#type.clone();
                if found != expected {
                    return Err(format!(
                        "'{}::{name}' must be '{expected}', found '{found}'",
                        join(&info_path)
                    ));
                }
                Ok(PanicInfoField {
                    index,
                    r#type: found,
                })
            };
            let str_pointer = ResolvedType::Str { mutable: false };
            (
                field("source_file", str_pointer.clone())?,
                field("line", ResolvedType::U32)?,
                field("column", ResolvedType::U32)?,
                field("message", str_pointer)?,
            )
        };

        let handler_path: Vec<Ident> = module
            .iter()
            .cloned()
            .chain([Ident(HANDLER.to_string())])
            .collect();
        let gap = match self.resolve_item(&module, &handler_path, &[], options) {
            Ok(ResolvedItem::Gap(gap)) => gap,
            Ok(_) => return Err(format!("'{}' is not a gap", join(&handler_path))),
            Err(error) => return Err(error.to_string()),
        };
        let Some((_, handler)) = gap
            .functions
            .iter()
            .find(|(name, _)| name.as_ref() == HANDLER_FUNCTION)
        else {
            return Err(format!(
                "gap '{}' declares no '{HANDLER_FUNCTION}' function",
                join(&handler_path)
            ));
        };
        let signature = &handler.fn_type;
        let expected_param = ResolvedType::Pointer {
            pointee: Box::new(info_type.clone()),
            mutable: false,
        };
        match signature.params.as_slice() {
            [param] if param.r#type == expected_param => {}
            _ => {
                return Err(format!(
                    "'{}::{HANDLER_FUNCTION}' must take exactly one '*{INFO}' parameter",
                    join(&handler_path)
                ));
            }
        }
        if *signature.return_type != ResolvedType::Never || signature.is_variadic {
            return Err(format!(
                "'{}::{HANDLER_FUNCTION}' must be a non-variadic '=> never' function",
                join(&handler_path)
            ));
        }

        let (source_file, line, column, message) = fields;
        Ok(PanicSupport {
            handler_decl_id: handler.decl_id,
            handler_fn_type: signature.clone(),
            info_type,
            source_file,
            line,
            column,
            message,
        })
    }
}

fn join(path: &[Ident]) -> String {
    path.iter()
        .map(Ident::as_ref)
        .collect::<Vec<_>>()
        .join("::")
}

/// Every emitted body in one item, paired with what identifies its source.
/// Methods are emitted from inside their owner's item, so they are reached
/// through it rather than as items of their own.
fn item_bodies(item: &CheckedItem) -> Vec<(HirId, Span, &CheckedBlock)> {
    fn owned(functions: &[CheckedFunctionDef]) -> Vec<(HirId, Span, &CheckedBlock)> {
        functions.iter().map(|f| (f.id, f.span, &f.body)).collect()
    }
    match item {
        CheckedItem::FunctionDefinition(f) => vec![(f.id, f.span, &f.body)],
        CheckedItem::ForeignFunction(f) => f
            .body
            .as_ref()
            .map(|body| vec![(f.id, f.span, body)])
            .unwrap_or_default(),
        CheckedItem::Struct(s) => owned(&s.functions),
        CheckedItem::Union(u) => owned(&u.functions),
        CheckedItem::Enum(e) => owned(&e.functions),
        CheckedItem::Declaration(_) | CheckedItem::ForeignBinding(_) => Vec::new(),
    }
}

/// The bodies that reach a checked operation, ready to be given their
/// support. Scanning here rather than in the caller is what keeps the scan
/// and the assignment from disagreeing about which bodies are affected.
fn checked_bodies_mut(item: &mut CheckedItem) -> Vec<(HirId, &mut Option<FunctionRuntimeChecks>)> {
    fn owned(
        functions: &mut [CheckedFunctionDef],
    ) -> Vec<(HirId, &mut Option<FunctionRuntimeChecks>)> {
        functions
            .iter_mut()
            .filter(|f| reaches_a_check(Some(&f.body)))
            .map(|f| (f.id, &mut f.runtime_checks))
            .collect()
    }
    match item {
        CheckedItem::FunctionDefinition(f) if reaches_a_check(Some(&f.body)) => {
            vec![(f.id, &mut f.runtime_checks)]
        }
        CheckedItem::ForeignFunction(f) if reaches_a_check(f.body.as_ref()) => {
            vec![(f.id, &mut f.runtime_checks)]
        }
        CheckedItem::FunctionDefinition(_) | CheckedItem::ForeignFunction(_) => Vec::new(),
        CheckedItem::Struct(s) => owned(&mut s.functions),
        CheckedItem::Union(u) => owned(&mut u.functions),
        CheckedItem::Enum(e) => owned(&mut e.functions),
        CheckedItem::Declaration(_) | CheckedItem::ForeignBinding(_) => Vec::new(),
    }
}

fn reaches_a_check(body: Option<&CheckedBlock>) -> bool {
    body.is_some_and(|body| !runtime_checks::scan_block(body).is_empty())
}

/// The first body in the compilation that reaches a checked operation, which
/// is both the reason to resolve `core::panic` and where a failure to is
/// reported.
fn first_checked_body(modules: &CheckedModules) -> Option<(ModulePath, HirId, Span)> {
    modules.iter().find_map(|(module, checked)| {
        checked.items.iter().find_map(|item| {
            item_bodies(item)
                .into_iter()
                .find(|(_, _, body)| reaches_a_check(Some(body)))
                .map(|(id, span, _)| (module.clone(), id, span))
        })
    })
}
