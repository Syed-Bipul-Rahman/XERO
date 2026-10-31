//! Abstract syntax tree produced by the parser.

use crate::diag::Span;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vis {
    Private,
    Pub,
}

#[derive(Debug)]
pub struct Module {
    pub file: u32,
    pub name: Option<Ident>,
    pub items: Vec<Item>,
}

#[derive(Debug)]
pub struct Item {
    pub vis: Vis,
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum ItemKind {
    Import(Import),
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Trait(TraitDecl),
    Impl(ImplDecl),
    TypeAlias {
        name: Ident,
        generics: Generics,
        ty: TypeExpr,
    },
    Const {
        name: Ident,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    Static {
        name: Ident,
        mutable: bool,
        ty: TypeExpr,
        value: Expr,
    },
}

#[derive(Debug)]
pub struct Import {
    /// `import std.io` -> ["std", "io"]
    pub path: Vec<Ident>,
    /// `import std.io.{print, println}`
    pub names: Option<Vec<Ident>>,
    /// `import std.io as out`
    pub alias: Option<Ident>,
}

#[derive(Clone, Debug, Default)]
pub struct Generics {
    pub params: Vec<GenericParam>,
}

#[derive(Clone, Debug)]
pub struct GenericParam {
    pub name: Ident,
    pub bounds: Vec<TypeExpr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelfKind {
    Value,
    Ref,
    RefMut,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Ident,
    pub mutable: bool,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    pub name: Ident,
    pub generics: Generics,
    pub self_param: Option<(SelfKind, bool, Span)>,
    pub params: Vec<Param>,
    pub variadic: bool,
    pub ret: Option<TypeExpr>,
    pub body: Option<Block>,
    pub is_unsafe: bool,
    pub is_export: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub vis: Vis,
    pub name: Ident,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug)]
pub struct StructDecl {
    pub name: Ident,
    pub generics: Generics,
    pub fields: Vec<FieldDecl>,
    pub is_copy: bool,
}

#[derive(Clone, Debug)]
pub struct VariantDecl {
    pub name: Ident,
    pub fields: Vec<TypeExpr>,
    pub discriminant: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub name: Ident,
    pub generics: Generics,
    pub variants: Vec<VariantDecl>,
    pub is_copy: bool,
}

#[derive(Clone, Debug)]
pub struct TraitDecl {
    pub name: Ident,
    pub generics: Generics,
    pub supertraits: Vec<TypeExpr>,
    pub methods: Vec<FnDecl>,
}

#[derive(Clone, Debug)]
pub struct ImplDecl {
    pub generics: Generics,
    pub trait_ref: Option<TypeExpr>,
    pub self_ty: TypeExpr,
    pub methods: Vec<(Vis, FnDecl)>,
    pub drop: Option<Block>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TypeExpr {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TypeKind {
    /// `a.b.Name<Args>`
    Path(Vec<Ident>, Vec<TypeExpr>),
    SelfType,
    Ref(bool, Box<TypeExpr>),
    Ptr(Box<TypeExpr>),
    Array(Box<TypeExpr>, Box<Expr>),
    Slice(Box<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    Never,
    Infer,
    /// `fn(i32, i32) -> i32`: a plain function pointer.
    FnPtr(Vec<TypeExpr>, Option<Box<TypeExpr>>),
    /// `Fn(i32) -> i32` / `FnMut(i32)` / `FnOnce()`: a closure bound (only in
    /// generic bounds). The kind is 0 = Fn, 1 = FnMut, 2 = FnOnce.
    FnTrait(u8, Vec<TypeExpr>, Option<Box<TypeExpr>>),
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let {
        pat: Pattern,
        ty: Option<TypeExpr>,
        init: Expr,
        span: Span,
    },
    /// The bool records whether a `;` followed the expression.
    Expr(Expr, bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
        }
    }

    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(u128, Option<String>),
    Float(f64, Option<String>),
    Str(Vec<u8>),
    Char(u8),
    Bool(bool),
    Unit,
    /// A name with optional explicit generic arguments: `x`, `Vec<i32>`.
    Name(Ident, Vec<TypeExpr>),
    SelfValue,
    SelfType(Vec<TypeExpr>),
    Field(Box<Expr>, Ident, Vec<TypeExpr>),
    TupleField(Box<Expr>, u32, Span),
    Call(Box<Expr>, Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Assign(Box<Expr>, Box<Expr>),
    CompoundAssign(BinOp, Box<Expr>, Box<Expr>),
    Ref(bool, Box<Expr>),
    Deref(Box<Expr>),
    Move(Box<Expr>),
    Copy(Box<Expr>),
    Drop(Box<Expr>),
    Cast(Box<Expr>, TypeExpr),
    Is(Box<Expr>, Pattern),
    StructLit(Box<Expr>, Vec<(Ident, Expr)>),
    Tuple(Vec<Expr>),
    Array(Vec<Expr>),
    ArrayRepeat(Box<Expr>, Box<Expr>),
    Range(Option<Box<Expr>>, Option<Box<Expr>>, bool),
    Block(Block),
    Unsafe(Block),
    If(Box<Expr>, Block, Option<Box<Expr>>),
    While(Box<Expr>, Block),
    Loop(Block),
    For(Pattern, Box<Expr>, Block),
    Break(Option<Box<Expr>>),
    Continue,
    Return(Option<Box<Expr>>),
    Match(Box<Expr>, Vec<Arm>),
    Try(Box<Expr>),
    Throw(Box<Expr>),
    Catch(Box<Expr>, Option<Ident>, Block),
    /// `|a, b: i32| a + b`, `move |x| -> i32 { ... }`
    Closure(Box<ClosureExpr>),
}

#[derive(Clone, Debug)]
pub struct ClosureParam {
    pub name: Ident,
    pub mutable: bool,
    pub ty: Option<TypeExpr>,
}

#[derive(Clone, Debug)]
pub struct ClosureExpr {
    pub params: Vec<ClosureParam>,
    pub ret: Option<TypeExpr>,
    pub body: Expr,
    pub is_move: bool,
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub pat: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Pattern {
    pub kind: PatKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum PatKind {
    Wild,
    /// A bare identifier: a binding, or a unit enum variant if one is in scope.
    Bind(Ident, bool),
    Lit(Box<Expr>),
    Range(Box<Expr>, Box<Expr>, bool),
    Tuple(Vec<Pattern>),
    /// `Some(x)`, `Shape.Circle(r)`, `Color.Red`
    Variant(Vec<Ident>, Option<Vec<Pattern>>),
    Or(Vec<Pattern>),
}
