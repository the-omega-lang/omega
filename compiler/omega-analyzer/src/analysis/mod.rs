mod abi;
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

#[cfg(test)]
mod tests;

use specs::FlattenedSpecFn;
pub use specs::PendingSpecMethod;

use calls::{CalleeResolution, Intercepted, Interceptor, ResolvedCallee};
use literals::parse_number_literal;

use crate::target::Target;
use crate::{
    checked::{
        CastKind, CheckedAddressOf, CheckedAnonymousEnumWiden, CheckedArrayLiteral,
        CheckedAsmDescriptor, CheckedAsmDescriptorKind, CheckedAssignment, CheckedBinaryOp,
        CheckedBlock, CheckedBreak, CheckedCast, CheckedCoercion, CheckedCoercionStep,
        CheckedCompoundAssign, CheckedContinue, CheckedDeclaration, CheckedDefer,
        CheckedDynamicCall, CheckedEnumConstruct, CheckedEnumDef, CheckedExpr, CheckedExprNode,
        CheckedField, CheckedFor, CheckedForeignFunctionDef, CheckedFunctionCall,
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
        CallingConvention, CastClass, ConformanceSource, ConstValue, FunctionNamespace,
        NumericKind, RawSpecFunctionSig, ResolvedAnonymousEnum, ResolvedBound, ResolvedEnumType,
        ResolvedEnumVariant, ResolvedField, ResolvedFunctionParam, ResolvedFunctionType,
        ResolvedMethod, ResolvedSpecType, ResolvedStructType, ResolvedType, ResolvedUnionType,
    },
    resolver::{
        GenericLiteralSignature, GenericOwnerFunctionSignature, GenericSignature, ImportTarget,
        ItemAccess, ItemNamespace, ModuleResolver, ResolveError, ResolveItemOptions, ResolvedItem,
        ResolvedOverloadSet,
    },
    similarity::best_match,
};
use omega_hir::{
    BinaryOp, HirAddressOf, HirAsmDescriptor, HirAsmDescriptorKind, HirBlock, HirCast,
    HirCompoundAssign, HirDeclaration, HirEnumDef, HirExpr, HirExprNode, HirField, HirFor,
    HirForIn, HirFunctionCall, HirFunctionDef, HirId, HirIf, HirInlineAsm, HirItem, HirMatch,
    HirMatchArm, HirParam, HirPattern, HirPatternValue, HirPlace, HirPlaceRoot, HirProjection,
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
        HirItem::ForeignBinding(d) => Some(d.ident.clone()),
        HirItem::ForeignFunction(f) => Some(f.name.clone()),
        HirItem::FunctionDefinition(f) => Some(f.name.clone()),
        HirItem::Struct(s) => Some(s.name.clone()),
        HirItem::Enum(e) => Some(e.name.clone()),
        HirItem::Union(u) => Some(u.name.clone()),
        HirItem::Spec(sp) => Some(sp.name.clone()),
        HirItem::Gap(gap) => Some(gap.name.clone()),
        HirItem::Glue(_) | HirItem::Conform(_) | HirItem::Primitive(_) => None,
        HirItem::Import(_) => None,
        // An alias declares a name but never a concrete item; the driver
        // indexes aliases in their own namespace-transparent table.
        HirItem::Alias(_) => None,
    }
}

pub fn item_visibility(item: &HirItem) -> Visibility {
    match item {
        HirItem::Declaration { visibility, .. } => *visibility,
        HirItem::DeclarationWithInit { visibility, .. } => *visibility,
        HirItem::Walrus { visibility, .. } => *visibility,
        HirItem::ForeignBinding(d) => d.visibility,
        HirItem::ForeignFunction(f) => f.visibility,
        HirItem::FunctionDefinition(f) => f.visibility,
        HirItem::Struct(s) => s.visibility,
        HirItem::Enum(e) => e.visibility,
        HirItem::Union(u) => u.visibility,
        HirItem::Spec(sp) => sp.visibility,
        HirItem::Gap(_) => Visibility::Exposed,
        HirItem::Alias(alias) => alias.visibility,
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
        HirItem::ForeignBinding(d) => (d.id, d.span),
        HirItem::ForeignFunction(f) => (f.id, f.span),
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
        HirItem::Alias(alias) => (alias.id, alias.span),
    }
}

/// What a written path's explicit `root::`/`self::`/`super::` anchor says,
/// resolved from the path's own resolution module.
pub(super) enum AnchoredPath {
    Absolute(Vec<Ident>),
    /// No explicit anchor: the caller's own unanchored rules apply.
    Unanchored,
    /// The anchor itself is illegal (`super::` above the package root); the
    /// diagnostic is already reported.
    Failed,
}

/// Whether a written qualified path resolves through a module binding.
/// The returned access always names the final item through the canonical
/// physical module path; module aliases are visibility gates, not modules
/// with a second identity.
pub(super) enum ModuleQualifiedPath {
    Item(ItemAccess),
    NotModule,
    Failed,
}

impl<'r> Analyzer<'r> {
    pub(super) fn path_module(&self, path: &Path) -> Vec<Ident> {
        self.resolver
            .macro_origin_module(path.origin)
            .unwrap_or_else(|| self.module_path.clone())
    }

    /// Resolves `path`'s explicit anchor, if it has one. Every item-like
    /// path consults this *before* asking whether its head is an import, so
    /// an anchored spelling never degrades into an import lookup for a name
    /// that was never meant to be one.
    pub(super) fn anchored_path(
        &mut self,
        node_id: HirId,
        span: Span,
        path: &Path,
    ) -> AnchoredPath {
        let module = self.path_module(path);
        match self.resolver.resolve_explicit_anchor(&module, path) {
            None => AnchoredPath::Unanchored,
            Some(Ok(absolute)) => AnchoredPath::Absolute(absolute),
            Some(Err(error)) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                AnchoredPath::Failed
            }
        }
    }

    /// The same question for a leading *prefix* of an anchored path, used
    /// where generic arguments or a trailing member split the path in two.
    pub(super) fn anchored_prefix(
        &mut self,
        node_id: HirId,
        span: Span,
        path: &Path,
        prefix: &[Ident],
    ) -> AnchoredPath {
        let Some((head, tail)) = prefix.split_first() else {
            return AnchoredPath::Unanchored;
        };
        self.anchored_path(
            node_id,
            span,
            &Path {
                anchor: path.anchor,
                head: head.clone(),
                tail: tail.to_vec(),
                origin: path.origin,
            },
        )
    }

    /// Resolves the module-qualified reading of `path`, including module
    /// aliases in any module segment. A path that does not resolve through a
    /// module binding is left to the caller's type-qualified interpretation.
    pub(super) fn module_qualified_path(
        &mut self,
        node_id: HirId,
        span: Span,
        path: &Path,
    ) -> ModuleQualifiedPath {
        let accessor = self.path_module(path);
        let absolute = match self.anchored_path(node_id, span, path) {
            AnchoredPath::Failed => return ModuleQualifiedPath::Failed,
            AnchoredPath::Absolute(absolute) => absolute,
            AnchoredPath::Unanchored => {
                if path.is_unqualified() {
                    return ModuleQualifiedPath::NotModule;
                }
                match self.resolver.resolve_import_alias(&accessor, &path.head) {
                    Ok(Some(ImportTarget::Module(target))) => target
                        .into_iter()
                        .chain(path.tail.iter().cloned())
                        .collect(),
                    Ok(_) => return ModuleQualifiedPath::NotModule,
                    Err(error) => {
                        self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                        return ModuleQualifiedPath::Failed;
                    }
                }
            }
        };

        // An explicitly anchored single item (`self::VALUE`) has no module
        // segment from the written path to discriminate: the anchor already
        // supplied its containing module.
        if path.anchor.is_some() && path.tail.is_empty() {
            return ModuleQualifiedPath::Item(ItemAccess::gated(absolute));
        }

        let Some((item, module)) = absolute.split_last() else {
            return ModuleQualifiedPath::NotModule;
        };
        match self.resolver.resolve_module_path(&accessor, module) {
            Ok(Some(module)) => ModuleQualifiedPath::Item(ItemAccess::gated(
                module
                    .into_iter()
                    .chain(std::iter::once(item.clone()))
                    .collect(),
            )),
            Ok(None) => ModuleQualifiedPath::NotModule,
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                ModuleQualifiedPath::Failed
            }
        }
    }

    /// Recovers a module-headed reading of `path` for the one case
    /// `module_qualified_path` deliberately declines: a module named by
    /// `path.head` alone (through an anchor or an import), with a type and
    /// its member still nested in the remaining tail (`module::Type::member`).
    /// `module_qualified_path` requires the *whole* prefix up to the last
    /// segment to be a module, which is wrong here since `Type` is not a
    /// module segment. Returns `Some(None)` when the head does not name a
    /// module at all, so the caller falls back to its type-headed reading;
    /// `None` means an error was already reported.
    pub(super) fn module_headed_path(
        &mut self,
        node_id: HirId,
        span: Span,
        path: &Path,
    ) -> Option<Option<ItemAccess>> {
        if path.tail.is_empty() {
            return Some(None);
        }
        let accessor = self.path_module(path);
        let head_absolute =
            match self.anchored_prefix(node_id, span, path, std::slice::from_ref(&path.head)) {
                AnchoredPath::Failed => return None,
                AnchoredPath::Absolute(absolute) => absolute,
                AnchoredPath::Unanchored => {
                    match self.resolve_path_alias_or_error(node_id, span, path)? {
                        Some(ImportTarget::Module(target)) => target,
                        _ => return Some(None),
                    }
                }
            };
        match self.resolver.resolve_module_path(&accessor, &head_absolute) {
            Ok(Some(module)) => Some(Some(ItemAccess::gated(
                module
                    .into_iter()
                    .chain(path.tail.iter().cloned())
                    .collect(),
            ))),
            Ok(None) => Some(None),
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                None
            }
        }
    }

    /// Canonicalizes only the module prefix of an already-absolute item
    /// access. If that prefix is type-qualified rather than module-qualified,
    /// the access is returned unchanged so the caller can try its other
    /// interpretation.
    pub(super) fn canonicalize_item_access(
        &mut self,
        node_id: HirId,
        span: Span,
        accessor: &[Ident],
        access: ItemAccess,
    ) -> Option<ItemAccess> {
        let Some((item, module)) = access.absolute.split_last() else {
            return Some(access);
        };
        let item = item.clone();
        let module = module.to_vec();
        match self.resolver.resolve_module_path(accessor, &module) {
            Ok(Some(module)) => Some(ItemAccess {
                absolute: module.into_iter().chain(std::iter::once(item)).collect(),
                bypass_visibility: access.bypass_visibility,
            }),
            Ok(None) => Some(access),
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                None
            }
        }
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
        let mut context = Context::new(target);
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
    pub(crate) fn check_redundant_hidden(
        &mut self,
        node_id: HirId,
        explicit_hidden_span: Option<Span>,
    ) {
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
        let Some(target) = expected else {
            return value;
        };
        match self.plan_coercion(value.id, value.span, target, &value.r#type) {
            Some(plan) => self.apply_coercion(plan, value),
            None => value,
        }
    }

    /// Decides how `found` can inhabit `expected`, without owning a value to
    /// convert. `Some` with no steps means plain acceptance; `None` means no
    /// conversion exists and the caller reports the mismatch.
    ///
    /// This is the single home of "does this value fit that type": an
    /// expected type, a `<enum ...>` cast, and `?`'s error propagation are
    /// three ways to establish a destination, and they must agree on the
    /// answer and on the IR.
    pub(crate) fn plan_coercion(
        &mut self,
        id: HirId,
        span: Span,
        expected: &ResolvedType,
        found: &ResolvedType,
    ) -> Option<CheckedCoercion> {
        if expected.accepts(found) {
            return Some(CheckedCoercion::default());
        }
        if let Some(steps) = Self::plan_anonymous_conversion(expected, found) {
            return Some(CheckedCoercion { steps });
        }
        // A refined read also satisfies a plain expected type: the proof names
        // the member, so the site reads it out of the unchanged storage.
        if let Some((variant_index, member)) = found.refined_anonymous_member()
            && expected.accepts(member)
        {
            return Some(CheckedCoercion {
                steps: vec![CheckedCoercionStep::ProjectAnonymousMember {
                    variant_index,
                    member_type: member.clone(),
                }],
            });
        }
        let ResolvedType::SpecObject {
            shape,
            mutable: expected_mutable,
        } = expected
        else {
            return None;
        };
        let ResolvedType::Pointer {
            pointee,
            mutable: value_mutable,
        } = found
        else {
            return None;
        };
        if !*value_mutable && *expected_mutable {
            return None;
        }
        let slots = self.type_implements_shape(id, span, pointee, shape).ok()?;
        Some(CheckedCoercion {
            steps: vec![CheckedCoercionStep::SpecCoerce {
                slots,
                target_type: expected.clone(),
            }],
        })
    }

    /// The steps that make `found` inhabit the anonymous enum `target`, or
    /// `None` when it cannot. Both conversions are real representation
    /// changes, so each leaves an explicit step rather than being spelled as
    /// an acceptance rule.
    fn plan_anonymous_conversion(
        target: &ResolvedType,
        found: &ResolvedType,
    ) -> Option<Vec<CheckedCoercionStep>> {
        let ResolvedType::AnonymousEnum {
            shape: target_shape,
            variant: None,
        } = target
        else {
            return None;
        };
        // Dropping a refinement of this very shape is a plain copy, so the
        // parent type is used as-is rather than unpacked and repacked.
        if target.accepts(found) {
            return Some(Vec::new());
        }
        // Projection runs first so a proven leaf can be injected into a
        // *different* anonymous enum in one step.
        let mut steps = Vec::new();
        let mut current = found;
        if let Some((variant_index, member)) = found.refined_anonymous_member()
            && (target.accepts(member)
                || Self::anonymous_member_index(target_shape, member).is_some())
        {
            steps.push(CheckedCoercionStep::ProjectAnonymousMember {
                variant_index,
                member_type: member.clone(),
            });
            current = member;
        }
        if let Some(variant_index) = Self::anonymous_member_index(target_shape, current) {
            steps.push(CheckedCoercionStep::InjectAnonymousMember {
                variant_index,
                target_type: target.clone(),
            });
            return Some(steps);
        }
        let ResolvedType::AnonymousEnum {
            shape: source_shape,
            variant: None,
        } = current
        else {
            return None;
        };
        let variant_map = target_shape.subset_remap(source_shape)?;
        steps.push(CheckedCoercionStep::WidenAnonymousEnum {
            variant_map,
            target_type: target.clone(),
        });
        Some(steps)
    }

    /// Runs an already-decided plan over a value the caller owns.
    pub(crate) fn apply_coercion(
        &mut self,
        plan: CheckedCoercion,
        mut value: CheckedExprNode,
    ) -> CheckedExprNode {
        for step in plan.steps {
            value = self.apply_coercion_step(step, value);
        }
        value
    }

    fn apply_coercion_step(
        &mut self,
        step: CheckedCoercionStep,
        value: CheckedExprNode,
    ) -> CheckedExprNode {
        let id = value.id;
        let span = value.span;
        match step {
            CheckedCoercionStep::ProjectAnonymousMember {
                variant_index,
                member_type,
            } => {
                let projection = CheckedProjection::EnumBody {
                    variant_index,
                    field_index: 0,
                    r#type: member_type.clone(),
                };
                let place = match value.kind {
                    CheckedExpr::Place(mut place) => {
                        place.projections.push(projection);
                        place.r#type = member_type.clone();
                        place
                    }
                    _ => {
                        let mut base = value;
                        base.id = self.resolver.fresh_synthetic_id();
                        CheckedPlace {
                            root: CheckedPlaceRoot::Expr(Box::new(base)),
                            projections: vec![projection],
                            r#type: member_type.clone(),
                        }
                    }
                };
                CheckedExprNode {
                    id,
                    span,
                    r#type: member_type,
                    kind: CheckedExpr::Place(place),
                }
            }
            CheckedCoercionStep::InjectAnonymousMember {
                variant_index,
                target_type,
            } => {
                // Injecting a constant yields a constant: the tagged value is
                // fully known, and folding it here keeps compile-time-only
                // positions (globals, `comp` bindings) on the one shared enum
                // constant representation instead of needing a second packer.
                if let CheckedExpr::Const(member) = value.kind {
                    return CheckedExprNode {
                        id,
                        span,
                        r#type: target_type,
                        kind: CheckedExpr::Const(ConstValue::anonymous_enum(
                            variant_index,
                            vec![member],
                        )),
                    };
                }
                let mut member = value;
                member.id = self.resolver.fresh_synthetic_id();
                CheckedExprNode {
                    id,
                    span,
                    r#type: target_type,
                    kind: CheckedExpr::EnumConstruct(CheckedEnumConstruct {
                        variant_index,
                        fields: vec![CheckedStructLiteralField {
                            field_index: 0,
                            value: member,
                        }],
                    }),
                }
            }
            CheckedCoercionStep::WidenAnonymousEnum {
                variant_map,
                target_type,
            } => {
                // Widening a constant yields a constant, for the same reason
                // injecting one does.
                if let CheckedExpr::Const(ConstValue::Enum {
                    variant_index,
                    fields,
                    ..
                }) = value.kind
                {
                    return CheckedExprNode {
                        id,
                        span,
                        r#type: target_type,
                        kind: CheckedExpr::Const(ConstValue::anonymous_enum(
                            variant_map[variant_index],
                            fields,
                        )),
                    };
                }
                let mut source = value;
                source.id = self.resolver.fresh_synthetic_id();
                CheckedExprNode {
                    id,
                    span,
                    r#type: target_type,
                    kind: CheckedExpr::AnonymousEnumWiden(CheckedAnonymousEnumWiden {
                        source: Box::new(source),
                        variant_map,
                    }),
                }
            }
            CheckedCoercionStep::SpecCoerce { slots, target_type } => {
                let mut base = value;
                base.id = self.resolver.fresh_synthetic_id();
                CheckedExprNode {
                    id,
                    span,
                    r#type: target_type,
                    kind: CheckedExpr::SpecCoerce(CheckedSpecCoerce {
                        base: Box::new(base),
                        slots,
                    }),
                }
            }
        }
    }

    /// What `coerce_to_expected` would have to do to make `found` acceptable
    /// where `expected` is wanted, as a ranking penalty: `0` for plain
    /// acceptance, higher for an anonymous-enum conversion, `None` when no
    /// conversion exists. Overload viability asks this so it can never
    /// disagree with the coercion that actually runs.
    pub(crate) fn conversion_cost(expected: &ResolvedType, found: &ResolvedType) -> Option<u32> {
        const ANONYMOUS: u32 = 2;

        if expected.accepts(found) {
            return Some(0);
        }
        if let Some((_, member)) = found.refined_anonymous_member()
            && (expected.accepts(member) || Self::converts_to_anonymous(expected, member))
        {
            return Some(ANONYMOUS);
        }
        Self::converts_to_anonymous(expected, found).then_some(ANONYMOUS)
    }

    /// Whether an unrefined `found` can be converted into the anonymous enum
    /// `expected`: it is either one of its members, or an anonymous enum
    /// whose every member is. Shared with `convert_to_anonymous_enum` so
    /// overload viability and real coercion cannot disagree.
    fn converts_to_anonymous(expected: &ResolvedType, found: &ResolvedType) -> bool {
        let ResolvedType::AnonymousEnum {
            shape,
            variant: None,
        } = expected
        else {
            return false;
        };
        if Self::anonymous_member_index(shape, found).is_some() {
            return true;
        }
        matches!(
            found,
            ResolvedType::AnonymousEnum { shape: source, variant: None }
                if shape.subset_remap(source).is_some()
        )
    }

    /// The canonical member index an exact value type injects into.
    /// Refinement is not part of a value's representation, so a refined
    /// variant value injects as its parent member. Nothing else is tried: the
    /// expected type is never solved as a disjunction of its members.
    pub(crate) fn anonymous_member_index(
        shape: &ResolvedAnonymousEnum,
        value: &ResolvedType,
    ) -> Option<usize> {
        shape
            .index_of(value)
            .or_else(|| shape.index_of(&value.widened()))
    }

    /// Converts `value` into the already-resolved anonymous enum `target`,
    /// or hands it back untouched when no conversion exists. An expected
    /// type and a `<enum ...>` cast are two ways to establish the
    /// destination, so both route through `plan_anonymous_conversion`.
    pub(crate) fn convert_to_anonymous_enum(
        &mut self,
        target: &ResolvedType,
        value: CheckedExprNode,
    ) -> Result<CheckedExprNode, CheckedExprNode> {
        match Self::plan_anonymous_conversion(target, &value.r#type) {
            Some(steps) => Ok(self.apply_coercion(CheckedCoercion { steps }, value)),
            None => Err(value),
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
        access: &ItemAccess,
        type_args: &[ResolvedType],
        indirect: bool,
    ) -> Result<ResolvedItem, ResolveError> {
        let bypass = self.reveals.active();
        let options = access
            .options(ResolveItemOptions::with_indirection(indirect).bypassing_visibility(bypass));
        let result =
            self.resolver
                .resolve_item(&self.module_path, &access.absolute, type_args, options);
        if bypass
            && result.is_ok()
            && !self
                .resolver
                .is_item_visible(&self.module_path, &access.absolute)
        {
            self.reveals.mark_used();
        }
        result
    }

    fn resolve_item_checked_with_ambient_fallback(
        &mut self,
        prefix: &[Ident],
        access: &ItemAccess,
        type_args: &[ResolvedType],
    ) -> Result<ResolvedItem, ResolveError> {
        let result = self.resolve_item_checked(access, type_args, true);
        match (prefix, &result) {
            ([single], Err(ResolveError::UnknownItem { .. })) => {
                match self
                    .resolver
                    .ambient_core_candidates(&self.module_path, single)?
                {
                    Some(ambient) => {
                        self.resolve_item_checked(&ItemAccess::gated(ambient), type_args, true)
                    }
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
        access: &ItemAccess,
        type_args: &[ResolvedType],
    ) -> Result<ResolvedItem, ResolveError> {
        let options = access.options(ResolveItemOptions::INDIRECT);
        let result = self
            .resolver
            .resolve_item(accessor, &access.absolute, type_args, options);
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

    /// The function's signature after static-spec parameter normalization.
    /// Every signature and body query works from this shape, so a literal
    /// `spec A + B` parameter and an aliased one cannot diverge.
    pub(crate) fn normalized_function(&mut self, f: &HirFunctionDef) -> Option<HirFunctionDef> {
        match crate::generics::normalize_static_spec_params(
            &mut *self.resolver,
            &self.module_path,
            f,
        ) {
            Ok(normalized) => Some(normalized),
            Err(error) => {
                self.error(f.id, f.span, AnalysisErrorKind::ModuleResolution(error));
                None
            }
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

    /// Checks the bounds every alias template applied by `typ` declares on
    /// its own generic parameters. Normalization is structural and cannot do
    /// this itself: whether an argument satisfies a bound is a conformance
    /// question, and the expanded target no longer mentions the alias's
    /// parameter list.
    fn check_alias_generic_bounds(&mut self, id: HirId, span: Span, typ: &Type, module: &[Ident]) {
        let applied = match crate::aliases::applied_alias_bounds(&mut *self.resolver, module, typ) {
            Ok(applied) => applied,
            Err(error) => {
                self.error(id, span, AnalysisErrorKind::ModuleResolution(error));
                return;
            }
        };
        for (bound, argument) in applied {
            let Some(concrete) = self.resolve_type_or_error_in(id, span, &argument, true, module)
            else {
                continue;
            };
            if let Some(Err((spec, missing))) =
                self.check_generic_bound(id, span, &bound, &concrete)
            {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::ModuleResolution(ResolveError::SpecNotImplemented {
                        type_name: concrete.to_string(),
                        spec,
                        missing,
                    }),
                );
            }
        }
    }

    pub(crate) fn resolve_type_or_error_in(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
        module: &[Ident],
    ) -> Option<ResolvedType> {
        self.check_alias_generic_bounds(id, span, typ, module);
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
            Type::SpecStatic(members) | Type::AnonymousEnum(members) => members
                .iter()
                .all(|m| Self::generic_refs_resolvable(m, generics, defaults, subst)),
            Type::Function(f) => {
                f.params
                    .iter()
                    .all(|p| Self::generic_refs_resolvable(&p.r#type, generics, defaults, subst))
                    && Self::generic_refs_resolvable(&f.return_type, generics, defaults, subst)
            }
        }
    }
}
