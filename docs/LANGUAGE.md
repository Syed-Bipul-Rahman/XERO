# The Xero Language Reference (v0.4)

This document describes the language as implemented by `xeroc` 0.3. Features
that are reserved but not implemented yet are marked **(reserved)**.

## 1. Lexical structure

* Source files are UTF-8 and use the `.xr` extension.
* Comments: `// line` and `/* block */`. Block comments nest.
* **Statements end at a newline** or a `;`. Inside `(...)` and `[...]`, newlines
  are ignored. A line may continue after a binary operator, and a method chain
  may continue on the next line if that line starts with `.`.
* Identifiers: `[A-Za-z_][A-Za-z0-9_]*`. `_` alone is the wildcard.
* Integer literals: `42`, `1_000_000`, `0xFF`, `0b1010`, `0o777`, with an
  optional type suffix such as `10u8`, `3i64` or `7usize`.
* Float literals: `1.5`, `2e10`, `1.0f32`.
* Character literals `'a'`, `'\n'` and `'\x41'` are bytes (`u8`).
* String literals `"..."` have type `&str`. Escapes: `\n \t \r \0 \\ \" \' \xNN`.
  Every literal is also stored with a trailing NUL that `.len()` does not count,
  so `"text" as *u8` is a valid C string.

### Keywords

| Group | Keywords |
|---|---|
| Modules | `module` `import` `export` `pub` `private` |
| Bindings | `let` `mut` `const` `static` |
| Functions | `fn` `return` |
| Control flow | `if` `else` `match` `for` `while` `loop` `break` `continue` |
| Types | `struct` `enum` `trait` `impl` `type` `where` `extends` |
| Ownership | `own` (reserved) `borrow` `move` `copy` `drop` `unsafe` |
| Concurrency | `spawn` · `async` `await` (reserved) |
| Errors | `try` `catch` `throw` |
| Literals | `true` `false` |
| Misc | `as` `in` `is` `self` `Self` |

Xero deliberately has no `class`, `new`, `null`, `extends` for classes,
`implements`, `go` or `chan`. `struct`, `trait` and `impl` cover composition.
Concurrency is `spawn { ... }` plus library types such as `Channel<T>` and `Mutex<T>` (section 11).

## 2. Items

```xero
module app.server              // optional; names this module

import std.math                // binds `math`; use as `math.sqrt(2.0)`
import std.vec.{Vec}           // binds selected items directly
import net.http as web         // binds `web`

pub fn visible_to_importers() {}
fn private_to_this_module() {}

export fn c_symbol(x: i32) -> i32 { x }       // C ABI, unmangled symbol name
unsafe fn puts(s: *u8) -> i32                 // foreign (C) function: no body

const MAX: usize = 1024                       // compile-time constant
static BANNER: &str = "xero"                  // immutable global
static mut COUNTER: u64 = 0                   // mutable global: access needs `unsafe`
type Pair<T> = (T, T)                         // type alias
```

### Modules

Each file is a module. `import a.b` loads `a/b.xr` relative to the directory of
the root file, and `import std.x` loads a module of the standard library. Items
are private by default. `pub` exports them, and `private` states the default
explicitly. Struct fields are private to their module unless they are marked `pub`.

The **prelude** is imported everywhere. It provides `Option`, `Some`, `None`,
`Result`, `Ok`, `Err`, `Print`, `print`, `println`, `eprint`, `eprintln`,
`panic`, `assert`, `Vec`, `String` and `Box`.

## 3. Types

| Type | Description |
|---|---|
| `i8 i16 i32 i64 isize` | signed integers |
| `u8 u16 u32 u64 usize` | unsigned integers |
| `f32 f64` | IEEE floats |
| `bool` | `true` / `false` |
| `()` | unit |
| `!` | never (the type of `return`, `break`, `panic(..)`) |
| `(A, B, ...)` | tuples; fields `t.0`, `t.1` |
| `[T; N]` | fixed-size arrays, stored inline |
| `&T`, `&mut T` | shared and exclusive references (never null, never dangling) |
| `&[T]`, `&mut [T]` | slices (pointer + length) |
| `&str` | string slice (UTF-8 bytes + length) |
| `*T` | raw pointer (dereferencing requires `unsafe`) |
| `fn(A, B) -> R` | function pointer: a named function or a closure without captures |
| `struct`, `enum` | user-defined types |

There is **no null**. An optional value is an `Option<T>`.

### Structs

```xero
struct User {
    name: String
    pub age: u32
}

let u = User { name: String.from("Bipul"), age: 25 }   // named fields
let u = User(String.from("Bipul"), 25)                 // positional
```

Struct layout follows the C layout rules, so a struct can be shared with C as-is.

Fields can be references. A struct holding a reference borrows what it points
to, and the borrow checker keeps that borrow alive for as long as the struct
is used, with no lifetime annotations:

```xero
struct Parser {
    input: &str
    pos: usize
}

impl Parser {
    fn word(&mut self) -> &str { ... }   // returns part of `input`
}

let text = String.from("the quick fox")
let mut p = Parser { input: text.as_str(), pos: 0 }
let w = p.word()      // borrows `text`, not `p`: `p` stays usable
// text.clear()       // error: `text` is borrowed while `p` or `w` is in use
```

### Enums

```xero
enum Shape {
    Circle(f64)
    Rect(f64, f64)
    Empty
}

copy enum Color { Red, Green = 5, Blue }    // explicit discriminants allowed

let s = Shape.Circle(1.0)
let c = Color.Blue as i32                     // 6
```

An enum is a tagged union, and the tag uses the smallest integer type that fits.

### Copy and move types

Primitives, shared references, raw pointers, and arrays and tuples of copy
types are **copied**. Everything else is **moved**. Declare a struct or enum with
`copy struct` or `copy enum` to make it copyable. A generic copy type such as
`Option<T>` is copyable only when its fields are. A `copy` type cannot have a
`drop` block.

## 4. Ownership and borrowing

Xero's safety model checks everything at compile time and adds nothing at run time.

1. **Every value has one owner.** Assigning or passing a move-type value moves
   it, and the source can no longer be used. `move x` spells the move out.
2. **Values are destroyed deterministically.** When the owner goes out of scope,
   the value's `drop` block runs, and then its fields are dropped. Locals are
   dropped in reverse declaration order.
3. **Borrowing**: `&x` (or `borrow x`) creates a shared reference and `&mut x`
   (or `borrow mut x`) creates an exclusive one. While a `&mut` borrow is live,
   nothing else may use the value. While a `&` borrow is live, the value may not
   be mutated or moved. A borrow lasts until the last use of the reference.
4. **No dangling references.** A reference can't outlive its referent. A
   function can return a reference only if it is derived from a reference
   parameter.
5. **Mutation must be visible**: you can mutate a value only through a `let mut`
   binding or a `&mut` reference.

```xero
struct Resource { id: i32 }

impl Resource {
    drop {
        println("released")         // `self` is `&mut Self` here
    }
}

fn take(r: Resource) {}             // takes ownership
fn look(r: &Resource) {}            // borrows
fn edit(r: &mut Resource) {}        // borrows exclusively

let mut r = Resource { id: 1 }
look(&r)
edit(&mut r)
take(r)                             // moved; `r` is gone
// look(&r)                         // error: use of moved value `r`
```

The rules in detail:

* **Moves in branches and loops** are tracked through control flow. A value
  moved in one branch counts as possibly moved afterwards, and a value moved
  inside a loop is an error on the next iteration. The compiler inserts a
  one-bit drop flag only where ownership depends on control flow, and LLVM
  removes most of them.
* **Moving a field out** (`let n = holder.name`) consumes the whole struct. The
  remaining fields are dropped immediately and the struct can't be used again.
  You can't move a field out of a type that has a `drop` block.
* **Two-phase borrows**: `v.push(v.len())` is allowed. The receiver's `&mut`
  borrow only becomes active when the call starts.
* `drop x` destroys `x` immediately.
* Assigning to a variable that owns a value drops the old value first.
* **References in values**: a struct, enum, closure or container that holds
  references (`Parser { input: &str }`, `Vec<&str>`, `Option<&T>`) keeps
  their borrows alive when it is moved, returned or stored. Passing such a
  value by `&mut` along with other borrows (`names.push(s.as_str())`) makes it
  hold those borrows too.
* **What a returned reference borrows** is worked out from the function body,
  so no lifetime annotations are needed. `fn field(&self) -> &T { &self.f }`
  keeps the receiver borrowed; `fn word(&mut self) -> &str` that returns part
  of `self.input` keeps only the text borrowed.
* A reference to a local can never be stored behind a reference
  (`self.r = &local`), because that memory may outlive the local.

### Data-race freedom

Safe code can't share mutable state: `&mut` is exclusive, and every access to
a `static mut` requires `unsafe`. The same rules apply across threads, so a
data race is a compile error (section 11).

* A value whose destructor can read what it borrows (a thread handle, a
  struct with a `drop` block holding references) keeps those borrows until
  it is destroyed, and must be destroyed before the data it borrows.

## 5. Functions and methods

```xero
fn add(a: i32, b: i32) -> i32 {
    a + b                           // last expression is the result
}

impl User {
    fn new(name: &str) -> User { ... }        // associated function: User.new("x")
    fn name(&self) -> &str { ... }            // shared borrow of the receiver
    fn rename(&mut self, n: &str) { ... }     // exclusive borrow
    fn into_name(self) -> String { ... }      // takes ownership
}

impl i32 {                                    // extension methods on any type
    fn squared(self) -> i32 { self * self }
}
```

Method calls borrow or dereference the receiver automatically (`u.rename("y")`
borrows `u` mutably). All calls are statically dispatched.

### Closures

```xero
let k = 10
let scale = |x: i32| x * k              // captures `k` by reference
let mut total = 0
let mut add = |x: i32| total += x       // mutates a capture: an FnMut closure
let name = String.from("xero")
let greet = move || println(&name)      // `move` takes ownership of captures

fn apply<F: Fn(i32) -> i32>(f: F, x: i32) -> i32 { f(x) }
apply(|x| x + 1, 5)                     // parameter types come from the bound

fn twice(f: fn(i32) -> i32, x: i32) -> i32 { f(f(x)) }
twice(double, 3)                        // named functions are values
twice(|x| x - 1, 3)                     // so are closures without captures
```

* Each closure has its own type, a struct holding its captures, plus a
  generated function. A parameter `F: Fn(A) -> R` (or `FnMut`) is
  monomorphized per closure, so calls are direct and usually inlined: no
  allocation and no dynamic dispatch.
* Captures are by reference, or by `&mut` when the body mutates them, which
  makes the closure `FnMut`. Calling an `FnMut` closure needs a `let mut`
  binding. `move` closures own their captures, and the borrow checker treats
  captures like any other borrow.
* Parameter types are inferred from an `Fn(...)` bound or a `fn(...)`
  parameter; otherwise annotate them (`|x: i32|`). A closure that uses
  `return` needs a return type: `|x: i32| -> i32 { ... }`.
* `fn(A) -> R` values are plain function pointers, compatible with C callbacks.
  Only closures without captures convert to them.
* A field holding a closure is called like a method: `self.on_click(x)`.

## 6. Traits and generics

```xero
trait Shape {
    fn area(&self) -> f64
    fn describe(&self) { println(self.area()) }   // default method
}

trait Solid extends Shape {                         // supertraits
    fn volume(&self) -> f64
}

impl Shape for Circle { fn area(&self) -> f64 { ... } }

fn total<T: Shape>(items: &[T]) -> f64 { ... }
fn show<T>(x: T) where T: Shape + Print { ... }

struct Stack<T> { items: Vec<T> }
impl<T> Stack<T> { ... }
impl<T: Print> Print for Stack<T> { ... }
```

* Generics are **monomorphized**: each instantiation compiles to specialized
  machine code, so a generic costs nothing at run time.
* Generic bodies are checked **per instantiation**, like templates. A generic
  function can use any operation that its concrete type arguments support, for
  example `math.max<T>` works with any ordered numeric type. Trait bounds are
  checked at each call site.
* Type arguments are inferred from the arguments and the expected type, or
  given explicitly: `size_of<u64>()`, `Vec<i32>.new()`, `mem.alloc<T>(n)`.
* Two implementations of the same trait for the same type are rejected.

## 7. Expressions and statements

* `let x = e`, `let mut x: T = e` and `let (a, b) = pair`. Every variable must
  be initialized when it is declared.
* `if c { a } else { b }` is an expression. `if x is Some(v) { ... }` matches
  and binds in a single step.
* `match value { pattern => expr ... }` must be exhaustive.
* `while cond { }`, `while x is Some(v) { }`, `loop { ... break value }`.
* `for i in 0..n`, `for i in a..=b`, `for x in &array`, `for x in slice`,
  `for x in array` (when the elements are copyable).
* Operators, by precedence: `||` · `&&` · comparisons (not chainable) and `is`
  · `..` · `|` · `^` · `&` · `<< >>` · `+ -` · `* / %` · `as` · unary
  `- ! * & move copy try` · calls, fields and indexing.
* `x += 1` and the other compound assignments: `-= *= /= %= &= |= ^= <<= >>=`.
* Slicing: `&arr[a..b]`, `&arr[a..]`, `&arr[..b]`, `&arr[a..=b]` and `&mut v[..]`
  on arrays, slices and `&str`. The range is bounds-checked once, when the
  slice is formed.
* Untyped literals default to `i32` and `f64`, unless context or a later use
  gives them a type: in `let mut total = 0` followed by `total += x` with
  `x: u64`, `total` is a `u64`.

### Operators on your own types

Operators on structs, enums, tuples and arrays call prelude traits, and are
statically dispatched:

| Operators | Trait | Method(s) | Operands |
|---|---|---|---|
| `==` `!=` | `Eq` | `eq` (`ne` is provided) | borrowed |
| `<` `<=` `>` `>=` | `Ord` (extends `Eq`) | `cmp` returns `Ordering.Less/Equal/Greater` (`lt`, `le`, `gt`, `ge` are provided) | borrowed |
| `+` `-` `*` `/` `%` | `Add` `Sub` `Mul` `Div` `Rem` | `add` `sub` `mul` `div` `rem` | moved |
| unary `-` | `Neg` | `neg` | moved |

```xero
impl Add for Vec2 {
    fn add(self, o: Vec2) -> Vec2 { Vec2 { x: self.x + o.x, y: self.y + o.y } }
}
impl Ord for Version {
    fn cmp(&self, o: &Version) -> Ordering { compare(self.major, o.major) }
}
```

Built-in numbers, `bool` and `str` implement these traits too, so bounds like
`T: Ord` and `T: Add` accept them. `compare(a, b)` returns the `Ordering` of any
two values that support `<` and `>`. `a += b` on a copyable user type means
`a = a + b`.

### Patterns

`_`, bindings (`x`, `mut x`), literals (`3`, `'a'`, `true`, `-1`), ranges
(`1..=9`, `0..10`), tuples `(a, b)`, variants `Some(x)`, `Shape.Circle(r)` and
`Color.Red`, positional struct patterns `Point(x, y)`, and alternatives `A | B`.
When you match through a reference, bindings become references automatically.

### Integer semantics

Arithmetic wraps on overflow (two's complement), so it is never undefined
behavior. Division by zero and `MIN / -1` stop the program. Shift amounts are
masked to the bit width. Float-to-int casts saturate. Array and slice indexing
is bounds-checked, and an out-of-range index stops the program.

## 8. Error handling

Errors are values. There are no exceptions and no unwinding.

```xero
fn parse(s: &str) -> Result<i32, ParseError> {
    if s.is_empty() {
        throw ParseError.Empty              // return Err(ParseError.Empty)
    }
    ...
    Ok(n)
}

fn sum(a: &str, b: &str) -> Result<i32, ParseError> {
    let x = try parse(a)                    // unwrap Ok, or return the Err early
    let y = try parse(b)
    Ok(x + y)
}

let n = parse(text) catch e {               // handle the error inline
    println("bad input")
    0
}
let m = lookup(key) catch { -1 }            // works on Option too
```

`try`, `throw` and `catch` compile to ordinary branches and cost nothing extra.
`panic(msg)` prints a message and aborts the process. Use it for bugs, not for
control flow.

## 9. Unsafe code and the hardware

`unsafe { ... }` blocks and `unsafe fn` allow:

* dereferencing raw pointers (`*p`, `*(p + i)`);
* accessing a `static mut`;
* calling foreign or `unsafe` functions;
* intrinsics: `ptr_read`, `ptr_write`, `volatile_load`, `volatile_store`,
  `drop_in_place`, `slice_from_raw`, `slice_from_raw_mut`, `str_from_raw`,
  `forget(value)` (skip a destructor), `asm("...")`, and atomics on integers
  and pointers: `atomic_load`, `atomic_store`, `atomic_swap`, `atomic_add`,
  `atomic_sub`, `atomic_cas` (all sequentially consistent);
* `syscall(number, args...)`: a raw Linux system call with up to six integer
  or raw-pointer arguments, returning `isize` (`-errno` on failure).

Safe intrinsics: `size_of<T>()`, `align_of<T>()`, `trap()`, `fence()`,
`os_argc()` and `os_argv()` (the raw command line; `std.env` wraps them).

```xero
unsafe fn printf(fmt: *u8, ...) -> i32          // C variadic

export fn kernel_main() {
    let uart = 0x4000_C000 as *u32
    unsafe {
        volatile_store(uart, 'X' as u32)
        asm("wfi")
    }
}
```

Casts with `as`: between numeric types, `bool → int`, field-less `enum → int`,
`&T → *T`, `&[T]`/`&str → *T`/`*u8`, between raw pointer types, and between
pointers and `usize`. Pointer arithmetic: `p + n` and `p - n` advance by whole elements.

## 10. The standard library

| Module | Contents |
|---|---|
| prelude | `Option`, `Result`, `Print`, `print`/`println`/`eprint`/`eprintln`, `panic`, `assert`, operator traits (`Eq`, `Ord`, `Add`, `Sub`, `Mul`, `Div`, `Rem`, `Neg`), `Ordering`, `compare`, `str` methods (`as_bytes`, `is_empty`, `byte_at`) |
| `std.mem` | `Box<T>`, `alloc`, `realloc_array`, `dealloc`, `swap`, `replace`, `copy_values` |
| `std.vec` | `Vec<T>`: `push pop get get_mut at at_mut set swap insert remove clear reserve as_slice as_mut_slice`, and with closures `map filter for_each any all count position sort_by sort` |
| `std.string` | `String`: `from new from_utf8 push_str push_byte push_int as_str as_bytes into_bytes len equals clear`, and `is_utf8` |
| `std.math` | `min max clamp abs isqrt sqrt pow_int gcd PI E` |
| `std.io` | `read_into write_all exit_process STDIN STDOUT STDERR` (errors are `errno` values) |
| `std.fs` | `File` (`open`, `create`, `append`, `read`, `read_all`, `read_to_string`, `write`, `write_str`; closed when destroyed), `read_to_string`, `read`, `write`, `remove`, `Error` (`code`, `message`, printable) |
| `std.env` | `args`, `arg`, `arg_count` |
| `std.os` | `NAME` (`"linux"`, `"macos"`, `"windows"`, `"wasm"`, `"none"`), `ARCH`, `POINTER_BITS` |
| `std.thread` | `start` (what `spawn` calls), `Thread<T, F>` with `join`, `sleep_ms`, `yield_now` |
| `std.sync` | `Mutex<T>` (`lock`, `with`, `into_inner`; guards `get`, `get_mut`, `set`), `Channel<T>` (`send`, `recv`, `try_recv`, `close`, `len`), `Atomic<T>` (`load`, `store`, `swap`, `fetch_add`, `fetch_sub`, `compare_exchange`), `AtomicBool` |

The standard library is written in Xero. Code you don't call is never compiled
into your program.

```xero
import std.env
import std.fs

fn main() {
    if env.arg(1) is Some(path) {
        let text = fs.read_to_string(path) catch e {
            eprint("cannot read the file: ")
            eprintln(e)              // e.g. "no such file or directory"
            return
        }
        print(&text)
    }
}
```

### The runtime

On **Linux**, Xero programs don't use the C library. The standard library
talks to the kernel with system calls, and brings its own heap (an
`mmap`-based allocator with per-thread arenas), threads (`clone` and
futexes) and program entry point. An executable is static, starts
instantly, and runs on any Linux kernel of the same architecture: "hello,
world" is about 9 KB. Because no C library is involved, building for another
architecture needs nothing but `lld`:

```console
$ xeroc build app.xr --target aarch64-linux     # also riscv64-linux
```

A program that declares C functions (`unsafe fn puts(s: *u8) -> i32`) is
linked with the system C library instead, and so is a library or object
(`--lib`, `--emit obj`) that someone else links. `--libc` and `--no-libc`
choose explicitly.

On **macOS** programs use the system library (`libSystem`), which is the only
supported way to talk to the macOS kernel. On **Windows** they use the C
runtime and `kernel32`. On bare-metal and wasm targets there is no OS: a
program that prints or allocates must provide `write`, `malloc`, `free`,
`realloc`, `memmove` and `abort` itself.

## 11. Concurrency

```xero
import std.sync

fn main() {
    let data = [1, 2, 3, 4]
    let mut evens: Vec<i32> = Vec.new()

    let sum = spawn { data[0] + data[1] + data[2] + data[3] }   // borrows `data`
    let fill = spawn {                                          // borrows `evens` exclusively
        for x in data {
            if x % 2 == 0 { evens.push(x) }
        }
    }
    println(sum.join())       // 10
    fill.join()
    println(evens.len())      // 2

    let owned = String.from("moved in")
    let t = spawn move { owned.len() }   // the thread owns `owned`
    println(t.join())

    let counter = sync.Mutex.new(0)
    let jobs: sync.Channel<i32> = sync.Channel.new()
    {
        let worker = spawn {
            while jobs.recv() is Some(n) {
                counter.with(|c| *c += n)
            }
        }
        for n in 1..=10 { jobs.send(n) }
        jobs.close()
    }                         // `worker` is joined here
    println(*counter.lock().get())   // 55
}
```

* `spawn { ... }` runs the block on a new OS thread and returns a
  `Thread<T, F>`, where `T` is the block's value. `join()` waits for it and
  returns the value. A thread that isn't joined explicitly is joined when its
  handle is destroyed, at the end of its scope at the latest.
* Because of that, a thread can borrow local variables, and the borrow checker
  applies the usual rules for the handle's whole lifetime. Data used through
  `&` can be shared by any number of threads. Data used through `&mut`
  belongs to one thread until it is joined. Anything else is a compile error:

  ```xero
  let mut counter = 0
  let t = spawn { counter += 1 }
  counter += 1        // error: cannot use `counter` because it is mutably borrowed
  ```

* `spawn move { ... }` moves the values the thread uses into it.
* To share *mutable* state between threads, use `Mutex<T>`, `Channel<T>` or
  `Atomic<T>` from `std.sync`. They are used through shared references and
  synchronize internally.
* There is no unwinding: a panic in any thread ends the whole program.
* Threads are Xero's own (`clone` and futexes) on Linux, POSIX threads on
  macOS and `kernel32` threads on Windows. On bare-metal and wasm targets,
  starting a thread is a runtime error.
* `async` and `await` are still reserved.
