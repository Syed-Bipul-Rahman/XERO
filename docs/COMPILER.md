# xeroc: Compiler Architecture

```
  .xr source
      │
      ▼
  lexer.rs        tokens (newlines are significant)
      │
      ▼
  parser.rs       AST (ast.rs)
      │
      ▼
  check/          name resolution · type checking · generics · traits
      │             monomorphization · method resolution · patterns
      │             exhaustiveness · constant evaluation
      ▼
  XIR (xir.rs)    Xero IR: typed, monomorphic, explicit moves/copies/borrows
      │
      ▼
  borrowck.rs     ownership & borrow checking on XIR
      │
      ▼
  codegen.rs      LLVM IR (text) with drop elaboration
      │
      ▼
  clang / LLVM    optimization + x86-64 / ARM64 / RISC-V / wasm machine code
```

## Front end

* **Lexer** (`lexer.rs`). Emits `Newline` tokens. The parser keeps a stack of
  "newlines are significant" flags: newlines matter inside `{}` blocks and are
  ignored inside `()` and `[]`.
* **Parser** (`parser.rs`). Hand-written recursive descent. Generic arguments
  in expression position (`Vec<i32>.new()`) are parsed speculatively and
  rewound when the parse fails. Struct literals are disabled in `if`/`while`/
  `match`/`for` heads.

## Checker (`check/`)

* `mod.rs` collects items from every module, resolves imports, resolves type
  templates for struct fields, enum payloads and impl self types, and validates
  trait impls (missing or extra methods, overlap, supertraits). It also manages
  the **instance queue**.
* **Monomorphization.** A function instance is a `FnKey { fid, targs, self_ty }`.
  Every non-generic function in a user module is a root. Instances are created
  on demand when a call is resolved. Each instance is checked and lowered with
  concrete types, like a template, so XIR never contains type parameters, and
  trait calls resolve to a specific impl method.
* **Inference** (`expr.rs::infer_args`). Signatures are templates over
  `Param(i)` slots for the impl's and the function's generic parameters. Slots
  are filled from the expected return type, then from the receiver, then from
  each argument in turn. Untyped integer literals take their type from context
  and default to `i32`.
* **Method resolution.** The receiver's type is dereferenced step by step
  (`T`, `*T`, `**T`, ...). Inherent impls are tried before trait impls, and
  trait default methods are instantiated with `Self` set to the concrete type.
  The receiver is then borrowed, reborrowed or moved according to the method's
  `self`, `&self` or `&mut self`.
* **Temporaries.** An rvalue that needs an address becomes a synthetic local
  owned by the enclosing block, so its drop is deterministic and borrows of it
  are checked like borrows of any other local.
* **Desugaring.** `while` and `for` become `loop` plus `if`/`break`. `try`,
  `catch` and `if x is P` become `match`. `throw e` becomes `return Err(e)`.
  `a == b` on `&str` becomes a call to `str_eq`.
* `pat.rs` lowers patterns with default binding modes. `exhaust.rs` checks
  exhaustiveness with a usefulness algorithm in the style of Maranget.

## Borrow checker (`borrowck.rs`)

The borrow checker runs per function on XIR, in two passes:

1. **Liveness.** Each expression gets a position in evaluation order, and the
   pass records the last use of every local. A local that is used inside a loop
   and declared outside it stays live until the loop ends.
2. **Checking.** Abstract interpretation over `State { moved, holders }`:
   * a **loan** is `(root local, projection path, mutable?)`, and the path
     records field, index and deref projections;
   * a variable that holds a reference holds that reference's loans;
     references returned from calls keep the loans of the arguments that were
     passed by reference (lifetime elision);
   * a loan is live while the variable holding it is still used later. Loans
     held by arguments of a call that hasn't returned yet are live too;
   * each access (read, write, move, `&`, `&mut`) is checked against the live
     loans that overlap its path, and against the set of moved values. The
     state is merged at control-flow joins, and loops are analyzed twice to
     reach a fixpoint, which catches moves made in an earlier iteration;
   * `&mut` call arguments are **two-phase**. They are reserved while the
     arguments are evaluated and activated when the call starts;
   * at the end of a block, values that escape it, and variables in outer
     scopes that are still live, must not hold loans on locals that die there.
     A function's return value must not hold any loan on a local.

## Code generation (`codegen.rs`)

* **Types.** Structs and tuples become LLVM literal structs with C layout.
  Enums become `{ tag, [N x iK] }`, with the tag sized to the discriminant
  range. `&[T]` and `&str` become `{ ptr, usize }`.
* **Locals** are `alloca`s in the entry block, and LLVM's SROA and mem2reg
  passes turn them into registers.
* **Drop elaboration.** A local whose type needs dropping gets a 1-bit flag.
  Assignment drops the old value. Moves clear the flag. Leaving a scope
  through fall-through, `break`, `continue` or `return` drops the live locals
  in reverse order. Drop glue is generated once per type: it runs the user's
  `drop` block, then drops the fields (and, for an enum, the payload of the
  active variant).
* **Safety checks.** Bounds checks, division by zero and `MIN / -1` branch to
  `llvm.trap`, and the branch is marked unlikely. Float-to-int casts use
  `llvm.fpto[su]i.sat`.
* All functions are `internal` and `nounwind`, except `export fn`s and the C
  `main` wrapper. That lets LLVM inline aggressively and delete unused code.

## Driver (`driver.rs`)

The driver loads the root file and follows imports. The standard library is
embedded in the `xeroc` binary. The driver then runs the front end, writes
LLVM IR, and invokes `clang -x ir --target=<triple> -O<n>` to produce assembly,
an object file, an executable or a wasm module. Set `XERO_CLANG` to choose a
specific clang binary.

The driver also chooses the OS layer, `std.sys`, for the target. It is the
only part of the standard library that knows about the operating system:

| File | Used for |
|---|---|
| `std/sys/linux.xr` | Linux without libc: system calls, heap, threads, futexes, `memcpy` & co. |
| `std/sys/unix.xr` | macOS, and Linux programs that use libc |
| `std/sys/windows.xr` | Windows: the C runtime and `kernel32` |
| `std/sys/none.xr` | bare metal and wasm |

Whether a Linux program uses libc is decided after parsing: it does if any
of the program's own modules declares a C function (a `fn` without a body),
or for `--lib` and `--emit obj`, unless `--libc`/`--no-libc` says otherwise.
So `std.sys` is loaded last. Two small modules are generated per target:
`std.os` (`NAME`, `ARCH`, `POINTER_BITS`) and `std.sys.target` (system call
numbers, `open` flags and the `errno` accessor).

Without libc, codegen adds the entry point `_start` (which passes `argc` and
`argv` to `main` and exits with its result) and the `__xero_clone` thread
trampoline as module-level assembly for x86-64, ARM64 and RISC-V, and the
driver links with `-nostdlib -static` and `--gc-sections`, using `lld` when
the target isn't the host. The `syscall` intrinsic becomes an inline `syscall`,
`svc #0` or `ecall` instruction. The runtime's `memcpy`, `memmove`, `memset`,
`memcmp` and `bcmp` get LLVM's `no-builtins` attribute, so their loops are
never turned back into calls to themselves.

## Testing

`cargo test` runs `tests/harness.rs`, which has these suites:

* `tests/run/*.xr` are compiled and run, and their stdout is compared with the
  `// output:` block at the end of each file;
* `tests/fail/*.xr` must be rejected with the messages listed in their
  `// error:` lines;
* `add_is_zero_cost` checks that `add` compiles to exactly `lea eax, [rdi + rsi]; ret`;
* `linux_programs_need_no_libc` checks that "hello, world" on Linux is a small
  static executable.

`XERO_TEST_FLAGS` adds flags to every build (for example `--libc`, or
`--target aarch64-linux`) and `XERO_TEST_RUNNER` runs the programs through
another program (for example `qemu-aarch64`). Programs marked
`// test: native-only` are skipped when cross-compiling.
