mod asm;
mod calls;
mod consts;
mod exprs;
mod items;
mod literals;
mod paths;
mod patterns;
mod places;
mod specs;
mod stmts;
mod visibility;

use specs::FlattenedSpecFn;
pub use specs::PendingSpecMethod;

use calls::{CalleeResolution, Intercepted, Interceptor, ResolvedCallee};
use literals::parse_number_literal;

use crate::target::Target;
use crate::{
    checked::{
        CastKind, CheckedAddressOf, CheckedArrayLiteral, CheckedAssignment, CheckedBinaryOp,
        CheckedBlock, CheckedBreak, CheckedCast, CheckedCompoundAssign, CheckedContinue,
        CheckedAsmDescriptor, CheckedAsmDescriptorKind, CheckedDeclaration, CheckedDefer,
        CheckedDynamicCall, CheckedEnumConstruct, CheckedEnumDef, CheckedExpr, CheckedExprNode,
        CheckedExternDeclaration, CheckedField, CheckedFor, CheckedFunctionCall,
        CheckedFunctionDef, CheckedIf, CheckedInlineAsm, CheckedLoop, CheckedMatch,
        CheckedMatchArm, CheckedParam, CheckedPlace, CheckedPlaceRoot, CheckedProjection,
        CheckedRangeEnd, CheckedSlice, CheckedSpecCoerce, CheckedStmt, CheckedStructDef,
        CheckedStructLiteral, CheckedStructLiteralField, CheckedUnionConstruct, CheckedUnionDef,
        CheckedWhile, NumberValue, Storage,
    },
    context::{Context, LexicalScope, VarBinding},
    error::{
        AnalysisError, AnalysisErrorKind, AnalysisWarning, AnalysisWarningKind, TypeResolutionError,
    },
    generics::{resolve_inferred_type_args, unify_generic_type},
    resolved_type::{
        CastClass, ConformanceSource, ConstValue, NumericKind, RawSpecFunctionSig, ResolvedBound,
        ResolvedEnumType, ResolvedEnumVariant, ResolvedField, ResolvedFunctionType, ResolvedMethod,
        ResolvedSpecType, ResolvedStructType, ResolvedType, ResolvedUnionType,
    },
    resolver::{
        GenericLiteralSignature, GenericSignature, GenericStaticFunctionSignature, ImportTarget,
        ItemNamespace, ModuleResolver, OverloadCandidates, ResolveError, ResolveItemOptions,
        ResolvedItem,
    },
    similarity::best_match,
};
use omega_hir::{
    BinaryOp, HirAddressOf, HirAsmDescriptor, HirAsmDescriptorKind, HirBlock, HirCast,
    HirCompoundAssign, HirDeclaration, HirEnumDef, HirExpr, HirExprNode, HirExternDeclaration,
    HirField, HirFor, HirForIn, HirFunctionCall, HirFunctionDef, HirId, HirIf, HirInlineAsm,
    HirItem, HirMatch, HirMatchArm, HirParam, HirPattern, HirPlace, HirPlaceRoot, HirProjection,
    HirRange, HirRangeEnd, HirSlice, HirSpecDef, HirStmt, HirStructDef, HirStructLiteral,
    HirStructLiteralField, HirUnionDef, HirWalrusDeclaration, LogicalOp,
};
use omega_parser::prelude::{
    ExprPath, Ident, NumberBase, NumberExpr, Origin, Path, QualifiedSpecPath, SelfMode, Span, Type,
    Visibility,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

pub struct Analyzer<'r> {
    errors: Vec<AnalysisError>,
    warnings: Vec<AnalysisWarning>,
    context: Context,
    target: Target,
    resolver: &'r mut dyn ModuleResolver,
    module_path: Vec<Ident>,
    current_return_type: ResolvedType,
    loop_stack: Vec<HirId>,
    loops_with_break: HashSet<HirId>,
    in_defer_body: bool,
    in_naked_asm: bool,
    suppressed: Vec<Vec<Ident>>,
    reveals: visibility::RevealState,
    current_owner: Option<HirId>,
    field_usage: crate::dead_code::FieldUsage,
    bounds: Vec<ResolvedBound>,
}

pub fn item_name(item: &HirItem) -> Option<Ident> {
    match item {
        HirItem::Declaration { decl, .. } => Some(decl.ident.clone()),
        HirItem::DeclarationWithInit { decl, .. } => Some(decl.ident.clone()),
        HirItem::Walrus { walrus, .. } => Some(walrus.ident.clone()),
        HirItem::ExternDeclaration(d) => Some(d.ident.clone()),
        HirItem::FunctionDefinition(f) => Some(f.name.clone()),
        HirItem::Struct(s) => Some(s.name.clone()),
        HirItem::Enum(e) => Some(e.name.clone()),
        HirItem::Union(u) => Some(u.name.clone()),
        HirItem::Spec(sp) => Some(sp.name.clone()),
        HirItem::Gap(gap) => Some(gap.name.clone()),
        HirItem::Glue(_) | HirItem::Conform(_) | HirItem::Primitive(_) => None,
        HirItem::Import(_) => None,
    }
}

pub fn item_visibility(item: &HirItem) -> Visibility {
    match item {
        HirItem::Declaration { visibility, .. } => *visibility,
        HirItem::DeclarationWithInit { visibility, .. } => *visibility,
        HirItem::Walrus { visibility, .. } => *visibility,
        HirItem::ExternDeclaration(d) => d.visibility,
        HirItem::FunctionDefinition(f) => f.visibility,
        HirItem::Struct(s) => s.visibility,
        HirItem::Enum(e) => e.visibility,
        HirItem::Union(u) => u.visibility,
        HirItem::Spec(sp) => sp.visibility,
        HirItem::Gap(_) => Visibility::Exposed,
        HirItem::Glue(_) | HirItem::Conform(_) | HirItem::Primitive(_) => {
            unreachable!("unnamed blocks have no item visibility")
        }
        HirItem::Import(_) => {
            unreachable!("imports have no item-level visibility and are never looked up by name")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisSite {
    pub id: HirId,
    pub span: Span,
}

impl AnalysisSite {
    pub const fn new(id: HirId, span: Span) -> Self {
        Self { id, span }
    }
}

impl From<(HirId, Span)> for AnalysisSite {
    fn from((id, span): (HirId, Span)) -> Self {
        Self::new(id, span)
    }
}

pub fn item_site(item: &HirItem) -> AnalysisSite {
    let (id, span) = item_id_span(item);
    AnalysisSite::new(id, span)
}

pub fn item_id_span(item: &HirItem) -> (HirId, Span) {
    match item {
        HirItem::Declaration { decl, .. } => (decl.id, decl.span),
        HirItem::DeclarationWithInit { decl, .. } => (decl.id, decl.span),
        HirItem::Walrus { walrus, .. } => (walrus.id, walrus.span),
        HirItem::ExternDeclaration(d) => (d.id, d.span),
        HirItem::FunctionDefinition(f) => (f.id, f.span),
        HirItem::Struct(s) => (s.id, s.span),
        HirItem::Enum(e) => (e.id, e.span),
        HirItem::Union(u) => (u.id, u.span),
        HirItem::Spec(sp) => (sp.id, sp.span),
        HirItem::Gap(gap) => (gap.id, gap.span),
        HirItem::Glue(glue) => (glue.id, glue.span),
        HirItem::Conform(conform) => (conform.id, conform.span),
        HirItem::Primitive(primitive) => (primitive.id, primitive.span),
        HirItem::Import(i) => (i.id, i.span),
    }
}

impl<'r> Analyzer<'r> {
    pub(super) fn path_module(&self, path: &Path) -> Vec<Ident> {
        self.resolver
            .macro_origin_module(path.origin)
            .unwrap_or_else(|| self.module_path.clone())
    }

    pub(super) fn check_macro_dependency_visibility(
        &mut self,
        id: HirId,
        span: Span,
        path: &Path,
        absolute: &[Ident],
    ) -> bool {
        let Some(macro_visibility) = self.resolver.macro_origin_visibility(path.origin) else {
            return true;
        };
        let Some(item_visibility) = self.resolver.declared_item_visibility(absolute) else {
            return true;
        };
        if item_visibility >= macro_visibility {
            return true;
        }
        self.error(
            id,
            span,
            AnalysisErrorKind::MacroDependencyTooPrivate {
                item: absolute
                    .last()
                    .expect("absolute item path has a name")
                    .clone(),
                macro_visibility,
                item_visibility,
            },
        );
        false
    }

    pub fn new(
        resolver: &'r mut dyn ModuleResolver,
        module_path: Vec<Ident>,
        generics: &[(Ident, ResolvedType)],
        owner: AnalysisSite,
        target: Target,
    ) -> Self {
        Self::new_in(resolver, module_path, generics, &[], owner, target)
    }

    pub fn new_in(
        resolver: &'r mut dyn ModuleResolver,
        module_path: Vec<Ident>,
        generics: &[(Ident, ResolvedType)],
        bounds: &[ResolvedBound],
        owner: AnalysisSite,
        target: Target,
    ) -> Self {
        let mut context = Context::new();
        let mut errors = Vec::new();

        let mut seen_generics = HashSet::new();
        for (ident, resolved_type) in generics {
            let dup = context.current_scope_has_type(ident) || !seen_generics.insert(ident);
            if dup {
                errors.push(AnalysisError::new(
                    owner.id,
                    owner.span,
                    AnalysisErrorKind::Redeclaration {
                        name: ident.clone(),
                        previous: None,
                    },
                ));
            } else {
                context.define_type(ident.clone(), resolved_type.clone());
            }
        }

        Self {
            errors,
            warnings: vec![],
            context,
            resolver,
            module_path,
            target,
            current_return_type: ResolvedType::Void,
            loop_stack: vec![],
            loops_with_break: HashSet::new(),
            in_defer_body: false,
            in_naked_asm: false,
            suppressed: vec![],
            reveals: visibility::RevealState::default(),
            current_owner: None,
            field_usage: crate::dead_code::FieldUsage::default(),
            bounds: bounds.to_vec(),
        }
    }

    pub fn pointer_bytes(&self) -> u32 {
        self.target.pointer_bytes()
    }

    pub fn pointer_bits(&self) -> u32 {
        self.target.pointer_bits()
    }

    pub fn finish(
        self,
    ) -> (
        Vec<AnalysisError>,
        Vec<AnalysisWarning>,
        crate::dead_code::FieldUsage,
    ) {
        (self.errors, self.warnings, self.field_usage)
    }

    fn is_suppressed(&self, kind: &AnalysisWarningKind) -> bool {
        self.suppressed
            .iter()
            .any(|frame| frame.iter().any(|name| name.as_ref() == kind.name()))
    }

    pub(crate) fn error(&mut self, node_id: HirId, span: Span, kind: AnalysisErrorKind) {
        self.errors.push(AnalysisError::new(node_id, span, kind));
    }

    pub(crate) fn warn(&mut self, node_id: HirId, span: Span, kind: AnalysisWarningKind) {
        if !self.is_suppressed(&kind) {
            self.warnings
                .push(AnalysisWarning::new(node_id, span, kind));
        }
    }

    /// `hidden` is the implicit default everywhere except spec members
    /// (which default to their spec's own visibility) -- callers checking a
    /// spec member must gate this on the member's default separately rather
    /// than calling it unconditionally.
    pub(crate) fn check_redundant_hidden(&mut self, node_id: HirId, explicit_hidden_span: Option<Span>) {
        if let Some(span) = explicit_hidden_span {
            self.warn(node_id, span, AnalysisWarningKind::RedundantHiddenModifier);
        }
    }

    pub fn without_diagnostics<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let errors = self.errors.len();
        let warnings = self.warnings.len();
        let result = f(self);
        self.errors.truncate(errors);
        self.warnings.truncate(warnings);
        result
    }

    fn analyze_all<T, U>(
        &mut self,
        items: &[T],
        mut f: impl FnMut(&mut Self, &T) -> Option<U>,
    ) -> Option<Vec<U>> {
        let mut checked = Vec::with_capacity(items.len());
        let mut ok = true;
        for item in items {
            match f(self, item) {
                Some(value) => checked.push(value),
                None => ok = false,
            }
        }
        ok.then_some(checked)
    }

    fn with_scope<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> (T, LexicalScope) {
        self.context.enter_scope();
        let result = f(self);
        let scope = self.context.leave_scope();
        (result, scope)
    }

    fn with_substitution<T>(
        &mut self,
        substitution: &[(Ident, ResolvedType)],
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.with_scope(|this| {
            for (name, ty) in substitution {
                this.context.define_type(name.clone(), ty.clone());
            }
            f(this)
        })
        .0
    }

    fn with_bounds<T>(
        &mut self,
        added: impl IntoIterator<Item = ResolvedBound>,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let original_len = self.bounds.len();
        self.bounds.extend(added);
        let result = f(self);
        self.bounds.truncate(original_len);
        result
    }

    fn with_loop<T>(&mut self, loop_id: HirId, f: impl FnOnce(&mut Self) -> T) -> T {
        self.loop_stack.push(loop_id);
        let result = f(self);
        let popped = self.loop_stack.pop();
        assert_eq!(
            popped,
            Some(loop_id),
            "loop stack must unwind in LIFO order"
        );
        result
    }

    fn with_suppressed<T>(&mut self, names: &[Ident], f: impl FnOnce(&mut Self) -> T) -> T {
        self.suppressed.push(names.to_vec());
        let result = f(self);
        self.suppressed
            .pop()
            .expect("suppression frame just pushed");
        result
    }

    fn with_owner<T>(&mut self, owner: HirId, f: impl FnOnce(&mut Self) -> T) -> T {
        let previous = self.current_owner.replace(owner);
        let result = f(self);
        self.current_owner = previous;
        result
    }

    fn with_defer_body<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let previous = std::mem::replace(&mut self.in_defer_body, true);
        let result = f(self);
        self.in_defer_body = previous;
        result
    }

    fn coerce_to_expected(
        &mut self,
        expected: Option<&ResolvedType>,
        value: CheckedExprNode,
    ) -> CheckedExprNode {
        let Some(
            target @ ResolvedType::SpecObject {
                spec,
                type_args,
                mutable: expected_mutable,
            },
        ) = expected
        else {
            return value;
        };
        if target.accepts(&value.r#type) {
            return value;
        }
        let ResolvedType::Pointer {
            pointee,
            mutable: value_mutable,
        } = &value.r#type
        else {
            return value;
        };
        if !*value_mutable && *expected_mutable {
            return value;
        }
        let Ok(slots) =
            self.type_implements_spec(value.id, value.span, pointee, spec, type_args, true)
        else {
            return value;
        };
        let id = value.id;
        let span = value.span;
        let mut base = value;
        base.id = self.resolver.fresh_synthetic_id();
        CheckedExprNode {
            id,
            span,
            r#type: target.clone(),
            kind: CheckedExpr::SpecCoerce(CheckedSpecCoerce {
                base: Box::new(base),
                slots,
            }),
        }
    }

    fn declare_binding(
        &mut self,
        id: HirId,
        span: Span,
        ident: &Ident,
        origin: Origin,
        r#type: ResolvedType,
        storage: Storage,
        mutable: bool,
    ) -> Option<()> {
        self.declare_binding_impl(
            ident,
            origin,
            VarBinding {
                decl_id: id,
                storage,
                r#type,
                span,
                narrowed: false,
                mutable,
                used: false,
                written: false,
            },
        )
    }

    fn declare_comp_binding(
        &mut self,
        id: HirId,
        span: Span,
        ident: &Ident,
        origin: Origin,
        r#type: ResolvedType,
        value: crate::resolved_type::ConstValue,
    ) -> Option<()> {
        self.declare_binding(id, span, ident, origin, r#type, Storage::Comp, false)?;
        self.context.set_comp_value(id, value);
        Some(())
    }

    fn declare_narrowed_binding(
        &mut self,
        id: HirId,
        span: Span,
        ident: &Ident,
        origin: Origin,
        r#type: ResolvedType,
        storage: Storage,
        mutable: bool,
    ) -> Option<()> {
        self.declare_binding_impl(
            ident,
            origin,
            VarBinding {
                decl_id: id,
                storage,
                r#type,
                span,
                narrowed: true,
                mutable,
                used: false,
                written: false,
            },
        )
    }

    fn declare_binding_impl(
        &mut self,
        ident: &Ident,
        origin: Origin,
        binding: VarBinding,
    ) -> Option<()> {
        let (id, span) = (binding.decl_id, binding.span);
        match self.context.declare(ident.clone(), origin, binding) {
            Ok(()) => Some(()),
            Err((name, previous)) => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::Redeclaration {
                        name,
                        previous: Some(previous),
                    },
                );
                None
            }
        }
    }

    fn resolve_item_checked(
        &mut self,
        absolute: &[Ident],
        type_args: &[ResolvedType],
        indirect: bool,
    ) -> Result<ResolvedItem, ResolveError> {
        let bypass = self.reveals.active();
        let result = self.resolver.resolve_item(
            &self.module_path,
            absolute,
            type_args,
            ResolveItemOptions::with_indirection(indirect).bypassing_visibility(bypass),
        );
        if bypass && result.is_ok() && !self.resolver.is_item_visible(&self.module_path, absolute) {
            self.reveals.mark_used();
        }
        result
    }

    fn resolve_item_checked_with_ambient_fallback(
        &mut self,
        prefix: &[Ident],
        absolute: &[Ident],
        type_args: &[ResolvedType],
    ) -> Result<ResolvedItem, ResolveError> {
        let result = self.resolve_item_checked(absolute, type_args, true);
        match (prefix, &result) {
            ([single], Err(ResolveError::UnknownItem { .. })) => {
                match self
                    .resolver
                    .ambient_core_candidates(&self.module_path, single)?
                {
                    Some(ambient) => self.resolve_item_checked(&ambient, type_args, true),
                    None => result,
                }
            }
            _ => result,
        }
    }

    fn resolve_item_with_ambient_from(
        &mut self,
        accessor: &[Ident],
        prefix: &[Ident],
        absolute: &[Ident],
        type_args: &[ResolvedType],
    ) -> Result<ResolvedItem, ResolveError> {
        let result =
            self.resolver
                .resolve_item(accessor, absolute, type_args, ResolveItemOptions::INDIRECT);
        match (prefix, &result) {
            ([single], Err(ResolveError::UnknownItem { .. })) => {
                match self.resolver.ambient_core_candidates(accessor, single)? {
                    Some(ambient) => self.resolver.resolve_item(
                        accessor,
                        &ambient,
                        type_args,
                        ResolveItemOptions::INDIRECT,
                    ),
                    None => result,
                }
            }
            _ => result,
        }
    }

    pub(crate) fn resolve_type_or_error(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
    ) -> Option<ResolvedType> {
        self.resolve_type_or_error_checked(id, span, typ, indirect, false)
    }

    pub(crate) fn resolve_return_type_or_error(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
    ) -> Option<ResolvedType> {
        self.resolve_type_or_error_checked(id, span, typ, indirect, true)
    }

    pub(crate) fn resolve_return_type_or_error_in(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
        module: &[Ident],
    ) -> Option<ResolvedType> {
        self.resolve_type_or_error_checked_in(id, span, typ, indirect, true, module)
    }

    fn resolve_type_or_error_checked_in(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
        allow_never: bool,
        module: &[Ident],
    ) -> Option<ResolvedType> {
        let resolved = self.resolve_type_or_error_in(id, span, typ, indirect, module)?;
        if let ResolvedType::Spec(spec) = &resolved {
            let name = spec.borrow().name.clone();
            self.error(
                id,
                span,
                AnalysisErrorKind::UnresolvedType(TypeResolutionError::SpecUsedAsValueType(name)),
            );
            return None;
        }
        if !allow_never && resolved == ResolvedType::Never {
            self.error(
                id,
                span,
                AnalysisErrorKind::UnresolvedType(TypeResolutionError::NeverNotAllowedHere),
            );
            return None;
        }
        Some(resolved)
    }

    fn resolve_type_or_error_checked(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
        allow_never: bool,
    ) -> Option<ResolvedType> {
        let resolved = self.resolve_type_or_error_raw(id, span, typ, indirect)?;
        if let ResolvedType::Spec(spec) = &resolved {
            let name = spec.borrow().name.clone();
            self.error(
                id,
                span,
                AnalysisErrorKind::UnresolvedType(TypeResolutionError::SpecUsedAsValueType(name)),
            );
            return None;
        }
        if !allow_never && resolved == ResolvedType::Never {
            self.error(
                id,
                span,
                AnalysisErrorKind::UnresolvedType(TypeResolutionError::NeverNotAllowedHere),
            );
            return None;
        }
        Some(resolved)
    }

    pub(crate) fn resolve_type_or_error_raw(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
    ) -> Option<ResolvedType> {
        let module = self.module_path.clone();
        self.resolve_type_or_error_in(id, span, typ, indirect, &module)
    }

    pub(crate) fn resolve_type_or_error_in(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
        module: &[Ident],
    ) -> Option<ResolvedType> {
        let bypass = self.reveals.active();
        match self.context.resolve_type(
            typ.to_owned(),
            &mut *self.resolver,
            module,
            ResolveItemOptions::with_indirection(indirect).bypassing_visibility(bypass),
        ) {
            Ok(resolved) => Some(resolved),
            Err(err) => {
                self.error(id, span, AnalysisErrorKind::UnresolvedType(err));
                None
            }
        }
    }

    pub fn resolve_under_substitution(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        subst: &[(Ident, ResolvedType)],
    ) -> Option<ResolvedType> {
        self.with_substitution(subst, |this| {
            this.resolve_type_or_error(id, span, typ, false)
        })
    }

    pub(crate) fn infer_generic_args(
        &mut self,
        generics: &[Ident],
        defaults: &[Option<Type>],
        params: &[Type],
        args: &[HirExprNode],
        seed: HashMap<Ident, ResolvedType>,
    ) -> Option<(Vec<CheckedExprNode>, HashMap<Ident, ResolvedType>)> {
        let mut subst = seed;
        let mut checked_args = Vec::with_capacity(args.len());
        for (raw_type, arg) in params.iter().zip(args) {
            let expected = self
                .expected_for_generic_param(arg.id, arg.span, raw_type, generics, defaults, &subst);
            let checked = self.analyze_expr(arg, expected.as_ref())?;
            unify_generic_type(generics, raw_type, &checked.r#type, &mut subst);
            checked_args.push(checked);
        }
        Some((checked_args, subst))
    }

    pub(crate) fn expected_for_generic_param(
        &mut self,
        id: HirId,
        span: Span,
        raw_type: &Type,
        generics: &[Ident],
        defaults: &[Option<Type>],
        subst: &HashMap<Ident, ResolvedType>,
    ) -> Option<ResolvedType> {
        if !Self::generic_refs_resolvable(raw_type, generics, defaults, subst) {
            return None;
        }
        let mut local: Vec<(Ident, ResolvedType)> =
            subst.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (generic, default) in generics.iter().zip(defaults) {
            if subst.contains_key(generic) {
                continue;
            }
            let Some(default) = default else { continue };
            let resolved = self.resolve_under_substitution(id, span, default, &local)?;
            local.push((generic.clone(), resolved));
        }
        self.resolve_under_substitution(id, span, raw_type, &local)
    }

    fn generic_refs_resolvable(
        raw_type: &Type,
        generics: &[Ident],
        defaults: &[Option<Type>],
        subst: &HashMap<Ident, ResolvedType>,
    ) -> bool {
        let name_ok = |name: &Ident| match generics.iter().position(|g| g == name) {
            Some(i) => subst.contains_key(name) || defaults[i].is_some(),
            None => true,
        };
        match raw_type {
            Type::Named(path) => !path.is_unqualified() || name_ok(&path.head),
            Type::Pointer(inner, _)
            | Type::InferredArray(inner)
            | Type::UnknownSizeArray(inner)
            | Type::SizedArray(inner, _) => {
                Self::generic_refs_resolvable(inner, generics, defaults, subst)
            }
            Type::Generic(path, args) => {
                (!path.is_unqualified() || name_ok(&path.head))
                    && args
                        .iter()
                        .all(|a| Self::generic_refs_resolvable(a, generics, defaults, subst))
            }
            Type::SpecObject(inner, _) => {
                Self::generic_refs_resolvable(inner, generics, defaults, subst)
            }
            Type::SpecStatic(inner) => {
                Self::generic_refs_resolvable(inner, generics, defaults, subst)
            }
            Type::Function(f) => {
                f.params
                    .iter()
                    .all(|p| Self::generic_refs_resolvable(&p.r#type, generics, defaults, subst))
                    && Self::generic_refs_resolvable(&f.return_type, generics, defaults, subst)
            }
        }
    }
}
