//! XIR: Xero's typed, monomorphized intermediate representation.
//!
//! Produced by the checker, validated by the ownership checker, and lowered
//! to LLVM IR by the code generator. Every generic item has been
//! instantiated, every method call resolved statically, and every implicit
//! operation (auto-ref, coercion, copy vs. move) made explicit.

use crate::ast::{BinOp, UnOp};
use crate::diag::Span;
use crate::types::Ty;

pub type LocalId = u32;
pub type LoopId = u32;

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
    pub span: Span,
    /// Compiler-generated temporaries are not user-visible.
    pub synthetic: bool,
}

#[derive(Debug)]
pub struct Func {
    pub name: String,
    pub params: Vec<LocalId>,
    pub ret: Ty,
    pub locals: Vec<Local>,
    pub body: Expr,
    /// Exported with an unmangled C symbol name.
    pub export: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct Extern {
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub variadic: bool,
}

#[derive(Clone, Debug)]
pub enum ConstVal {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(Vec<u8>),
    Array(Vec<ConstVal>),
    Repeat(Box<ConstVal>, u64),
    Tuple(Vec<ConstVal>),
}

#[derive(Debug)]
pub struct Static {
    pub name: String,
    pub ty: Ty,
    pub init: ConstVal,
    pub mutable: bool,
}

#[derive(Debug)]
pub struct Program {
    pub funcs: Vec<Func>,
    pub externs: Vec<Extern>,
    pub statics: Vec<Static>,
    /// Name of the user's `main`, if any.
    pub entry: Option<(String, Ty)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UseKind {
    Copy,
    Move,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindMode {
    /// Copy or move the matched value into the binding.
    Value,
    /// Bind a reference to the matched value (default binding mode when
    /// matching through `&T`/`&mut T`).
    Ref(bool),
}

#[derive(Clone, Debug)]
pub enum Pat {
    Wild,
    Bind(LocalId, BindMode),
    Int(i128),
    Bool(bool),
    Range(i128, i128),
    Tuple(Vec<Pat>),
    /// Enum variant index + sub-patterns (also used for struct-like tuples).
    Variant(u32, Vec<Pat>),
    Or(Vec<Pat>),
    /// Match through a reference: the scrutinee is `&T`, the inner pattern matches `T`.
    Deref(Box<Pat>),
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub pat: Pat,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Place {
    Local(LocalId),
    Static(String, bool),
    /// Field of a struct or tuple.
    Field(Box<Place>, u32),
    /// Element of an array (base is an array place) or a slice
    /// (base is `Deref` of a fat reference). Always bounds-checked.
    Index(Box<Place>, Box<Expr>),
    /// Dereference of a reference or raw pointer value.
    Deref(Box<Expr>),
    /// `base[lo..hi]`: an unsized sub-range of an array, slice or `str`
    /// (bounds-checked once, when it is formed). Missing bounds mean the
    /// start / the end. Only ever borrowed or indexed.
    Slice(Box<Place>, Option<Box<Expr>>, Option<Box<Expr>>),
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    /// Locals declared in this block; destroyed in reverse order at exit.
    pub scope: Vec<LocalId>,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let(LocalId, Expr),
    /// Destructure a tuple value into several locals.
    LetPat(Pat, Expr),
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub enum Intrinsic {
    SizeOf(Ty),
    AlignOf(Ty),
    Trap,
    /// Inline assembly with no operands.
    Asm(String),
    /// (ptr, len) -> fat reference of `self.ty`
    MakeFat,
    /// Length of a fat reference or the constant length of an array reference.
    Len,
    /// Data pointer of a fat reference.
    DataPtr,
    /// Move a value out of raw memory: `ptr_read(p)`.
    PtrRead,
    /// Move a value into raw memory without dropping the old contents.
    PtrWrite,
    /// Run the destructor of the value behind a raw pointer.
    DropInPlace(Ty),
    /// Consume a value without running its destructor.
    Forget,
    /// Memory-mapped I/O: loads/stores the optimizer may not remove or merge.
    VolatileLoad,
    VolatileStore,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(i128),
    Float(f64),
    Bool(bool),
    /// String literal: `&str`.
    Str(Vec<u8>),
    Unit,
    Use(Place, UseKind),
    Borrow(Place, bool),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Cast(Box<Expr>, Ty),
    /// `&[T; N]` -> `&[T]`
    ArrayToSlice(Box<Expr>),
    Call(String, Vec<Expr>),
    /// Call through a function pointer value.
    CallPtr(Box<Expr>, Vec<Expr>),
    /// The address of a function, as a `fn(...)` value.
    FnRef(String),
    Intrinsic(Intrinsic, Vec<Expr>),
    Struct(Vec<Expr>),
    Tuple(Vec<Expr>),
    Array(Vec<Expr>),
    ArrayRepeat(Box<Expr>, u64),
    Variant(u32, Vec<Expr>),
    Assign(Place, Box<Expr>),
    CompoundAssign(BinOp, Place, Box<Expr>),
    Block(Block),
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>),
    Loop(Box<Expr>, LoopId),
    Break(LoopId, Option<Box<Expr>>),
    Continue(LoopId),
    Return(Option<Box<Expr>>),
    /// Scrutinee place; `moves` is true if any arm binds by move.
    Match(Place, Vec<Arm>),
    /// `drop x`: destroy the value now.
    Drop(Place),
    /// Statement-like `if scrutinee is pat` test with no bindings.
    Is(Place, Pat),
}
