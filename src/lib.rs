//! xeroc — the Xero compiler.
//!
//! Pipeline: source -> lexer -> parser (AST) -> checker (types, generics,
//! traits; lowers to XIR) -> borrow checker -> LLVM IR -> native code via
//! LLVM (linked into xeroc with the `llvm` feature, otherwise through clang).

pub mod ast;
pub mod borrowck;
pub mod check;
pub mod codegen;
pub mod diag;
pub mod driver;
pub mod lexer;
#[cfg(feature = "llvm")]
pub mod llvm;
pub mod parser;
pub mod target;
pub mod types;
pub mod xir;
