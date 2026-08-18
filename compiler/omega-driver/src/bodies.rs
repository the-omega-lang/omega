use crate::Driver;
use crate::items::{CheckedBody, ItemKey};
use omega_analyzer::analysis::{AnalysisSite, Analyzer};
use omega_analyzer::checked::{
    CheckedDeclaration, CheckedEnumDef, CheckedExternDeclaration, CheckedItem, CheckedStructDef,
    CheckedUnionDef,
};
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind};
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_analyzer::resolver::{ResolveError, ResolvedItem};
use omega_hir::{HirGenericParam, HirItem};
use omega_parser::prelude::Ident;

trait CheckedAggregate: Sized {
    fn assemble(self, type_args: Vec<ResolvedType>) -> CheckedItem;
}

impl CheckedAggregate for CheckedStructDef {
    fn assemble(mut self, type_args: Vec<ResolvedType>) -> CheckedItem {
        self.type_args = type_args;
        CheckedItem::Struct(self)
    }
}

impl CheckedAggregate for CheckedEnumDef {
    fn assemble(mut self, type_args: Vec<ResolvedType>) -> CheckedItem {
        self.type_args = type_args;
        CheckedItem::Enum(self)
    }
}

impl CheckedAggregate for CheckedUnionDef {
    fn assemble(mut self, type_args: Vec<ResolvedType>) -> CheckedItem {
        self.type_args = type_args;
        CheckedItem::Union(self)
    }
}

impl Driver {
    pub(crate) fn ensure_item_body(&mut self, key: &ItemKey, index: usize) -> Option<CheckedBody> {
        if let Some(body) = self.items.cached_body(key) {
            return Some(body.clone_of());
        }
        if !self.items.begin_body(key) {
            return None;
        }

        let body = if key.is_instantiation() {
            self.check_generic_instantiation_body(key, index);
            self.items.cached_body(key).map(CheckedBody::clone_of)
        } else {
            let hir = self.modules.hir(&key.module);
            let body = self.check_item_body(key, &hir.items[index]);
            if let Some(body) = &body {
                self.items.cache_checked_body(key, body.clone_of());
            }
            body
        };

        self.items.finish_body(key);
        body
    }

    pub(crate) fn check_item_body(&mut self, key: &ItemKey, item: &HirItem) -> Option<CheckedBody> {
        match item {
            HirItem::Declaration { decl, .. } => {
                let r#type = self.resolved_value_type(key);
                let checked = CheckedDeclaration {
                    id: decl.id,
                    span: decl.span,
                    ident: decl.ident.clone(),
                    r#type,
                    mutable: decl.mutable,
                    initial_value: None,
                };
                Some(CheckedBody {
                    item: CheckedItem::Declaration(checked),
                    warnings: vec![],
                })
            }

            HirItem::DeclarationWithInit { decl, .. } => {
                let r#type = self.resolved_value_type(key);
                let initial_value = self.items.global_initial_values.get(&decl.id).cloned();
                let checked = CheckedDeclaration {
                    id: decl.id,
                    span: decl.span,
                    ident: decl.ident.clone(),
                    r#type,
                    mutable: decl.mutable,
                    initial_value,
                };
                Some(CheckedBody {
                    item: CheckedItem::Declaration(checked),
                    warnings: vec![],
                })
            }

            HirItem::Walrus { walrus: w, .. } if w.comp => None,
            HirItem::Walrus { walrus: w, .. } => {
                let r#type = self.resolved_value_type(key);
                let initial_value = self.items.global_initial_values.get(&w.id).cloned();
                let checked = CheckedDeclaration {
                    id: w.id,
                    span: w.span,
                    ident: w.ident.clone(),
                    r#type,
                    mutable: w.mutable,
                    initial_value,
                };
                Some(CheckedBody {
                    item: CheckedItem::Declaration(checked),
                    warnings: vec![],
                })
            }

            HirItem::ExternDeclaration(decl) => {
                let r#type = self.resolved_value_type(key);
                let checked = CheckedExternDeclaration {
                    id: decl.id,
                    span: decl.span,
                    ident: decl.ident.clone(),
                    r#type,
                    mangling: omega_analyzer::annotations::ManglingMode::Disabled,
                };
                Some(CheckedBody {
                    item: CheckedItem::ExternDeclaration(checked),
                    warnings: vec![],
                })
            }

            HirItem::FunctionDefinition(f) => {
                let ResolvedItem::Value {
                    r#type: ResolvedType::Function(fn_type),
                    decl_id,
                    ..
                } = self.items.expect_resolved(key).clone()
                else {
                    unreachable!("a function's own resolved item is always ResolvedType::Function");
                };
                let substitution = Self::substitution(&f.generics, &key.type_args);
                let declared = self
                    .items
                    .declared_bounds
                    .get(key)
                    .cloned()
                    .unwrap_or_default();
                let keys_run = self.with_analyzer(
                    &key.module,
                    &substitution,
                    AnalysisSite::new(f.id, f.span),
                    |a| a.expand_bound_set(f.id, f.span, &declared),
                );
                self.diagnostics
                    .record_warnings(&key.module, keys_run.warnings);
                let keys = keys_run.result;
                let bounds = self.bound_context_over(&declared, &keys);
                let annotations = self
                    .items
                    .function_annotations
                    .get(&decl_id)
                    .cloned()
                    .unwrap_or_default();
                let run = self.with_analyzer_in(
                    &key.module,
                    &substitution,
                    &bounds,
                    AnalysisSite::new(f.id, f.span),
                    |analyzer| analyzer.check_function_body(f, &fn_type, decl_id, &annotations),
                );
                run.result.map(|mut checked| {
                    checked.type_args = key.type_args.clone();
                    CheckedBody {
                        item: CheckedItem::FunctionDefinition(checked),
                        warnings: run.warnings,
                    }
                })
            }

            HirItem::Struct(s) => {
                let cell = self.items.cells.expect_struct(key);
                let self_type = ResolvedType::Struct(cell.clone());
                self.check_aggregate_body(
                    key,
                    AnalysisSite::new(s.id, s.span),
                    &s.generics,
                    self_type,
                    |a| a.check_struct_body(s, &cell),
                )
            }

            HirItem::Enum(e) => {
                let cell = self.items.cells.expect_enum(key);
                let self_type = ResolvedType::Enum {
                    cell: cell.clone(),
                    variant: None,
                };
                self.check_aggregate_body(
                    key,
                    AnalysisSite::new(e.id, e.span),
                    &e.generics,
                    self_type,
                    |a| a.check_enum_body(e, &cell),
                )
            }

            HirItem::Union(u) => {
                let cell = self.items.cells.expect_union(key);
                let self_type = ResolvedType::Union(cell.clone());
                self.check_aggregate_body(
                    key,
                    AnalysisSite::new(u.id, u.span),
                    &u.generics,
                    self_type,
                    |a| a.check_union_body(u, &cell),
                )
            }

            HirItem::Spec(_) => None,
            HirItem::Gap(_) | HirItem::Glue(_) | HirItem::Conform(_) | HirItem::Primitive(_) => {
                None
            }
            HirItem::Import(_) => unreachable!("imports are filtered out before this is called"),
        }
    }

    fn check_aggregate_body<C: CheckedAggregate>(
        &mut self,
        key: &ItemKey,
        owner: AnalysisSite,
        generics: &[HirGenericParam],
        self_type: ResolvedType,
        check: impl FnOnce(&mut Analyzer) -> Option<C>,
    ) -> Option<CheckedBody> {
        let mut substitution = Self::substitution(generics, &key.type_args);
        substitution.push((Ident("Self".to_string()), self_type.clone()));

        let declared = self
            .items
            .declared_bounds
            .get(key)
            .cloned()
            .unwrap_or_default();
        let keys_run = self.with_analyzer(&key.module, &substitution, owner, |a| {
            a.expand_bound_set(owner.id, owner.span, &declared)
        });
        self.diagnostics
            .record_warnings(&key.module, keys_run.warnings);
        let keys = keys_run.result;
        let bounds = self.bound_context_over(&declared, &keys);
        let run = self.with_analyzer_in(&key.module, &substitution, &bounds, owner, check);
        run.result.map(|checked| CheckedBody {
            item: checked.assemble(key.type_args.clone()),
            warnings: run.warnings,
        })
    }

    pub(crate) fn check_generic_instantiation_body(&mut self, key: &ItemKey, index: usize) {
        let hir = self.modules.hir(&key.module);
        if let Some(body) = self.check_item_body(key, &hir.items[index]) {
            self.items.generic_instantiations.insert(key.clone(), body);
        }
    }

    fn resolved_value_type(&self, key: &ItemKey) -> ResolvedType {
        match self.items.expect_resolved(key) {
            ResolvedItem::Value { r#type, .. } => r#type.clone(),
            ResolvedItem::Type(_) | ResolvedItem::Gap(_) => {
                unreachable!("a declaration's own resolved item is always a value")
            }
        }
    }

    fn substitution(
        generics: &[HirGenericParam],
        type_args: &[ResolvedType],
    ) -> Vec<(Ident, ResolvedType)> {
        generics
            .iter()
            .map(|g| g.ident.clone())
            .zip(type_args.iter().cloned())
            .collect()
    }
}

impl Driver {
    pub(crate) fn ensure_overload_signature(
        &mut self,
        module_path: &[Ident],
        index: usize,
    ) -> Result<ResolvedFunctionType, ResolveError> {
        let key = (module_path.to_vec(), index);
        if let Some(fn_type) = self.items.overload_signatures.get(&key) {
            return Ok(fn_type.clone());
        }
        let hir = self.modules.hir(module_path);
        let HirItem::FunctionDefinition(f) = &hir.items[index] else {
            unreachable!("only ever called with an index confirmed to be a function");
        };

        let checked = self.analyze(module_path, &[], AnalysisSite::new(f.id, f.span), |a| {
            a.collect_function_signature(f)
        });
        let (fn_type, annotations) = checked.ok_or_else(|| ResolveError::ItemFailed {
            module: module_path.to_vec(),
            item: f.name.clone(),
        })?;

        self.items.function_annotations.insert(f.id, annotations);
        self.items.overload_signatures.insert(key, fn_type.clone());
        Ok(fn_type)
    }

    pub(crate) fn ensure_overload_body(
        &mut self,
        module_path: &[Ident],
        index: usize,
    ) -> Option<CheckedBody> {
        let key = (module_path.to_vec(), index);
        if let Some(body) = self.items.overload_bodies.get(&key) {
            return Some(body.clone_of());
        }
        let fn_type = self.ensure_overload_signature(module_path, index).ok()?;
        let hir = self.modules.hir(module_path);
        let HirItem::FunctionDefinition(f) = &hir.items[index] else {
            unreachable!("only ever called with an index confirmed to be a function");
        };
        let annotations = self
            .items
            .function_annotations
            .get(&f.id)
            .cloned()
            .unwrap_or_default();

        let run = self.with_analyzer(
            module_path,
            &[],
            AnalysisSite::new(f.id, f.span),
            |analyzer| analyzer.check_function_body(f, &fn_type, f.id, &annotations),
        );
        let body = CheckedBody {
            item: CheckedItem::FunctionDefinition(run.result?),
            warnings: run.warnings,
        };
        self.items.overload_bodies.insert(key, body.clone_of());
        Some(body)
    }

    pub(crate) fn check_overload_duplicates(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
        indices: &[usize],
        signatures: &[ResolvedFunctionType],
    ) {
        let hir = self.modules.hir(module_path);
        let same_params = |a: &ResolvedFunctionType, b: &ResolvedFunctionType| {
            a.params
                .iter()
                .map(|(_, t)| t)
                .eq(b.params.iter().map(|(_, t)| t))
        };
        for i in 1..indices.len() {
            let Some(j) = (0..i).find(|&j| same_params(&signatures[i], &signatures[j])) else {
                continue;
            };
            let HirItem::FunctionDefinition(function) = &hir.items[indices[i]] else {
                unreachable!("overload indices only contain function definitions");
            };
            let HirItem::FunctionDefinition(previous_function) = &hir.items[indices[j]] else {
                unreachable!("overload indices only contain function definitions");
            };
            self.diagnostics.error(
                module_path,
                AnalysisError::new(
                    function.id,
                    function.name_span,
                    AnalysisErrorKind::Redeclaration {
                        name: name.clone(),
                        previous: Some(previous_function.name_span),
                    },
                ),
            );
        }
    }
}
