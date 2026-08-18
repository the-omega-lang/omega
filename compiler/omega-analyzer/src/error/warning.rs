//! Non-fatal findings: everything worth telling the author about that
//! doesn't reject the program.

use super::*;

/// A non-fatal analysis finding: unlike `AnalysisError`, this never rejects
/// the program (see `Analyzer::analyze`'s return type) -- surfaced to the
/// user as a rendered `warning:` diagnostic, but compilation proceeds.
#[derive(Debug, Clone)]
pub struct AnalysisWarning {
    pub node_id: HirId,
    pub span: Span,
    pub kind: AnalysisWarningKind,
}

impl AnalysisWarning {
    pub fn new(node_id: HirId, span: Span, kind: AnalysisWarningKind) -> Self {
        Self { node_id, span, kind }
    }

    pub fn to_diagnostic(&self) -> Diagnostic {
        let d = Diagnostic::warning(self.kind.to_string());
        let d = match &self.kind {
            AnalysisWarningKind::UnreachableCode => d
                .with_label(self.span, "this can never run")
                .with_note(
                    "it follows something that always diverges (`return`, `break`, `continue`, \
                     a `loop` with no way out, or a call to a `never`-returning function)",
                ),
            AnalysisWarningKind::PreferLoop => d
                .with_label(self.span, "this condition is always true")
                .with_help("use `loop { }` instead -- it also lets the compiler prove this always diverges"),
            AnalysisWarningKind::InlineNotEnforced => d
                .with_label(self.span, "this hint is recorded but not acted on")
                .with_note("this backend has no function-inlining support yet"),
            AnalysisWarningKind::UnusedVariable { name } => {
                d.with_label(self.span, format!("`{}` is never read", name.as_ref()))
            }
            AnalysisWarningKind::UnusedParameter { name } => {
                d.with_label(self.span, format!("`{}` is never read", name.as_ref()))
            }
            AnalysisWarningKind::UnnecessaryMut { name } => d
                .with_label(self.span, format!("`{}` is declared `mut` but never reassigned", name.as_ref()))
                .with_help("remove `mut` -- it isn't just a hint, dropping it changes nothing about how this compiles"),
            AnalysisWarningKind::UnnecessaryReveal => d
                .with_label(self.span, "this 'reveal' never suppresses anything")
                .with_help("remove 'reveal' -- every check inside it would already pass without it"),
            AnalysisWarningKind::UnusedImport { alias } => {
                d.with_label(self.span, format!("`{}` is never referenced in this module", alias.as_ref()))
            }
            AnalysisWarningKind::UnusedField { owner, field } => d.with_label(
                self.span,
                format!("`{}` is never read anywhere `{}` is used", field.as_ref(), owner.as_ref()),
            ),
            AnalysisWarningKind::NeverConstructedVariant { r#enum, variant } => d.with_label(
                self.span,
                format!("`{}` is never built anywhere `{}` is used", variant.as_ref(), r#enum.as_ref()),
            ),
            AnalysisWarningKind::UnusedReturnValue => d
                .with_label(self.span, "this call's result is discarded")
                .with_note("if that's intentional, bind it to a local instead of leaving it a bare statement"),
            AnalysisWarningKind::NoOpCast { r#type } => {
                d.with_label(self.span, format!("`{type}` cast to itself changes nothing"))
            }
            AnalysisWarningKind::SelfAssignment => {
                d.with_label(self.span, "this assigns a value to itself")
            }
            AnalysisWarningKind::AlwaysTrueFalseComparison { result } => d.with_label(
                self.span,
                format!("this comparison is always {result}, no matter the other operand's value"),
            ),
            AnalysisWarningKind::RedundantLayoutAnnotation => d
                .with_label(self.span, "these values are already the default")
                .with_help("remove the explicit arguments, or bare `@layout`, to mean the same thing"),
            AnalysisWarningKind::LargeStructByValue { r#type, size } => d
                .with_label(self.span, format!("`{type}` is at least {size} bytes, passed by value"))
                .with_note("this backend passes structs as flattened scalars, not by reference -- consider a pointer instead"),
            AnalysisWarningKind::UnfilledGap { functions, .. } => d
                .with_label(self.span, "no glue implements this gap anywhere in this compilation")
                .with_note(format!(
                    "missing: {}",
                    functions.iter().map(|f| format!("'{}'", f.as_ref())).collect::<Vec<_>>().join(", ")
                ))
                .with_help("this only matters if something actually calls this gap -- an unglued, uncalled gap links fine"),
        };
        // Always last, after any warning-specific notes above -- a trailing,
        // low-priority hint (mirrors rustc's own `` `#[warn(...)]` on by
        // default `` note) so the warning's own reason stays the focus and
        // this doesn't read as "you should probably suppress this."
        d.with_note(format!("suppress this with '@suppress({})'", self.kind.name()))
    }
}

impl fmt::Display for AnalysisWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

#[derive(Debug, Clone)]
pub enum AnalysisWarningKind {
    /// A statement that can never run: it follows something that
    /// unconditionally diverges (`return`/`break`/`continue`, or an
    /// `if`/`else` where every branch diverges) in the same block. Dropped
    /// before codegen ever sees it (see `Analyzer::analyze_stmts`/
    /// `analyze_block`) rather than risking codegen emitting instructions
    /// into an already-terminated cranelift block.
    UnreachableCode,
    /// A `while` condition that compile-time-folds to the literal `true`
    /// (`while true { }`, `while !false { }`, ...) -- purely cosmetic,
    /// suggesting `loop { }` instead, which reads the same way to a human
    /// but also gets real, provable-divergence treatment `while` never
    /// does (see `Analyzer::stmt_diverges`'s `CheckedStmt::Loop` case).
    /// Deliberately *not* also treated as diverging itself -- only `loop`
    /// is -- so this stays a pure style nudge, not a second, parallel
    /// divergence-detection path to keep in sync with the real one.
    PreferLoop,
    /// A function carries `@inline(always)`/`@inline(never)`, but this
    /// backend has no per-function inlining mechanism to honor it with yet
    /// (see `omega_codegen`) -- `@inline` is purely a hint (per the
    /// language design), so this warns rather than errors, and is
    /// suppressible like any other warning (`@suppress(inline_not_enforced)`).
    InlineNotEnforced,
    /// A local variable (declared with `:=`/`ident: Type;`) that's never
    /// read after its declaration -- tracked live, via `Context::
    /// mark_used`, not by a post-hoc tree walk (see `VarBinding::used`'s
    /// doc comment). A write-only local (assigned but never read back) is
    /// still "unused" in the sense that matters: nothing about the
    /// program's observable behavior depends on it.
    UnusedVariable { name: Ident },
    /// A function/method parameter never read in the body -- same
    /// tracking as `UnusedVariable`, scoped to `check_function_body`'s own
    /// parameter-binding scope. The implicit `self` parameter is
    /// deliberately exempt (see `Analyzer::warn_unused_bindings`'s doc
    /// comment) -- plenty of legitimate methods never touch it.
    UnusedParameter { name: Ident },
    /// A local declared `mut` that's read but never actually reassigned
    /// (via `=`, a compound assignment, `++`/`--`, or `&mut`) after its
    /// initializer -- `mut` was never load-bearing here. Gated on the
    /// local having been *read* at least once (see `VarBinding::used`) so
    /// a completely dead `mut` local only produces `UnusedVariable`, not
    /// both.
    UnnecessaryMut { name: Ident },
    /// A `reveal` expression where every visibility check reached while
    /// analyzing its wrapped expression would have passed anyway -- the
    /// bypass was never load-bearing. See `Analyzer::reveal_stack`'s doc
    /// comment and `HirExpr::Reveal`'s analysis arm.
    UnnecessaryReveal,
    /// A module-level `import` whose alias is never referenced anywhere in
    /// that module -- tracked across the whole module by `Driver`, not by
    /// any one `Analyzer` (imports aren't per-item). See
    /// `Driver::used_imports`'s doc comment.
    UnusedImport { alias: Ident },
    /// A struct/union field, or an enum's own shared dynamic field or a
    /// specific variant's body field, that's never read *anywhere in this
    /// compilation* -- a whole-program sweep (`omega_analyzer::dead_code`),
    /// not a per-module one, since Omega has no visibility/`pub` system
    /// yet to scope "used" any more narrowly. `owner` is the struct's/
    /// union's/enum's own name.
    UnusedField { owner: Ident, field: Ident },
    /// An enum variant that's never constructed anywhere in this
    /// compilation -- the `EnumConstruct` counterpart of `UnusedField`,
    /// same whole-program sweep, same visibility caveat.
    NeverConstructedVariant { r#enum: Ident, variant: Ident },
    /// A non-`void` function call used as a bare statement, its result
    /// silently dropped. Scoped to calls specifically (see
    /// `Analyzer::analyze_stmt`'s expression-statement arm) -- deliberately
    /// unconditional, with no explicit-discard syntax to opt out of it
    /// (decided against introducing one; `@suppress` is the only escape
    /// hatch).
    UnusedReturnValue,
    /// `<T>expr` where `expr` already has resolved type `T` exactly --
    /// `CastKind::Reinterpret` alone isn't enough to detect this (it also
    /// covers meaningfully different same-width conversions, like an
    /// int/uint sign relabel or a pointer mutability change), so this
    /// additionally requires the source and target `ResolvedType`s to be
    /// equal, not just same-width-and-kind.
    NoOpCast { r#type: ResolvedType },
    /// `place = place;` where both sides are the exact same place (same
    /// root variable, same field/index/deref chain) -- see
    /// `Analyzer::places_provably_equal`'s doc comment for what counts as
    /// "provably equal" (a conservative, false-negative-tolerant, never
    /// false-positive-tolerant check).
    SelfAssignment,
    /// A comparison (`< <= > >= == !=`) between a literal and an operand
    /// whose resolved type's value domain (`ResolvedType::integer_domain`)
    /// makes the result a foregone conclusion regardless of the operand's
    /// actual runtime value -- e.g. `unsigned_val < 0` (always `false`) or
    /// `unsigned_val >= 0` (always `true`). `result` is which constant the
    /// comparison always evaluates to.
    AlwaysTrueFalseComparison { result: bool },
    /// `@layout(...)` with at least one argument explicitly given, whose
    /// resolved value is nonetheless identical to `Layout::default()` --
    /// writing `pack = 1`/`align = 1` explicitly does nothing bare
    /// `@layout` (or no annotation at all) doesn't already do.
    RedundantLayoutAnnotation,
    /// A `struct`/`union`/`enum`-typed function/method parameter passed by
    /// value (not `*T`) whose estimated size clears a "this is probably an
    /// accidental copy" threshold -- see `annotations::estimate_byte_size`'s
    /// doc comment for why this is a deliberately approximate lower bound,
    /// not this type's real, `@layout`-aware size.
    LargeStructByValue { r#type: ResolvedType, size: u32 },
    /// A `gap` with no `glue` anywhere in this compilation (local or
    /// `--extern`-visible) implementing it -- deliberately a warning, not an
    /// error (see `docs/language/gaps-and-glue.md`: catching this precisely would
    /// need whole-program reachability analysis through indirect calls,
    /// which this design specifically avoids by leaving it to the linker).
    /// `functions` names every one of the gap's own required functions, so
    /// a later bare linker "undefined reference" (naming the *mangled*
    /// symbol, not this) is traceable back to this warning by the gap name
    /// alone.
    UnfilledGap { gap: Ident, functions: Vec<Ident> },
}

impl AnalysisWarningKind {
    /// A stable, machine-readable slug for `@suppress(...)` to match
    /// against -- deliberately independent of `Display`'s human sentence
    /// (which is free to change wording without breaking anyone's
    /// `@suppress` list) and never validated for existence at the
    /// `@suppress` site (see `annotations::resolve`'s doc comment): a
    /// renamed/removed warning just makes an old suppression silently
    /// inert, not an error.
    pub fn name(&self) -> &'static str {
        match self {
            Self::UnreachableCode => "unreachable_code",
            Self::PreferLoop => "prefer_loop",
            Self::InlineNotEnforced => "inline_not_enforced",
            Self::UnusedVariable { .. } => "unused_variable",
            Self::UnusedParameter { .. } => "unused_parameter",
            Self::UnnecessaryMut { .. } => "unnecessary_mut",
            Self::UnnecessaryReveal => "unnecessary_reveal",
            Self::UnusedImport { .. } => "unused_import",
            Self::UnusedField { .. } => "unused_field",
            Self::NeverConstructedVariant { .. } => "never_constructed_variant",
            Self::UnusedReturnValue => "unused_return_value",
            Self::NoOpCast { .. } => "no_op_cast",
            Self::SelfAssignment => "self_assignment",
            Self::AlwaysTrueFalseComparison { .. } => "always_true_false_comparison",
            Self::RedundantLayoutAnnotation => "redundant_layout_annotation",
            Self::LargeStructByValue { .. } => "large_struct_by_value",
            Self::UnfilledGap { .. } => "unfilled_gap",
        }
    }
}

impl fmt::Display for AnalysisWarningKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnreachableCode => write!(f, "unreachable code"),
            Self::PreferLoop => write!(f, "this `while` condition is always true"),
            Self::InlineNotEnforced => write!(f, "'@inline' is not enforced by this backend yet"),
            Self::UnusedVariable { name } => write!(f, "unused variable '{}'", name.as_ref()),
            Self::UnusedParameter { name } => write!(f, "unused parameter '{}'", name.as_ref()),
            Self::UnnecessaryMut { name } => write!(f, "unnecessary 'mut' on '{}'", name.as_ref()),
            Self::UnnecessaryReveal => write!(f, "unnecessary 'reveal'"),
            Self::UnusedImport { alias } => write!(f, "unused import '{}'", alias.as_ref()),
            Self::UnusedField { owner, field } => {
                write!(f, "field '{}' of '{}' is never read", field.as_ref(), owner.as_ref())
            }
            Self::NeverConstructedVariant { r#enum, variant } => {
                write!(f, "variant '{}' of '{}' is never constructed", variant.as_ref(), r#enum.as_ref())
            }
            Self::UnusedReturnValue => write!(f, "unused return value"),
            Self::NoOpCast { r#type } => write!(f, "this cast to '{type}' has no effect"),
            Self::SelfAssignment => write!(f, "self-assignment"),
            Self::AlwaysTrueFalseComparison { result } => write!(f, "comparison is always {result}"),
            Self::RedundantLayoutAnnotation => write!(f, "redundant '@layout' arguments"),
            Self::LargeStructByValue { r#type, .. } => write!(f, "large type '{type}' passed by value"),
            Self::UnfilledGap { gap, .. } => write!(f, "gap '{}' has no glue implementation", gap.as_ref()),
        }
    }
}
