## 0.4.0

* Adds Xero's own runtime on Linux: programs make system calls directly and no longer use the C library, so executables are small and static ("hello, world" is about 9 KB) and run on any Linux system of the same architecture.
* Adds the runtime's memory allocator (built on `mmap`, with an arena per thread), threads (`clone` and futexes) and program entry point.
* Adds cross-compiling Linux executables for ARM64 and RISC-V (`--target aarch64-linux`, `--target riscv64-linux`) with only `lld` installed.
* Adds `--libc` and `--no-libc`; a program that declares C functions is linked with the C library automatically.
* Adds `std.fs`: `File` (`open`, `create`, `append`, `read`, `read_all`, `read_to_string`, `write`, `write_str`), closed when destroyed, plus `read_to_string`, `read`, `write`, `remove` and a printable `Error`.
* Adds `std.env` with `args`, `arg` and `arg_count`.
* Adds `std.os` with `NAME`, `ARCH` and `POINTER_BITS`.
* Adds `String.from_utf8`, `as_bytes`, `into_bytes` and `std.string.is_utf8`.
* Adds the `syscall` intrinsic for raw Linux system calls, and `os_argc`/`os_argv`.
* Changes `std.io` functions to return positive `errno` values as errors.
* Changes threads to get 8 MiB stacks on every OS (macOS and Windows defaulted to 512 KiB and 1 MiB).
* Fixes `%` on floating-point values computed at run time failing to link on Linux.
* Fixes `std.io.read_into` failing to compile.

## 0.3.0

* Adds threads: `spawn { ... }` runs a block on a new OS thread and returns a handle whose `join()` gives the block's value.
* Adds scoped threads: a thread is joined when its handle is destroyed, so it can borrow local variables, and data races are compile errors.
* Adds `spawn move { ... }`, which moves the values a thread uses into it.
* Adds `std.sync`: `Mutex<T>` (with `lock()` guards and `with`), `Channel<T>` (`send`, `recv`, `try_recv`, `close`), `Atomic<T>` and `AtomicBool`.
* Adds `std.thread` with `start`, `sleep_ms` and `yield_now`; threads use POSIX threads on Linux and macOS and `kernel32` on Windows.
* Adds `FnOnce` closures: a `move` closure that moves a captured value out can be called once, and `FnOnce(A) -> R` bounds accept any closure.
* Adds atomic intrinsics `atomic_load`, `atomic_store`, `atomic_swap`, `atomic_add`, `atomic_sub`, `atomic_cas` and `fence`.
* Infers the element type of literal arrays (`[0; 4]`) from later use.
* Allows patterns to move out of temporaries: `while ch.recv() is Some(job)`.
* Changes `forget` to require `unsafe`.
* Fixes `Box.new(&local)` and values sent into a channel not keeping their data borrowed.
* Fixes values with a `drop` block being destroyed after data they borrow.
* Fixes enums whose only fields are zero-sized, such as `Option<()>`, failing to compile.

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
