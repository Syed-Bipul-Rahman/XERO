## 0.2.0

* Adds closures: `|a, b| a + b`, `|x: i32| -> i32 { ... }` and `move |x| ...`, captured by reference (`&mut` when mutated) or by value.
* Adds `Fn(A) -> R` and `FnMut(A) -> R` bounds; generic functions taking closures are specialized per closure and inlined.
* Adds function pointer types `fn(A) -> R`: named functions and closures without captures can be stored, passed and returned.
* Adds calling closures stored in struct fields: `self.on_click(x)`.
* Adds operator traits `Eq`, `Ord`, `Add`, `Sub`, `Mul`, `Div`, `Rem` and `Neg`, so `==`, `<`, `+` and the rest work on your own types, plus `Ordering` and `compare`.
* Adds slicing with ranges: `&arr[1..3]`, `&text[7..]`, `&v[..n]`, `&arr[a..=b]`, including `&mut` slices.
* Adds references inside structs and enums (`struct Parser { input: &str }`) without lifetime annotations.
* Adds `Vec` methods `map`, `filter`, `for_each`, `any`, `all`, `count`, `position`, `sort_by` and `sort`.
* Infers the type of untyped literals from later use: `let mut total = 0` becomes `u64` after `total += x` with `x: u64`.
* Fixes references read out of containers or returned from functions not keeping their data borrowed, which could allow a dangling reference.
* Fixes `v.push(&local)` into a `Vec<&T>` from an inner scope not being rejected.
* Fixes `&mut` call arguments skipping the mutability check.

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
