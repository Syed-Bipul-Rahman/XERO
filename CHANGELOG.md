## 0.1.0

* Initial release of Xero and its compiler, `xeroc`, which generates native code through LLVM.
* Adds structs, enums with data, tuples, arrays, slices, `&str` and type aliases.
* Adds traits with default methods and supertraits (`extends`), generics with bounds and `where` clauses, and extension methods on any type.
* Adds `if`, `match` with exhaustiveness checking, `if x is Pattern`, `for`, `while`, `loop`, `break` and `continue`.
* Adds compile-time ownership, moves and borrow checking that rejects use after move, aliased mutation and dangling references.
* Adds deterministic destruction with `drop` blocks, and `copy` types.
* Adds error handling with `Result`, `Option`, `try`, `throw` and `catch`.
* Adds modules with `import`, `pub` visibility, `const`, `static` and `static mut`.
* Adds `unsafe` blocks, raw pointers, C interop (including variadic functions), `export fn`, inline `asm` and `volatile_load`/`volatile_store`.
* Adds the standard library: `print`/`println`, `Vec`, `String`, `Box`, `std.math` and `std.io`.
* Adds compilation for Linux, macOS, Windows, WebAssembly and bare-metal ARM and RISC-V.
* Adds one-command installers for Linux, macOS (`install.sh`) and Windows (`install.ps1`), which verify checksums and install clang when it is missing.
* Requires clang from LLVM 15 or newer, and reports a clear error when only an older clang is installed.
