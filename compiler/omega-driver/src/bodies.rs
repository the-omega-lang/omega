use crate::Driver;
use crate::items::{CheckedBody, ItemKey};
use omega_analyzer::analysis::{AnalysisSite, Analyzer, MemberBodies};
use omega_analyzer::checked::{CheckedEnumDef, CheckedItem, CheckedStructDef, CheckedUnionDef};
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind};
use omega_analyzer::generics::GenericParam;
use omega_analyzer::generics::GenericSubstitution;
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedGenericArg, ResolvedType};
use omega_analyzer::resolver::ResolvedItem;
use omega_hir::{HirId, HirItem, ModulePath};
use omega_parser::prelude::Ident;

trait CheckedAggregate: Sized {
    fn assemble(self, generic_args: Vec<ResolvedGenericArg>) -> CheckedItem;
}

impl CheckedAggregate for CheckedStructDef {
    fn assemble(mut self, generic_args: Vec<ResolvedGenericArg>) -> CheckedItem {
        self.generic_args = generic_args;
        CheckedItem::Struct(self)
    }
}

impl CheckedAggregate for CheckedEnumDef {
    fn assemble(mut self, generic_args: Vec<ResolvedGenericArg>) -> CheckedItem {
        self.generic_args = generic_args;
        CheckedItem::Enum(self)
    }
}

impl CheckedAggregate for CheckedUnionDef {
    fn assemble(mut self, generic_args: Vec<ResolvedGenericArg>) -> CheckedItem {
        self.generic_args = generic_args;
        CheckedItem::Union(self)
    }
}

impl Driver {
    pub(crate) fn ensure_item_body(&mut self, key: &ItemKey) -> Option<CheckedBody> {
        if let Some(body) = self.items.cached_body(key) {
            return Some(body.clone_of());
        }
        // A body has nothing sound to check while its own signature is
        // unavailable. The signature failure is already reported, so this is a
        // skip, not a second diagnostic.
        if !self.items.is_resolved(key) {
            return None;
        }
        if !self.items.begin_body(key) {
            return None;
        }

        let body = if key.owner().is_some() {
            self.check_method_body(key)
        } else {
            let hir = self.modules.hir(key.module());
            self.check_item_body(key, &hir.items[key.disambiguator])
        };
        if let Some(body) = &body {
            self.items.cache_checked_body(key, body.clone_of());
        }

        self.items.finish_body(key);
        body
    }

    pub(crate) fn check_item_body(&mut self, key: &ItemKey, item: &HirItem) -> Option<CheckedBody> {
        match item {
            HirItem::Declaration { decl, .. } => Some(self.checked_global_body(decl.id)),

            HirItem::DeclarationWithInit { decl, .. } => Some(self.checked_global_body(decl.id)),

            HirItem::Walrus { walrus: w, .. } if w.comp => None,
            HirItem::Walrus { walrus: w, .. } => Some(self.checked_global_body(w.id)),

            HirItem::ForeignBinding(binding) => {
                let r#type = self.resolved_value_type(key);
                let symbol = self
                    .items
                    .function_annotations
                    .get(&binding.id)
                    .map(|a| a.symbol.clone())
                    .unwrap_or_else(omega_analyzer::annotations::SymbolPolicy::foreign);
                let checked = omega_analyzer::checked::CheckedForeignBinding {
                    id: binding.id,
                    span: binding.span,
                    ident: binding.ident.clone(),
                    r#type,
                    symbol,
                };
                Some(CheckedBody {
                    item: CheckedItem::ForeignBinding(checked),
                    warnings: vec![],
                })
            }

            HirItem::ForeignFunction(f) => {
                let ResolvedItem::Value {
                    r#type: ResolvedType::Function(fn_type),
                    decl_id,
                    ..
                } = self.items.expect_resolved(key).clone()
                else {
                    unreachable!(
                        "a foreign function's own resolved item is always ResolvedType::Function"
                    );
                };
                let annotations = self
                    .items
                    .function_annotations
                    .get(&decl_id)
                    .cloned()
                    .unwrap_or_else(|| omega_analyzer::annotations::ResolvedAnnotations {
                        symbol: omega_analyzer::annotations::SymbolPolicy::foreign(),
                        ..Default::default()
                    });
                let run = self.with_analyzer(
                    key.module(),
                    &GenericSubstitution::new(),
                    AnalysisSite::new(f.id, f.span),
                    |analyzer| analyzer.check_foreign_function_body(f, &fn_type, &annotations),
                );
                run.result.map(|checked| CheckedBody {
                    item: CheckedItem::ForeignFunction(checked),
                    warnings: run.warnings,
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
                // The body is checked against the normalized signature, so its
                // substitution must be keyed by the normalized generics.
                let generics = self
                    .normalized_function(key.module(), f)
                    .map(|f| f.generics)
                    .unwrap_or_else(|_| GenericParam::declared(f.id, &f.generics));
                let substitution = GenericSubstitution::zip(&generics, &key.generic_args);
                let declared = self
                    .items
                    .declared_bounds
                    .get(key)
                    .cloned()
                    .unwrap_or_default();
                let keys_run = self.with_analyzer(
                    key.module(),
                    &substitution,
                    AnalysisSite::new(f.id, f.span),
                    |a| a.expand_bound_set(f.id, f.span, &declared),
                );
                self.diagnostics
                    .record_warnings(key.module(), keys_run.warnings);
                let keys = keys_run.result;
                let bounds = self.bound_context_over(&declared, &keys);
                let annotations = self
                    .items
                    .function_annotations
                    .get(&decl_id)
                    .cloned()
                    .unwrap_or_default();
                let template = if generics.is_empty() {
                    None
                } else {
                    Some(self.template_descriptor(key, f, &GenericSubstitution::new())?)
                };
                let run = self.with_analyzer_in(
                    key.module(),
                    &substitution,
                    &bounds,
                    AnalysisSite::new(f.id, f.span),
                    |analyzer| analyzer.check_function_body(f, &fn_type, decl_id, &annotations),
                );
                run.result.map(|mut checked| {
                    checked.generic_args = key.generic_args.clone();
                    checked.template = template;
                    CheckedBody {
                        item: CheckedItem::FunctionDefinition(checked),
                        warnings: run.warnings,
                    }
                })
            }

            HirItem::Struct(_) | HirItem::Enum(_) | HirItem::Union(_) => {
                let members = if key.is_instantiation() {
                    MemberBodies::Only(&[])
                } else {
                    MemberBodies::All
                };
                self.check_aggregate_item(key, item, members)
            }

            HirItem::Spec(_) => None,
            HirItem::Gap(_) | HirItem::Glue(_) | HirItem::Conform(_) | HirItem::Primitive(_) => {
                None
            }
            HirItem::Import(_) => unreachable!("imports are filtered out before this is called"),
            HirItem::Alias(_) => unreachable!("aliases never get an item key, so never a body"),
        }
    }

    /// An aggregate's body, covering the member bodies `members` selects.
    fn check_aggregate_item(
        &mut self,
        key: &ItemKey,
        item: &HirItem,
        members: MemberBodies<'_>,
    ) -> Option<CheckedBody> {
        match item {
            HirItem::Struct(s) => {
                let nominal = self.items.nominals.expect(key);
                let self_type = ResolvedType::Struct(nominal.clone());
                self.check_aggregate_body(
                    key,
                    AnalysisSite::new(s.id, s.span),
                    &GenericParam::declared(s.id, &s.generics),
                    self_type,
                    |a| a.check_struct_body(s, &nominal, members),
                )
            }

            HirItem::Enum(e) => {
                let nominal = self.items.nominals.expect(key);
                let self_type = ResolvedType::Enum {
                    nominal: nominal.clone(),
                    variant: None,
                };
                self.check_aggregate_body(
                    key,
                    AnalysisSite::new(e.id, e.span),
                    &GenericParam::declared(e.id, &e.generics),
                    self_type,
                    |a| a.check_enum_body(e, &nominal, members),
                )
            }

            HirItem::Union(u) => {
                let nominal = self.items.nominals.expect(key);
                let self_type = ResolvedType::Union(nominal.clone());
                self.check_aggregate_body(
                    key,
                    AnalysisSite::new(u.id, u.span),
                    &GenericParam::declared(u.id, &u.generics),
                    self_type,
                    |a| a.check_union_body(u, &nominal, members),
                )
            }

            _ => unreachable!("only an aggregate declaration has member bodies"),
        }
    }

    /// The instantiated generic owner that declares `decl_id` as one of its
    /// concrete members. Such a member's body is checked only when used.
    pub(crate) fn instance_member_owner(&self, decl_id: HirId) -> Option<ItemKey> {
        let owner = self.items.decl_id_owner.get(&decl_id)?;
        if !owner.is_instantiation() || owner.owner().is_some() || !self.items.is_resolved(owner) {
            return None;
        }
        let hir = self.modules.hir(owner.module());
        if !matches!(
            hir.items[owner.disambiguator],
            HirItem::Struct(_) | HirItem::Enum(_) | HirItem::Union(_)
        ) {
            return None;
        }
        let nominal = self.items.nominals.expect(owner);
        let methods = self.items.types.functions(&nominal).ok()?;
        methods
            .iter()
            .any(|(_, method)| method.decl_id == decl_id)
            .then(|| owner.clone())
    }

    /// One concrete member of an instantiated generic owner, checked under
    /// the owner's substitution and bounds, as a `FunctionDefinition` body.
    pub(crate) fn ensure_instance_member_body(&mut self, decl_id: HirId) -> Option<CheckedBody> {
        if let Some(body) = self.items.instance_member_bodies.get(&decl_id) {
            return body.as_ref().map(CheckedBody::clone_of);
        }
        let owner = self.instance_member_owner(decl_id)?;
        if !self.items.begin_member_body(decl_id) {
            return None;
        }
        let hir = self.modules.hir(owner.module());
        let body = self
            .check_aggregate_item(
                &owner,
                &hir.items[owner.disambiguator],
                MemberBodies::Only(&[decl_id]),
            )
            .and_then(|body| {
                let functions = match body.item {
                    CheckedItem::Struct(s) => s.functions,
                    CheckedItem::Enum(e) => e.functions,
                    CheckedItem::Union(u) => u.functions,
                    _ => unreachable!("an aggregate body check yields an aggregate"),
                };
                let function = functions.into_iter().next()?;
                Some(CheckedBody {
                    item: CheckedItem::FunctionDefinition(function),
                    warnings: body.warnings,
                })
            });
        self.items.finish_member_body(decl_id);
        self.items
            .instance_member_bodies
            .insert(decl_id, body.as_ref().map(CheckedBody::clone_of));
        body
    }

    /// The declaration identity an instantiation's symbol carries.
    ///
    /// Taken under `enclosing` alone -- the owner instantiation for a method,
    /// nothing for a free function -- so the declaration's *own* generic
    /// parameters are still open. That is exactly what distinguishes two
    /// declarations one selector could have chosen between, and substituting
    /// them first would erase it.
    ///
    /// A failure stops body materialization: a generic body cannot be emitted
    /// without its declaration identity, even in builds without assertions.
    pub(crate) fn template_descriptor(
        &mut self,
        key: &ItemKey,
        function: &omega_hir::HirFunctionDef,
        enclosing: &GenericSubstitution,
    ) -> Option<omega_analyzer::template::TemplateDescriptor> {
        let site = AnalysisSite::new(function.id, function.span);
        let run = self.with_analyzer(key.module(), enclosing, site, |analyzer| {
            analyzer.template_descriptor(function)
        });
        if run.failed {
            return None;
        }
        Some(
            run.result
                .expect("a valid generic declaration has a template identity"),
        )
    }

    fn check_aggregate_body<C: CheckedAggregate>(
        &mut self,
        key: &ItemKey,
        owner: AnalysisSite,
        generics: &[GenericParam],
        self_type: ResolvedType,
        check: impl FnOnce(&mut Analyzer) -> Option<C>,
    ) -> Option<CheckedBody> {
        let substitution =
            GenericSubstitution::zip(generics, &key.generic_args).with_self_type(self_type.clone());

        let declared = self
            .items
            .declared_bounds
            .get(key)
            .cloned()
            .unwrap_or_default();
        let keys_run = self.with_analyzer(key.module(), &substitution, owner, |a| {
            a.expand_bound_set(owner.id, owner.span, &declared)
        });
        self.diagnostics
            .record_warnings(key.module(), keys_run.warnings);
        let keys = keys_run.result;
        let bounds = self.bound_context_over(&declared, &keys);
        let run = self.with_analyzer_in(key.module(), &substitution, &bounds, owner, check);
        run.result.map(|checked| CheckedBody {
            item: checked.assemble(key.generic_args.clone()),
            warnings: run.warnings,
        })
    }

    fn checked_global_body(&self, id: omega_hir::HirId) -> CheckedBody {
        CheckedBody {
            item: CheckedItem::Declaration(self.items.expect_checked_global(id).clone()),
            warnings: vec![],
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
}

impl Driver {
    pub(crate) fn check_overload_duplicates(
        &mut self,
        module_path: &ModulePath,
        name: &Ident,
        indices: &[usize],
        signatures: &[ResolvedFunctionType],
    ) {
        let hir = self.modules.hir(module_path);
        let same_params = |a: &ResolvedFunctionType, b: &ResolvedFunctionType| {
            a.param_types().eq(b.param_types())
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
