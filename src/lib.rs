//! xeroc — the Xero compiler.
//!
//! Pipeline: source -> lexer -> parser (AST) -> checker (types, generics,
//! traits; lowers to XIR) -> borrow checker -> LLVM IR -> native code via clang.

pub mod ast;
pub mod borrowck;
pub mod check;
pub mod codegen;
pub mod diag;
pub mod driver;
pub mod lexer;
pub mod parser;
pub mod target;
pub mod types;
pub mod xir;
