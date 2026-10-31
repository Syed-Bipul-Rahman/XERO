# Xero Roadmap

## Done in v0.1

- [x] Lexer, parser, and newline-terminated statements
- [x] Structs, enums with payloads, tuples, arrays, slices, `&str`
- [x] Traits: default methods, supertraits (`extends`), static dispatch
- [x] Generics with bounds and `where` clauses, monomorphization, inference
- [x] Extension methods (`impl i32 { ... }`)
- [x] Pattern matching with exhaustiveness checking; `if x is P` and `while x is P`
- [x] Ownership, moves, `copy` types, RAII with `drop` blocks and drop flags
- [x] Borrow checking: aliasing XOR mutation, two-phase borrows, no dangling references
- [x] Errors as values: `Result`, `Option`, `try`, `throw`, `catch`
- [x] Modules, `import`, `pub` and private visibility, `const`, `static`, `static mut`
- [x] `unsafe`, raw pointers, C FFI (including variadic functions), `export fn`, inline `asm`
- [x] `volatile_load`/`volatile_store` for memory-mapped I/O
- [x] Standard library in Xero: `Box`, `Vec`, `String`, `math`, `io`
- [x] LLVM back end: x86-64, ARM64, RISC-V, wasm32, bare-metal ARM and RISC-V

## Done in v0.2

- [x] Slicing: `&arr[a..b]`, `&s[a..]`, `&v[..b]`, `&arr[a..=b]`
- [x] Literal types inferred from later use (`for i in 0..10 { v.push(i) }` with `v: Vec<i64>`)
- [x] Operator traits `Eq`, `Ord`, `Add`, `Sub`, `Mul`, `Div`, `Rem`, `Neg`, plus `Ordering` and `compare`
- [x] Closures (`Fn` / `FnMut`, `move`), function pointers `fn(A) -> R`, named functions as values
- [x] References inside structs and enums, without lifetime annotations
- [x] `Vec` helpers taking closures: `map`, `filter`, `for_each`, `any`, `all`, `count`, `position`, `sort_by`, `sort`

## Done in v0.3

- [x] `spawn { ... }` and `spawn move { ... }`: scoped OS threads joined when their handle is destroyed
- [x] Data races are compile errors (the borrow checker applies across threads)
- [x] `std.sync`: `Mutex<T>`, `Channel<T>`, `Atomic<T>`, `AtomicBool`
- [x] `FnOnce` closures and atomic intrinsics

## Done in v0.4

- [x] No libc on Linux: direct system calls, an `mmap`-based allocator with per-thread arenas, `clone` threads, futexes and our own entry point
- [x] Static executables, and cross-compiling Linux programs for ARM64 and RISC-V with only `lld`
- [x] `std.fs`, `std.env`, `std.os`
- [x] CI on ARM64 Linux, and every test program run for ARM64 and RISC-V under qemu

## Next

### Concurrency
- `async fn` and `await` compiled to state machines, with no mandatory runtime.
- Thread-safety marker traits (the role of Rust's `Send` and `Sync`) for
  user types with interior mutability built on `unsafe`.
- Thread pools and parallel iterators in the standard library.

### Self-contained programs
- No C runtime on Windows: call `kernel32` directly.
- Directories, environment variables, time and processes in the standard library.

### Type system
- Trait objects (`dyn Trait`) as an opt-in for dynamic dispatch.
- `impl Fn(A) -> R` return types, so functions can return capturing closures.
- An `Index` operator trait, and `+=` on move-only user types.
- Generic traits, associated types and constants.
- Struct patterns with named fields, and bindings inside `|` patterns.

### Toolchain
- LLVM (and lld) built into `xeroc`, so no separate clang install is needed.

### Performance and the hardware
- SIMD vector types (`f32x4`, `i32x8`, ...) mapped to LLVM vectors.
- Niche layout optimization (`Option<Box<T>>` and `Option<&T>` the size of a pointer).
- Overflow-checked arithmetic in debug builds.
- Attributes for packed or aligned layout, `#[inline]` and `#[cold]`.
- Allocator interface and arena allocators.

### Tooling
- Package manager and build tool.
- Formatter and language server.
- Debug info (DWARF) and source maps.
- Incremental compilation; a self-hosted compiler written in Xero.
