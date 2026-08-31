use crate::ast::annotation::AnnotationNode;
use crate::ast::expression::{CodeblockExpr, ExpressionNode, MacroInvocationExpr};
use crate::ast::generics::GenericParam;
use crate::ast::identifier::{Ident, Path};
use crate::ast::self_mode::SelfMode;
use crate::ast::statement::{DeclarationStmt, FunctionDefinitionStmt, WalrusStmt};
use crate::ast::r#type::{FunctionType, Param, RawConvention, Type};
use crate::ast::visibility::Visibility;
use crate::diagnostics::Span;
use crate::lexer::Token;

#[derive(Debug, Clone)]
pub enum Item {
    Declaration(DeclarationStmt),
    DeclarationWithInit(DeclarationStmt, ExpressionNode),
    ForeignBinding(ForeignBindingItem),
    ForeignFunction(ForeignFunctionItem),
    ForeignBlock(ForeignBlockItem),
    FunctionDefinition(FunctionDefinitionStmt),
    Struct(StructStmt),
    Enum(EnumStmt),
    Union(UnionStmt),
    Spec(SpecStmt),
    Gap(GapStmt),
    Glue(GlueStmt),
    Conform(ConformStmt),
    Primitive(PrimitiveStmt),
    Walrus(WalrusStmt),
    Import(ImportStmt),
    MacroDefinition(MacroDefinitionStmt),
    MacroInvocation(MacroInvocationExpr),
    Alias(AliasItem),
}

/// `alias Name<G...> = <target>;` -- a compile-time-only second source name
/// for an existing declaration. The parser never classifies the target; it
/// only records whether it was written as a bare path (which may name any
/// namespace) or as some other type syntax.
#[derive(Debug, Clone)]
pub struct AliasItem {
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub ident: Ident,
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    pub target: AliasTarget,
    pub target_span: Span,
}

#[derive(Debug, Clone)]
pub enum AliasTarget {
    Path(Path),
    Type(Type),
}

#[derive(Debug, Clone)]
pub struct ItemNode {
    pub item: Item,
    pub span: Span,
}

/// `foreign name : Type;` -- binds an external symbol. Any function
/// convention comes from `Type` itself; a block's convention never applies
/// here (see `ForeignBlockItem`).
#[derive(Debug, Clone)]
pub struct ForeignBindingItem {
    pub annotations: Vec<AnnotationNode>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub ident: Ident,
    pub name_span: Span,
    pub r#type: Type,
}

/// A direct foreign function declaration/definition:
/// `foreign(cc) name(args) => T;` or `... { body }`. `convention` is `None`
/// for the bare Omega-convention form (`foreign name(args) => T;`).
#[derive(Debug, Clone)]
pub struct ForeignFunctionItem {
    pub annotations: Vec<AnnotationNode>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub convention: Option<RawConvention>,
    pub ident: Ident,
    pub name_span: Span,
    pub signature_span: Span,
    pub return_type_span: Span,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub is_variadic: bool,
    pub return_type: Type,
    pub body: Option<CodeblockExpr>,
}

impl ForeignFunctionItem {
    pub fn function_type(&self) -> FunctionType {
        FunctionType {
            params: self
                .params
                .iter()
                .map(Param::as_function_type_param)
                .collect(),
            return_type: Box::new(self.return_type.clone()),
            is_variadic: self.is_variadic,
            self_mode: None,
            convention: self.convention.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ForeignBlockEntry {
    Binding(ForeignBindingItem),
    Function(ForeignFunctionItem),
}

/// `foreign(cc) { ... }` -- syntactic grouping only, flattened away before
/// semantic analysis. `cc` applies to direct function-signature entries in
/// `entries`; a `name : Type;` binding entry always ignores it.
#[derive(Debug, Clone)]
pub struct ForeignBlockItem {
    pub convention: Option<RawConvention>,
    pub entries: Vec<ForeignBlockEntry>,
}

#[derive(Debug, Clone)]
pub struct StructStmt {
    pub annotations: Vec<AnnotationNode>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub ident: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<DeclarationStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
    pub is_marker: bool,
}

#[derive(Debug, Clone)]
pub struct UnionStmt {
    pub annotations: Vec<AnnotationNode>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub ident: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<DeclarationStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
}

#[derive(Debug, Clone)]
pub struct EnumStmt {
    pub annotations: Vec<AnnotationNode>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub ident: Ident,
    pub generics: Vec<GenericParam>,
    pub header: Vec<EnumHeaderField>,
    pub dynamic_fields: Vec<DeclarationStmt>,
    pub variants: Vec<EnumVariantStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
}

#[derive(Debug, Clone)]
pub struct EnumHeaderField {
    pub ident: Ident,
    pub name_span: Span,
    pub r#type: Type,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumVariantStmt {
    pub ident: Ident,
    pub span: Span,
    pub args: Vec<ExpressionNode>,
    pub fields: Vec<DeclarationStmt>,
}

#[derive(Debug, Clone)]
pub struct SpecStmt {
    pub ident: Ident,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub generics: Vec<GenericParam>,
    pub functions: Vec<SpecFunctionStmt>,
    pub annotations: Vec<AnnotationNode>,
}

#[derive(Debug, Clone)]
pub struct SpecFunctionStmt {
    pub ident: Ident,
    pub name_span: Span,
    pub signature_span: Span,
    pub return_type_span: Span,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub generics: Vec<GenericParam>,
    pub self_mode: Option<SelfMode>,
    pub params: Vec<Param>,
    pub is_variadic: bool,
    pub return_type: Type,
    pub body: Option<CodeblockExpr>,
}

#[derive(Debug, Clone)]
pub struct GapStmt {
    pub ident: Ident,
    pub functions: Vec<SpecFunctionStmt>,
}

#[derive(Debug, Clone)]
pub struct GlueStmt {
    pub gap: Path,
    pub functions: Vec<FunctionDefinitionStmt>,
}

#[derive(Debug, Clone)]
pub struct ConformStmt {
    pub generics: Vec<GenericParam>,
    pub target: Type,
    pub spec: Type,
    pub functions: Vec<FunctionDefinitionStmt>,
}

#[derive(Debug, Clone)]
pub struct PrimitiveStmt {
    pub generics: Vec<GenericParam>,
    pub target: Type,
    pub functions: Vec<FunctionDefinitionStmt>,
}

/// An `import` item, kept in the shape it was written: a root prefix plus a
/// tree of brace groups beneath it. Nothing here is resolved; [`Self::leaves`]
/// is the one place the tree is turned into the flat bindings it denotes.
#[derive(Debug, Clone)]
pub struct ImportStmt {
    pub annotations: Vec<AnnotationNode>,
    pub reveal: bool,
    pub path: Path,
    /// The whole `import ... ;` statement, which is also the leaf span when
    /// the root prefix is itself the only binding.
    pub span: Span,
    pub kind: ImportKind,
}

#[derive(Debug, Clone)]
pub enum ImportKind {
    /// A terminal binding, renamed by `as` when the alias is present.
    Leaf {
        alias: Option<Ident>,
    },
    Group(Vec<ImportNode>),
}

/// One entry of a brace group.
#[derive(Debug, Clone)]
pub struct ImportNode {
    pub reveal: bool,
    /// Segments appended to the enclosing prefix. Empty for the `self` entry,
    /// which binds the enclosing prefix itself.
    pub segments: Vec<Ident>,
    pub span: Span,
    pub kind: ImportKind,
}

/// One terminal binding an import tree denotes: the complete target path, the
/// name it binds locally, and the `reveal` it inherited from its ancestors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportLeaf {
    pub path: Path,
    pub name: Ident,
    pub reveal: bool,
    pub span: Span,
}

impl ImportStmt {
    /// Every binding this import makes, in textual depth-first order. This is
    /// a structural view of the written syntax: it appends nested prefixes,
    /// inherits `reveal` down each branch, and derives local names, but it
    /// resolves nothing and checks no visibility.
    pub fn leaves(&self) -> Vec<ImportLeaf> {
        let mut leaves = Vec::new();
        collect_import_leaves(&self.path, self.reveal, &self.kind, self.span, &mut leaves);
        leaves
    }
}

fn collect_import_leaves(
    prefix: &Path,
    reveal: bool,
    kind: &ImportKind,
    span: Span,
    leaves: &mut Vec<ImportLeaf>,
) {
    match kind {
        ImportKind::Leaf { alias } => leaves.push(ImportLeaf {
            name: alias
                .clone()
                .unwrap_or_else(|| prefix.tail.last().unwrap_or(&prefix.head).clone()),
            path: prefix.clone(),
            reveal,
            span,
        }),
        ImportKind::Group(entries) => {
            for entry in entries {
                let mut nested = prefix.clone();
                nested.tail.extend(entry.segments.iter().cloned());
                collect_import_leaves(
                    &nested,
                    reveal || entry.reveal,
                    &entry.kind,
                    entry.span,
                    leaves,
                );
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentKind {
    Expr,
    Type,
    Ident,
    Path,
}

#[derive(Debug, Clone)]
pub struct MacroParam {
    pub name: Ident,
    pub kind: FragmentKind,
}

#[derive(Debug, Clone)]
pub struct MacroSignature {
    pub fixed: Vec<MacroParam>,
    pub variadic: Option<MacroParam>,
}

#[derive(Debug, Clone)]
pub enum MacroBodyPiece {
    Token(Token),
    Repetition(MacroRepetition),
}

#[derive(Debug, Clone)]
pub struct MacroRepetition {
    pub separator: Option<Token>,
    pub body: Vec<MacroBodyPiece>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MacroDefinitionStmt {
    pub visibility: Visibility,
    /// The whole `macro name(...) => { ... }` declaration, which is the
    /// actionable source for anything a macro's own template authored.
    pub span: Span,
    pub name: Ident,
    pub signature: MacroSignature,
    pub body: Vec<MacroBodyPiece>,
    pub defining_module: Vec<Ident>,
    /// Set only for the canonical `core::builtins` declarations, whose empty
    /// bodies the compiler substitutes instead of expanding as templates.
    /// Bound from the defining module and name, so a same-named macro
    /// declared anywhere else stays an ordinary template.
    pub builtin: Option<MacroBuiltin>,
}

/// A macro whose expansion the compiler supplies. Each one substitutes an
/// ordinary literal token at the invocation site, so nothing downstream of
/// macro expansion needs to know these exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroBuiltin {
    File,
    Line,
    Column,
}

impl MacroBuiltin {
    /// The module whose declarations of `NAMES` are the compiler-backed
    /// ones. Every other module's same-named macro is an ordinary template.
    pub const MODULE: [&'static str; 2] = ["core", "builtins"];

    pub fn canonical(defining_module: &[Ident], name: &Ident) -> Option<Self> {
        if defining_module.len() != Self::MODULE.len()
            || !defining_module
                .iter()
                .zip(Self::MODULE)
                .all(|(segment, expected)| segment.as_ref() == expected)
        {
            return None;
        }
        match name.as_ref() {
            "file" => Some(Self::File),
            "line" => Some(Self::Line),
            "column" => Some(Self::Column),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Line => "line",
            Self::Column => "column",
        }
    }
}
