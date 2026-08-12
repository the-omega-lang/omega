use crate::ast::annotation::AnnotationNode;
use crate::ast::expression::codeblock::CodeblockExpr;
use crate::ast::generics::GenericParam;
use crate::ast::identifier::Ident;
use crate::ast::self_mode::SelfMode;
use crate::ast::statement::declaration::DeclarationStmt;
use crate::ast::r#type::Type;
use crate::ast::visibility::Visibility;

/// A `spec` -- a function-only interface/trait, in one of two surface
/// forms:
///
/// ```text
/// spec Name<T, ...> : Dep1, Dep2 {
///     required(self) => T;
///     with_default(self) => T { self.required() }
/// }
///
/// spec Alias<T, ...> = Dep1 | Dep2;
/// ```
///
/// The declaration form (`:`, with a `{}` body) lists zero or more
/// dependency specs (other specs this one requires/extends -- a type
/// implementing this spec must also satisfy each of them) plus its own
/// function members, each either *required* (no body -- every implementor
/// must provide one) or *default* (a body, using this same `dependencies`
/// syntax for what's available on `self`; overridable per implementor).
///
/// The alias form (`=`, `|`-separated, no body) is pure union sugar for
/// "requires all of these" with no functions of its own -- both forms are
/// carried in the same `dependencies`/`functions` shape (an alias just has
/// `functions: vec![]`), since resolution treats them identically: flatten
/// `dependencies` transitively, then this spec's own `functions`. Kept as
/// two parser-level entry points purely for the clearer `=`/`:` syntax the
/// user sees; see `parser::item::parse_spec_def`.
///
#[derive(Debug, Clone)]
pub struct SpecStmt {
    pub ident: Ident,
    /// `exposed`/`internal`/(default `Hidden`).
    pub visibility: Visibility,
    pub generics: Vec<GenericParam>,
    pub dependencies: Vec<Type>,
    pub functions: Vec<SpecFunctionStmt>,
    /// `true` for the `=`/`|`-separated alias form (`spec Alias = A | B;`),
    /// `false` for the ordinary `:`/`{}` declaration form -- both are
    /// carried in this same struct shape (see the type's own doc comment),
    /// so this is the one thing that actually tells them apart. An alias
    /// has no function list of its own; what it means for conformance is
    /// resolved during analysis (`Analyzer::flatten_spec`), not here.
    pub is_alias: bool,
    /// `@suppress` -- the only annotation a spec accepts (see
    /// `omega_analyzer::annotations::ItemKind::Spec`); validated during
    /// analysis, not parsing.
    pub annotations: Vec<AnnotationNode>,
}

/// One function member of a spec -- `body: None` for a required function
/// (every implementor must provide a matching method, own or default),
/// `body: Some` for a default (used as-is unless a concrete implementor
/// overrides it with its own same-named, same-signature method). `Self` is
/// meaningful inside `params`/`return_type`/`body` here -- see
/// `omega_hir::lower::lower_function_def`'s spec-aware `self`-typing case.
#[derive(Debug, Clone)]
pub struct SpecFunctionStmt {
    pub ident: Ident,
    /// See `FunctionDefinitionStmt::self_mode`. Always `*self`/`*mut self`
    /// (`SelfMode::Pointer`/`MutPointer`) for an ordinary spec function --
    /// by-value self is rejected during spec signature resolution (see
    /// `Analyzer::resolve_spec_functions`), since it can't survive `spec
    /// *T` dynamic dispatch's `Self`-erasure.
    pub self_mode: Option<SelfMode>,
    pub params: Vec<DeclarationStmt>,
    /// A final `...`, matching ordinary function-type variadics.
    pub is_variadic: bool,
    pub return_type: Type,
    pub body: Option<CodeblockExpr>,
}
