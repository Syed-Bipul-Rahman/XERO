# Xero

**Xero** is a minimal, native, zero-runtime-cost systems programming language.

```
                  XERO
                   │
       ┌───────────┼───────────┐
       ▼           ▼           ▼
    ZERO GC    ZERO-COST    ZERO DATA
    REQUIRED   ABSTRACTION     RACES
                   │
                   ▼
           NATIVE MACHINE CODE
                   │
                   ▼
          MAXIMUM HARDWARE USE
```

Xero compiles to native machine code through LLVM. Memory safety comes from
compile-time ownership and borrow checking, and values are destroyed
deterministically (RAII). There is no garbage collector, no runtime, no hidden
allocation and no dynamic dispatch unless you write it. You pay only for
what you use.

```xero
export fn add(a: i32, b: i32) -> i32 {
    a + b
}
```

```console
$ xeroc build add.xr --lib --emit asm
add:
        lea     eax, [rdi + rsi]
        ret
```

It needs no GC, no runtime objects, no vtables, no heap and no metadata. That
is the code a careful systems programmer would write by hand.

## A taste

```xero
struct User {
    name: String
    age: u32
}

trait Greet {
    fn greet(&self)
}

impl Greet for User {
    fn greet(&self) {
        print("Hello, ")
        println(&self.name)
    }
}

enum Shape {
    Circle(f64)
    Rect(f64, f64)
}

fn area(s: &Shape) -> f64 {
    match s {
        Shape.Circle(r) => 3.14159 * *r * *r
        Shape.Rect(w, h) => *w * *h
    }
}

fn parse_digit(b: u8) -> Result<u8, &str> {
    if b < '0' || b > '9' {
        throw "not a digit"
    }
    Ok(b - '0')
}

fn main() {
    let user = User { name: String.from("Bipul"), age: 25 }
    user.greet()

    let other = move user          // ownership moves; `user` is no longer usable
    let shapes = [Shape.Circle(1.0), Shape.Rect(2.0, 3.0)]
    for s in &shapes {
        println(area(s))
    }

    let d = parse_digit('7') catch e {
        println(e)
        0
    }
    println(d)
}                                   // `other` is destroyed here, deterministically
```

## Design

| Pillar | How Xero delivers it |
|---|---|
| **Performance** | Native code via LLVM · no GC · monomorphized generics · static dispatch · structs laid out like C · predictable destruction · no hidden allocations |
| **Safety** | Ownership and moves · borrowing (`&T` / `&mut T`) with aliasing-XOR-mutation · no dangling references · no null (`Option<T>`) · exhaustive `match` · bounds-checked indexing · `unsafe` as an explicit escape hatch · `static mut` access requires `unsafe` |
| **Simplicity** | Small keyword set · no semicolons needed · type inference for locals · `struct` + `trait` + `impl` instead of classes and inheritance · errors as values (`Result`, `try`, `throw`, `catch`) |

Xero takes ideas from four languages:

* **Rust**: ownership, borrowing, traits, pattern matching, `Result`/`Option`, zero-cost abstractions, `unsafe`
* **Kotlin**: concise syntax, type inference, null safety, extension methods (`impl i32 { ... }`)
* **Go**: simplicity, fast compilation, `.`-qualified module access
* **Java**: modules and visibility, mature generics with bounds

On top of those, Xero adds compile-time race prevention and deterministic
destruction, and targets bare metal (x86-64, ARM64, RISC-V and WebAssembly).

## Getting started

Install with one command:

```sh
# Linux / macOS
curl -fsSL https://download.xerolang.org/install.sh | sudo bash
```

```powershell
# Windows (PowerShell)
irm https://download.xerolang.org/install.ps1 | iex
```

The installer also sets up `clang` (LLVM 17+), which is Xero's code-generation
back end. See [INSTALL.md](INSTALL.md) for options and manual installation.
To build from source:

```console
$ cargo build --release
$ ./target/release/xeroc run examples/hello.xr
Hello, Xero!
```

```
xeroc build <file.xr> [options]     compile to a native executable
xeroc run <file.xr> [-- args...]    compile and run
xeroc check <file.xr>               type- and borrow-check only
xeroc targets                       list supported targets

  -o <path>          output path
  --emit <kind>      exe | obj | asm | llvm | llvm-raw | xir
  -O0..-O3           optimization level (default -O2)
  --target <name>    cross-compile, e.g. aarch64-linux, riscv64-linux, wasm32, thumbv7em-none
  --lib              no `main` required (library object / wasm module)
```

## Targets

| Target | Kind |
|---|---|
| `x86_64-linux`, `aarch64-linux`, `riscv64-linux` | hosted executables |
| `x86_64-macos`, `aarch64-macos`, `x86_64-windows` | objects (cross-compiled) |
| `wasm32` | standalone WebAssembly modules (`export fn` becomes a wasm export) |
| `x86_64-none`, `aarch64-none`, `riscv64-none`, `riscv32-none`, `thumbv7em-none` | bare-metal objects |

## Examples

| File | Shows |
|---|---|
| [`examples/hello.xr`](examples/hello.xr) | the smallest program |
| [`examples/tour.xr`](examples/tour.xr) | a tour of the language |
| [`examples/ownership.xr`](examples/ownership.xr) | moves, borrows, RAII |
| [`examples/sieve.xr`](examples/sieve.xr) | CPU-bound code (within about 10% of C at `-O3`) |
| [`examples/wasm_math.xr`](examples/wasm_math.xr) | a 447-byte WebAssembly module |
| [`examples/baremetal.xr`](examples/baremetal.xr) | no OS: MMIO with `volatile_load`/`volatile_store`, inline asm |

The test programs in [`tests/run`](tests/run) cover every implemented feature,
and [`tests/fail`](tests/fail) contains programs that the compiler must reject:
use after move, aliasing violations, dangling references, data races
on `static mut`, non-exhaustive matches and more.

## Documentation

* [Language reference](docs/LANGUAGE.md)
* [Compiler architecture](docs/COMPILER.md)
* [Roadmap](docs/ROADMAP.md)

## Status

Xero 0.1 is a working bootstrap compiler. It already supports structs, enums
with payloads, traits with default methods and supertraits, generics with
bounds, `impl` blocks on any type, pattern matching with exhaustiveness
checking, modules, `Box`/`Vec`/`String`, RAII, a borrow checker, errors as
values, unsafe code, raw pointers, C FFI, inline assembly and cross-compilation.
The concurrency keywords (`spawn`, `async`, `await`) are reserved and are the
next major milestone; see the [roadmap](docs/ROADMAP.md).

```console
$ cargo test       # builds and runs every test program
```
