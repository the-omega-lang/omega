use crate::{Driver, ModulePath};
use omega_analyzer::analysis::PendingSpecMethod;
use omega_analyzer::checked::ComposeOwner;
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind};
use omega_analyzer::resolved_type::{ResolvedMethod, ResolvedSpecType, ResolvedType};
use omega_diagnostics::Span;
use omega_hir::{HirComposeDef, HirFunctionDef, HirId, HirItem, HirPrimitiveDef};
use omega_parser::prelude::{Ident, Type};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub(crate) struct ComposeEntry {
    pub module: ModulePath,
    pub id: HirId,
    pub span: Span,
    pub target: ResolvedType,
    pub spec: Rc<RefCell<ResolvedSpecType>>,
    pub spec_args: Vec<ResolvedType>,
    pub methods: Vec<(Ident, ResolvedMethod)>,
    pub functions: Vec<HirFunctionDef>,
    pub pending: Vec<PendingSpecMethod>,
    pub substitution: Vec<(Ident, ResolvedType)>,
}

#[derive(Clone)]
struct ComposeTemplate {
    module: ModulePath,
    compose: HirComposeDef,
}

#[derive(Default)]
pub(crate) struct Composes {
    pub entries: Vec<ComposeEntry>,
    templates: Vec<ComposeTemplate>,
    pub emitted: Vec<(ResolvedType, HirId, Vec<ResolvedType>)>,
}

#[derive(Clone)]
pub(crate) struct PrimitiveEntry {
    pub module: ModulePath,
    pub span: Span,
    pub target: ResolvedType,
    pub methods: Vec<(Ident, ResolvedMethod)>,
    pub functions: Vec<HirFunctionDef>,
    pub substitution: Vec<(Ident, ResolvedType)>,
}

#[derive(Clone)]
struct PrimitiveTemplate {
    module: ModulePath,
    primitive: HirPrimitiveDef,
}

#[derive(Default)]
pub(crate) struct Primitives {
    pub entries: Vec<PrimitiveEntry>,
    templates: Vec<PrimitiveTemplate>,
    pub emitted: Vec<ResolvedType>,
}

impl Driver {
    pub(crate) fn collect_primitive_signatures(&mut self, paths: &[ModulePath]) {
        for module in paths {
            let declarations: Vec<_> = self
                .modules
                .parsed(module)
                .hir
                .items
                .iter()
                .filter_map(|item| match item {
                    HirItem::Primitive(primitive) => Some(primitive.clone()),
                    _ => None,
                })
                .collect();
            for primitive in declarations {
                if module.first().map(Ident::as_ref) != Some("core") {
                    self.diagnostics.error(
                        module,
                        AnalysisError::new(
                            primitive.id,
                            primitive.span,
                            AnalysisErrorKind::PrimitiveOutsideCore,
                        ),
                    );
                    continue;
                }
                if primitive.generics.is_empty() {
                    self.instantiate_primitive(module, &primitive, &[], None);
                } else {
                    self.primitives.templates.push(PrimitiveTemplate {
                        module: module.clone(),
                        primitive,
                    });
                }
            }
        }
    }

    fn instantiate_primitive(
        &mut self,
        module: &[Ident],
        primitive: &HirPrimitiveDef,
        substitution: &[(Ident, ResolvedType)],
        actual_target: Option<&ResolvedType>,
    ) -> Option<PrimitiveEntry> {
        let target = if let Some(actual) = actual_target {
            actual.clone()
        } else {
            let run = self.with_analyzer(
                module,
                substitution,
                (primitive.id, primitive.span),
                |analyzer| {
                    analyzer.resolve_compose_target(primitive.id, primitive.span, &primitive.target)
                },
            );
            self.diagnostics.record_warnings(module, run.warnings);
            run.result?
        };
        if !Self::primitive_target_allowed(&target) {
            self.diagnostics.error(
                module,
                AnalysisError::new(
                    primitive.id,
                    primitive.span,
                    AnalysisErrorKind::PrimitiveTargetNotAllowed {
                        target: target.to_string(),
                    },
                ),
            );
            return None;
        }
        if let Some(previous) = self
            .primitives
            .entries
            .iter()
            .find(|entry| entry.target == target)
        {
            self.diagnostics.error(
                module,
                AnalysisError::new(
                    primitive.id,
                    primitive.span,
                    AnalysisErrorKind::DuplicatePrimitiveTarget {
                        target: target.to_string(),
                        previous: previous.span,
                    },
                ),
            );
            return None;
        }
        let mut method_substitution = substitution.to_vec();
        let self_type = match (&primitive.target, &target) {
            (Type::UnknownSizeArray(_), ResolvedType::Slice { item, mutable }) => {
                ResolvedType::Array(item.clone(), *mutable)
            }
            _ => target.clone(),
        };
        method_substitution.push((Ident("Self".to_string()), self_type));
        let signatures = self.analyze(
            module,
            &method_substitution,
            (primitive.id, primitive.span),
            |analyzer| {
                let mut resolved = Vec::with_capacity(primitive.functions.len());
                for function in &primitive.functions {
                    let (fn_type, annotations) =
                        analyzer.collect_function_signature(function, None)?;
                    resolved.push((fn_type, annotations));
                }
                analyzer.check_overload_duplicates(&primitive.functions, &resolved);
                Some(
                    primitive
                        .functions
                        .iter()
                        .zip(resolved)
                        .map(|(function, (fn_type, annotations))| {
                            (
                                function.name.clone(),
                                ResolvedMethod {
                                    decl_id: function.id,
                                    fn_type,
                                    visibility: function.visibility,
                                    annotations,
                                    source: None,
                                },
                            )
                        })
                        .collect(),
                )
            },
        )?;
        let entry = PrimitiveEntry {
            module: module.to_vec(),
            span: primitive.span,
            target,
            methods: signatures,
            functions: primitive.functions.clone(),
            substitution: method_substitution,
        };
        self.primitives.entries.push(entry.clone());
        Some(entry)
    }

    fn primitive_target_allowed(target: &ResolvedType) -> bool {
        matches!(
            target,
            ResolvedType::Bool
                | ResolvedType::Char
                | ResolvedType::I8
                | ResolvedType::I16
                | ResolvedType::I32
                | ResolvedType::I64
                | ResolvedType::ISize
                | ResolvedType::U8
                | ResolvedType::U16
                | ResolvedType::U32
                | ResolvedType::U64
                | ResolvedType::USize
                | ResolvedType::F32
                | ResolvedType::F64
                | ResolvedType::Str { .. }
                | ResolvedType::Slice { .. }
        )
    }

    pub(crate) fn primitive_methods(
        &mut self,
        target: &ResolvedType,
    ) -> Vec<(Ident, ResolvedMethod)> {
        if let Some(entry) = self
            .primitives
            .entries
            .iter()
            .find(|entry| entry.target == *target)
        {
            return entry.methods.clone();
        }
        let templates = self.primitives.templates.clone();
        for template in templates {
            let Some(substitution) = Self::match_primitive_target(&template.primitive, target)
            else {
                continue;
            };
            if let Some(entry) = self.instantiate_primitive(
                &template.module,
                &template.primitive,
                &substitution,
                Some(target),
            ) {
                return entry.methods;
            }
        }
        Vec::new()
    }

    fn match_primitive_target(
        primitive: &HirPrimitiveDef,
        actual: &ResolvedType,
    ) -> Option<Vec<(Ident, ResolvedType)>> {
        let ResolvedType::Slice { item, .. } = actual else {
            return None;
        };
        let Type::UnknownSizeArray(raw_item) = &primitive.target else {
            return None;
        };
        let Type::Named(path) = raw_item.as_ref() else {
            return None;
        };
        if !path.is_unqualified()
            || !primitive
                .generics
                .iter()
                .any(|generic| generic.ident == path.head)
        {
            return None;
        }
        Some(vec![(path.head.clone(), (**item).clone())])
    }

    fn inherent_methods(&mut self, target: &ResolvedType) -> Vec<(Ident, ResolvedMethod)> {
        match target {
            ResolvedType::Struct(cell) => cell.borrow().functions.clone(),
            ResolvedType::Enum { cell, .. } => cell.borrow().functions.clone(),
            ResolvedType::Union(cell) => cell.borrow().functions.clone(),
            _ => self.primitive_methods(target),
        }
    }

    pub(crate) fn collect_compose_signatures(&mut self, paths: &[ModulePath]) {
        for module in paths {
            let declarations: Vec<_> = self
                .modules
                .parsed(module)
                .hir
                .items
                .iter()
                .filter_map(|item| match item {
                    HirItem::Compose(compose) => Some(compose.clone()),
                    _ => None,
                })
                .collect();
            for compose in declarations {
                if compose.generics.is_empty() {
                    self.instantiate_compose(module, &compose, &[]);
                } else if let Some(parameter) = Self::blanket_parameter(&compose) {
                    // Registering this as a template would be worse than
                    // useless: `match_compose_target` can never bind it, so
                    // the compose would be silently dropped and the only
                    // diagnostic anyone ever saw would be a
                    // `SpecNotImplemented` at some unrelated use site.
                    self.diagnostics.error(
                        module,
                        AnalysisError::new(
                            compose.id,
                            compose.span,
                            AnalysisErrorKind::BlanketComposeNotYetSupported { parameter },
                        ),
                    );
                } else {
                    self.composes.templates.push(ComposeTemplate {
                        module: module.clone(),
                        compose,
                    });
                }
            }
        }
    }

    fn instantiate_compose(
        &mut self,
        module: &[Ident],
        compose: &HirComposeDef,
        substitution: &[(Ident, ResolvedType)],
    ) -> Option<ComposeEntry> {
        let target_run = self.with_analyzer(
            module,
            substitution,
            (compose.id, compose.span),
            |analyzer| analyzer.resolve_compose_target(compose.id, compose.span, &compose.target),
        );
        self.diagnostics
            .record_warnings(module, target_run.warnings);
        let target = target_run.result?;
        // Instantiating one template twice at the same target is not a
        // duplicate compose -- `composes_for_type` re-walks every matching
        // template on each call, so without this the *second* lookup for a
        // generic target would report `DuplicateCompose` against the entry
        // the first lookup registered. Keyed on the declaration's own id, so
        // two genuinely distinct `compose` blocks still collide below.
        if let Some(existing) = self
            .composes
            .entries
            .iter()
            .find(|existing| existing.id == compose.id && existing.target == target)
        {
            return Some(existing.clone());
        }
        let mut method_substitution = substitution.to_vec();
        method_substitution.push((Ident("Self".to_string()), target.clone()));
        let inherent = self.inherent_methods(&target);
        let run = self.with_analyzer(
            module,
            &method_substitution,
            (compose.id, compose.span),
            |analyzer| {
                analyzer.check_compose_block(
                    compose.id,
                    compose.span,
                    &target,
                    &compose.spec,
                    &compose.functions,
                    &inherent,
                )
            },
        );
        self.diagnostics.record_warnings(module, run.warnings);
        let (spec, spec_args, methods, pending) = run.result?;
        let entry = ComposeEntry {
            module: module.to_vec(),
            id: compose.id,
            span: compose.span,
            target,
            spec,
            spec_args,
            methods,
            functions: compose.functions.clone(),
            pending,
            substitution: method_substitution,
        };
        if !self.check_compose_orphan(&entry) || self.reject_duplicate_compose(&entry) {
            return None;
        }
        self.composes.entries.push(entry.clone());
        Some(entry)
    }

    fn check_compose_orphan(&mut self, entry: &ComposeEntry) -> bool {
        let local = entry
            .module
            .first()
            .cloned()
            .unwrap_or_else(|| Ident(String::new()));
        let target_package = entry
            .target
            .declaring_owner()
            .and_then(|(path, _)| path.first().cloned())
            .unwrap_or_else(|| Ident("core".to_string()));
        let spec_package = entry
            .spec
            .borrow()
            .module_path
            .first()
            .cloned()
            .unwrap_or_else(|| Ident(String::new()));
        if local == target_package || local == spec_package {
            return true;
        }
        self.diagnostics.error(
            &entry.module,
            AnalysisError::new(
                entry.id,
                entry.span,
                AnalysisErrorKind::ComposeOrphanViolation {
                    target_package,
                    spec_package,
                },
            ),
        );
        false
    }

    fn reject_duplicate_compose(&mut self, entry: &ComposeEntry) -> bool {
        let Some(previous) = self.composes.entries.iter().find(|existing| {
            existing.target == entry.target
                && existing.spec.borrow().id == entry.spec.borrow().id
                && existing.spec_args == entry.spec_args
        }) else {
            return false;
        };
        self.diagnostics.error(
            &entry.module,
            AnalysisError::new(
                entry.id,
                entry.span,
                AnalysisErrorKind::DuplicateCompose {
                    target: entry.target.to_string(),
                    spec: entry.spec.borrow().name.clone(),
                    previous: previous.span,
                },
            ),
        );
        true
    }

    pub(crate) fn compose_for(
        &mut self,
        target: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedType],
    ) -> Option<ComposeEntry> {
        if let Some(entry) = self.composes.entries.iter().find(|entry| {
            entry.target == *target
                && entry.spec.borrow().id == spec.borrow().id
                && entry.spec_args == spec_args
        }) {
            return Some(entry.clone());
        }
        let templates = self.composes.templates.clone();
        for template in templates {
            let Some(substitution) = Self::match_compose_target(&template.compose, target) else {
                continue;
            };
            if let Some(entry) =
                self.instantiate_compose(&template.module, &template.compose, &substitution)
                && entry.spec.borrow().id == spec.borrow().id
                && entry.spec_args == spec_args
            {
                return Some(entry);
            }
        }
        None
    }

    pub(crate) fn composes_for_type(&mut self, target: &ResolvedType) -> Vec<ComposeEntry> {
        let templates = self.composes.templates.clone();
        for template in templates {
            if let Some(substitution) = Self::match_compose_target(&template.compose, target) {
                self.instantiate_compose(&template.module, &template.compose, &substitution);
            }
        }
        self.composes
            .entries
            .iter()
            .filter(|entry| entry.target == *target)
            .cloned()
            .collect()
    }

    /// The first of `compose`'s own generic parameters that its *target*
    /// does not pin down -- either because the target is that parameter
    /// (`compose<T: Numeric> T : Sum`, which would apply to every type) or
    /// because the parameter appears nowhere in the target at all
    /// (`compose<T, U: Foo> List<T> : Bar`, whose `U` nothing can ever
    /// bind). Both are blanket composes, deliberately out of scope for now;
    /// a target that uses every parameter (`compose<T> List<T> :
    /// ToIterator<T>`) is not one, and is fully supported.
    fn blanket_parameter(compose: &HirComposeDef) -> Option<Ident> {
        if let Type::Named(path) = &compose.target
            && path.is_unqualified()
            && let Some(generic) = compose
                .generics
                .iter()
                .find(|generic| generic.ident == path.head)
        {
            return Some(generic.ident.clone());
        }
        let mut mentioned = Vec::new();
        Self::collect_type_idents(&compose.target, &mut mentioned);
        compose
            .generics
            .iter()
            .find(|generic| !mentioned.contains(&generic.ident))
            .map(|generic| generic.ident.clone())
    }

    /// Every unqualified identifier a raw `Type` mentions, in source order.
    /// Only used to ask whether a generic parameter occurs in a compose
    /// target, so a qualified path (which can never *be* a parameter) is
    /// deliberately not contributed.
    fn collect_type_idents(r#type: &Type, out: &mut Vec<Ident>) {
        match r#type {
            Type::Named(path) => {
                if path.is_unqualified() {
                    out.push(path.head.clone());
                }
            }
            Type::Pointer(inner, _)
            | Type::UnsizedArray(inner)
            | Type::UnknownSizeArray(inner)
            | Type::SizedArray(inner, _) => Self::collect_type_idents(inner, out),
            Type::Generic(_, args) => {
                for arg in args {
                    Self::collect_type_idents(arg, out);
                }
            }
            _ => {}
        }
    }

    fn match_compose_target(
        compose: &HirComposeDef,
        actual: &ResolvedType,
    ) -> Option<Vec<(Ident, ResolvedType)>> {
        let (actual_name, actual_args) = match actual {
            ResolvedType::Struct(cell) => {
                let cell = cell.borrow();
                (cell.name.clone(), cell.type_args.clone())
            }
            ResolvedType::Enum { cell, .. } => {
                let cell = cell.borrow();
                (cell.name.clone(), cell.type_args.clone())
            }
            ResolvedType::Union(cell) => {
                let cell = cell.borrow();
                (cell.name.clone(), cell.type_args.clone())
            }
            _ => return None,
        };
        let (path, args) = match &compose.target {
            Type::Generic(path, args) => (path, args),
            _ => return None,
        };
        if path.segments().last() != Some(&actual_name) || args.len() != actual_args.len() {
            return None;
        }
        let mut substitution = Vec::new();
        for (raw, concrete) in args.iter().zip(actual_args) {
            let Type::Named(path) = raw else { return None };
            if !path.is_unqualified()
                || !compose
                    .generics
                    .iter()
                    .any(|generic| generic.ident == path.head)
            {
                return None;
            }
            substitution.push((path.head.clone(), concrete));
        }
        Some(substitution)
    }

    pub(crate) fn compose_owner(entry: &ComposeEntry) -> ComposeOwner {
        let spec = entry.spec.borrow();
        ComposeOwner {
            target: entry.target.clone(),
            spec_module_path: spec.module_path.clone(),
            spec_name: spec.name.clone(),
            spec_args: entry.spec_args.clone(),
        }
    }
}
