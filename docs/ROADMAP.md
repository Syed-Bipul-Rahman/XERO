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

## Next

### Concurrency without data races (`spawn`, `async`, `await`)
- `spawn { ... }` starts a scoped thread. Captured values are moved in, or
  borrowed when they outlive the scope.
- Thread-safety marker traits checked at compile time, playing the role of
  Rust's `Send` and `Sync`.
- `Channel<T>`, `Mutex<T>` and `Atomic*` as library types.
- `async fn` and `await` compiled to state machines, with no mandatory runtime.

### Type system
- Lifetime parameters, so structs can hold references.
- Closures and function values (with static dispatch where possible).
- Trait objects (`dyn Trait`) as an opt-in for dynamic dispatch.
- Operator traits (`Add`, `Eq`, `Ord`, `Index`) and `==` on structs.
- Full inference variables for integer literals (`for i in 0..10 { v.push(i) }` with `v: Vec<i64>`).
- Generic traits, associated types and constants.
- Slicing syntax: `&array[a..b]`.
- Struct patterns with named fields, and bindings inside `|` patterns.

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
