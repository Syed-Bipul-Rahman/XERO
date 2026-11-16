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

## Done in v0.5

- [x] Generic traits, default type arguments, associated types, static trait functions
- [x] `Index`/`IndexMut`, the `Iterator` trait and adapters, `for` over iterators, ranges as values
- [x] Struct patterns and bindings inside `|` patterns
- [x] `impl Trait` return types and trait objects (`dyn Trait`)
- [x] `async fn` and `.await` as state machines, with `std.task`
- [x] LLVM and lld built into `xeroc`

## Done in v0.6

- [x] Format strings checked at compile time: `format`, and `println` with arguments; the `Display` trait
- [x] String methods and number parsing; `Option`/`Result` helpers; `Deref`
- [x] `std.collections`: `HashMap` and `HashSet`, with `Hash` and `Borrow`
- [x] `std.time`, `std.process`, directories in `std.fs`, environment variables in `std.env`
- [x] `async` blocks, `+=` on move-only types, bounds on associated types, items from `iter_mut()` that outlive the next call

## Done in v0.7

- [x] Moving fields out of a struct one at a time
- [x] `Copy` and `Clone`; traits implemented for references (`Vec<&String>.sort()`)
- [x] Associated constants, integer limits and checked/wrapping/saturating arithmetic
- [x] Overflow checks in debug builds; panic messages for failed run-time checks
- [x] `Option<&T>` and `Option<Box<T>>` the size of a pointer
- [x] Projects: `xeroc new`, `xero.toml` with path and git dependencies, `xeroc test`
- [x] `TreeMap` and `VecDeque`

## Next

### Concurrency
- Async closures; async I/O in `std.task`, and timers without a thread each.
- Thread-safety marker traits (the role of Rust's `Send` and `Sync`) for
  user types with interior mutability built on `unsafe`.
- Thread pools and parallel iterators in the standard library.

### Standard library
- Piping input to a process; setting environment variables; file
  modification times; iterating a directory lazily; range queries on
  `TreeMap`.

### Self-contained programs
- No C runtime on Windows: call `kernel32` directly.

### Type system
- Moving out of fields in `async fn`s one at a time (moving a field there
  still consumes the whole value).
- Constants usable in array lengths when they come from an impl.

### Toolchain
- LLVM built in on Intel Macs too.

### Performance and the hardware
- SIMD vector types (`f32x4`, `i32x8`, ...) mapped to LLVM vectors.
- Attributes for packed or aligned layout, `#[inline]` and `#[cold]`.
- Allocator interface and arena allocators.

### Tooling
- A package registry, lock files, and workspaces with several packages.
- Formatter and language server.
- Debug info (DWARF) and source maps.
- Incremental compilation; a self-hosted compiler written in Xero.
