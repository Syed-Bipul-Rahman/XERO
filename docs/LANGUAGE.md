# The Xero Language Reference (v0.1)

This document describes the language as implemented by `xeroc` 0.1. Features
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
| Concurrency | `async` `await` `spawn` (all reserved) |
| Errors | `try` `catch` `throw` |
| Literals | `true` `false` |
| Misc | `as` `in` `is` `self` `Self` |

Xero deliberately has no `class`, `new`, `null`, `extends` for classes,
`implements`, `go` or `chan`. `struct`, `trait` and `impl` cover composition.
Concurrency is planned as `spawn { ... }` plus library types such as `Channel<T>`.

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
* **References in fields**: struct and enum fields can't be references in
  v0.1, because there are no lifetime parameters yet. Generic containers can
  still hold references, for example `Option<&T>` as returned by `Vec.get`.

### Data-race freedom

Safe code can't share mutable state: `&mut` is exclusive, and every access to
a `static mut` requires `unsafe`. When `spawn` arrives, the same rules will
apply across threads (see the roadmap).

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
* `a == b` works on numbers, `bool`, raw pointers, field-less enums and `&str`.

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
  `drop_in_place`, `slice_from_raw`, `slice_from_raw_mut`, `str_from_raw`
  and `asm("...")`.

Safe intrinsics: `size_of<T>()`, `align_of<T>()`, `forget(value)`, `trap()`.

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
| prelude | `Option`, `Result`, `Print` trait, `print`/`println`/`eprint`/`eprintln`, `panic`, `assert`, `str` methods (`as_bytes`, `is_empty`, `byte_at`) |
| `std.mem` | `Box<T>`, `alloc`, `realloc_array`, `dealloc`, `swap`, `replace`, `copy_values` |
| `std.vec` | `Vec<T>`: `push pop get get_mut at at_mut set swap insert remove clear reserve as_slice as_mut_slice` |
| `std.string` | `String`: `from new push_str push_byte push_int as_str len equals clear` |
| `std.math` | `min max clamp abs isqrt sqrt pow_int gcd PI E` |
| `std.io` | `read_into write_all exit_process STDIN STDOUT STDERR` |

The standard library is written in Xero. Code you don't call is never compiled
into your program, so a freestanding program doesn't depend on libc unless it
uses something that needs it (printing or allocation).
