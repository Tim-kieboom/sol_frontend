# The Sol Programming Language

Sol is a systems language with a Rust-style borrow checker and a terser, more expression-oriented
surface syntax. Its planned companion, **Gol**, is a transpile target that wraps heap values in
reference-counted boxes (ARC, à la Swift) for a runtime memory model instead of static borrow
checking — same language, two backends. Goul is not designed yet; see §18.

This document is both a reference and an introduction: it explains *why* a feature works the way
it does, not just its syntax, so it can be read cover-to-cover to learn the language or used as a
lookup reference. Anything not yet decided is marked **Open:** inline, and the full list is
collected in the Appendix.

---

## 1. Lexical Basics

- Files use the `.sol` extension.
- **Whitespace is not significant.** Sol is free-form like Rust or C, not newline-sensitive like
  Python. Writing one statement per line is just house style.
- **`;` has exactly one job: discard a value.** Since newlines don't separate statements, `;` is
  needed to stack multiple statements on one line (`a := 1; b := 2`), and it's also the one way
  to suppress a block's final expression from being that block's value (see §4). **These two
  cases are exhaustive** — there's no other situation where a semicolon is required.
- Three comment forms: `//` single-line, `///` doc comment, `/* ... */` multi-line.
- Punctuation used for declarations:
  - `::` — associated constant (`LIST_GROW :: f32.(2)`)
  - `:=` — local variable with inferred type (`x := 5`)
  - `:` — type annotation (`mut len: uint = 0`)
- **String interpolation**, two forms:
  - `f"{expr}"` — eagerly builds a `str`; any expression is allowed inside `{}`, not just bare
    names (`f"{el * 2}"`).
  - `fstr"{expr}"` — the lazy underlying form, mirroring Rust's `format_args!`: produces a
    "format arguments" value a function like `println` can consume directly, without forcing an
    allocation first. `f"..."` is sugar for building an `fstr"..."` and immediately materializing
    it into a `str`.

---

## 2. Modules & Visibility

Sol has one `import` keyword doing the job Rust splits across `use` and `mod` — there's no
separate module-declaration keyword.

```sol
import (
    Std.{Fmt, Display},
    Core.{
        Mem.{malloc, free},
        Array
    }
)
```

- Import trees nest with `.` as the path separator (Rust's `use a::{b, c::{d, e}}`, but with `.`).
- **Imports are scoped to where they're written**, following ordinary block-scoping: a top-level
  `import` is visible through the whole file; one written inside a function body is only visible
  in that function.
- **A module's visibility is its file name's capitalization.** Since there's no `mod`/`pub mod`
  distinction, the file itself carries that information: `Math.sol` (uppercase-leading) is a
  public module, `math.sol` (lowercase-leading) is private. This is file/module granularity
  only.
- **Item-level visibility** inside a file uses `pub`, `pub(crate)`, `pub(super)` — unaffected by
  the file-capitalization rule above. Private-by-default at the item level.

**Multi-file projects use a `Sol.toml` manifest.** A relative import (`import crate.sub`) finds
`sub.sol` next to the importing file within the same crate; a named import (`import Math`)
resolves against a dependency declared in `Sol.toml`:

```toml
name = "SolTest"

[dependencies]
Std = {path = "lib/Std", linkage = "static"}
Core = {path = "lib/Core", linkage = "static"}
```

See `docs/crate-system-plan.md` for the compiler-side `CrateForest`/`.solo` design this maps onto.

---

## 3. Types

### 3.1 Primitives
`int`, `uint`, `i64`, `u64`, `u8`, `f32`, `f64`, `bool`, `char`, `str`, `&str`, `none`

- `int`/`uint` are machine-width; explicit-width types (`i64`, `u8`, ...) are also available.
- `str` is an owned string; `&str` is a borrowed view (Rust's `String`/`&str` split).
- `none` is the unit/empty type. Since it has exactly one possible value, **any parameter of
  type `none` can be omitted from a call entirely** — a general calling-convention rule, not
  specific to any one construct.

### 3.2 Optionals
`?T` is an optional type (`[]?T` is an array of optionals). `.null{}` is a match-chain (§10)
specialized to the `null` case: the argument is the fallback for when the value is null, and the
value itself when it isn't.

```sol
option: ?int = null

// if null then 1 is returned
number: int = option.null{1}

// if null then panic is thrown
panicUnwrap: int = option.null{panic("panic message")}

// ignores null
//!!NEVER DO THIS UNLESS YOU REALLY KNOW IT CAN NEVER BE NULL!!
unsafe { unwrap: int = option.null{undefined} }
```

### 3.3 Arrays, Slices, and Array Literals

Six array-family types, all sharing the `[marker]T` shape — the marker before the element type
says where the data lives and what's tracked alongside it:

| Type | Syntax | Representation |
|---|---|---|
| `StackArray` | `[N]int` | stack value, `N` part of the type (LLVM `[int x N]`) |
| `Slice` | `[&]int` | `{buffer: RawPtr, len: uint}` — borrowed, immutable view |
| `MutableSlice` | `[&mut]int` | `{buffer: RawPtr, len: uint}` — borrowed, mutable view |
| `Array` | `[]int` | `{buffer: RawPtr, len: uint}` — owned, heap-allocated |
| `DynamicArray` | `[dyn]int` | `{buffer: RawPtr, len: uint, cap: uint}` — owned, growable |

**`StackArray` (`[N]T`)** — a fixed-size stack value; `N` is part of the type, exactly as before.
**`N` must be a compile-time value** (a literal or a `const` fn, §5) — a runtime `N` couldn't
produce a fixed type, so a runtime-length fill goes through heap `Array` instead (below):
```sol
COMPTIME_LEN :: 4
mut stackArray: [4]int = [1, 2, 3, 4]   // plain literal
stackArray = [for COMPTIME_LEN => 0]    // fill/comprehension; comptimeLen() must be a `const` fn (§5)

a: [3]i64 := [1, 2, 3]                  
b := [i64: 1, 2, 3]                     // explicit element-type prefix, for when inference is needed
c := [i8: for i in 3 => i]             // named loop variable → [0, 1, 2]
```

**`Array` (`[]T`)** — a heap-allocated, owned array whose **length is fixed once created and
never resized in place** (no `cap`, no `push`/`pop`/realloc — the heap counterpart to `[N]T`, not
to `[dyn]T`). Growing means building a new `Array`/`[dyn]T` and moving data over. `new[...]`
allocates a literal/fill form on the heap:
```sol
mut array: []int = new[1, 2, 3, 4]
array = new[for runtimeLen() => 1]    // fill length can be a runtime value here, unlike [N]T
```

**`DynamicArray` (`[dyn]T`)** — the growable one (Rust's `Vec<T>`): adds `cap` alongside `buffer`
and `len`, and supports `push`/`pop`. `new dyn[...]` is the literal/fill form; `withCapacity` is an
ordinary associated constructor (§3.5), called through the type the same way `int.parse(...)` is:
```sol
mut dynArray: [dyn]int = new dyn[1, 2, 3, 4]    
dynArray = new dyn[for runtimeLen() => 1]       
dynArray = [dyn]int.withCapacity(10)            
dynArray.push(10)
value: ?int = dynArray.pop()     // pop(): ?T — .null{} (§3.2) supplies the empty-array fallback
```

**`Slice`/`MutableSlice` (`[&]T`/`[&mut]T`)** — a borrowed view, never owning: `{buffer, len}`
exactly like `Array`, but the `buffer` is borrowed rather than owned, subject to the same aliasing
rules as any other borrow (§11). `&[...]`/`&mut[...]` borrows an array literal — stack-allocating
the backing data as a temporary and handing back a slice over it, empty (`&[]`) being the
degenerate case:
```sol
slice: [&]int = &[]              // empty slice
mutSlice: [&mut]int = &mut[]     // empty mutable slice
overlay: [&]int = &[1, 2, 3]     // slice over a stack-allocated temporary
```

### 3.4 Generics
`List<T>` — angle-bracket generics, with default type parameters supported:
`Res<O = none, E = str>`.

Bounds use Rust's syntax:
```sol
function<T: Display>(value: T)          // inline bound
function<T>(value: T) where T: Display  // where clause, for more complex bounds
function(value: impl Trait)             // anonymous generic parameter, no <...> needed
function(): impl Trait                  // works in return position too
```

### 3.5 Type-Call Construction Shorthand

A type name followed by `.(args)` or `.()` constructs a value — see §6 for the full story. Two
related shorthands:
- `Type.` (bare, no call) denotes **the constructor as a value** — a one-argument lambda
  equivalent to `el => Type.(el)`. Used directly inside a match-chain arm where a value (not a
  function) is expected, a bare `Type.` implicitly applies itself to `it`
  (`this.Int{f64.}` ≡ `this.Int{f64.(it)}`). **This isn't constructor-specific: any bare reference
  to a one-argument function or method, used as a value in that same position, gets the same
  implicit-apply-to-`it` treatment** — `Type.` is just the most common case, not a special rule of
  its own.
- `Type.EMPTY`/similar named constants (e.g. `str.EMPTY`) are ordinary associated constants,
  unrelated to the `Type.` shorthand above — they just happen to coincide for `str`.

---

## 4. Expressions, Blocks, and Statements

**Sol follows Rust's block-expression model: everything is an expression, and a block's last
expression is its value unless suppressed by `;`.** This applies uniformly — `if`, `match`,
function bodies, and `for` loops with `limit` (§12) all follow the same rule, not just function
bodies:

```sol
x := if cond { 1 } else { 2 }         // if/else as an expression — both arms' types must agree
y := match value { Ok(v) => v, Err(_) => 0 }

doSomething(): int => 1

returnsNone() {
    println("hello")
    doSomething();   // trailing `;` discards the value — block now evaluates to `none`
}

returnsInt(): int {
    println("hello")
    doSomething()    // no trailing `;` — block evaluates to int
}
```

**Items can be nested inside other items.** Functions can be declared inside functions, and
presumably structs/enums/unions/traits can be declared locally too. A **named** function or
method, wherever it's declared, **never captures its enclosing scope** — exactly like Rust's
`fn`. Only an **anonymous closure literal** (`(params) => expr`) captures:

```sol
name() { ... }            // a normal, non-capturing function — even if nested inside another
() => 1 + 1                // an anonymous closure literal — CAN capture the enclosing scope
```

Closures follow Rust's model: capture mode (borrow / mutable borrow / move) is inferred from
usage, with an explicit `move` to force capture-by-move. Closure types are presumably a
`Fn`/`FnMut`/`FnOnce`-equivalent family, usable via `impl Trait` (§3.4).

---

## 5. Functions

**Sol has no `fn` keyword.** A function is just its name and parameter list:

```sol
add(a: int, b: int): int => a + b
```

`const`, `pub`, and `async` (§15) are prefix modifiers on that same bare form — there's no `fn`
keyword for them to modify:

```sol
const comptimeAdd(a: int, b: int): int => a + b     // comptime-evaluable
pub validate(input: str): bool => input.len() > 0   // => sugar for a single-expression body
async fetchUser(id: int): str { ... }               // §15
```

**No ad-hoc overloading, anywhere — with one deliberate exception.** A given function/method
name resolves to exactly one signature; Sol doesn't dispatch on argument types or count the way
C++/Swift do. **The one exception is the `This.(name: T)` constructor form (§6):** a struct can
declare several of these, each distinguished by its single parameter's type, and the compiler
dispatches on the argument's type at the call site. This is a narrow, special-cased carve-out
for construction specifically — not a general overloading mechanism — and it's the same
type-dispatch principle behind the array-literal constructor `This.[T](param)` (§6) accepting
multiple element types too.

---

## 6. Structs

```sol
pub struct List<T> {
    LIST_GROW :: f32.(2)

    mut len: uint = 0
    mut buffer: []?T = []

    pub This.() => This{..}
    pub This.[int](array) => This{ buffer: array, len: array.len() }

    pub len(&this): uint => this.len
}
```

- `pub` on the struct and per-member controls visibility (§2).
- Fields default to **immutable**; `mut` opts a field into mutability.
- Field default values (`= 0`, `= []`) fill in whatever a constructor doesn't set.

**Construction has three independent mechanisms**, chosen by shape:

1. **`This.() => This{..}` — the zero-argument/default constructor.** Its own dedicated syntax.
   `This{..}` is the struct-literal spread, filling every field from its default value.
2. **`This.(name: T) => This{..}` — construct from a value of type `T`.** A struct can declare
   several of these side by side, each with a differently-typed single parameter; the compiler
   dispatches on the argument's type at the call site (the one overloading exception, §5):
   ```sol
   pub struct Number {
       mut inner: f64
       This.(float: f64) => This{inner: float}
       This.(interger: int) => This{inner: f64.(interger)}
   }

   mut number := Number.(int.(1))     // one-arg call → the `int` constructor
   number = Number.(f64.(2))          // one-arg call → the `f64` constructor
   assertEq(number.typeof, Number)
   ```
   `Type.(arg)` dispatches on `arg`'s type to the matching `This.(name: T)` definition. No-args
   always means `This.()`; one-arg always means this type-dispatched form — the two are cleanly
   split by arity. This is also the real mechanism behind primitive conversions like `f32.(2)`,
   `u8.(1)`: built-in constructors the language ships for its own primitive types.
3. **`This.[T](param)` — the array-literal constructor.** Its own mechanism, since it routes a
   *literal syntax form* rather than converting a single value. **A struct can declare more than
   one, for different element types**, following the same type-dispatch principle as (2):
   ```sol
   struct IntArray {
       array: []int
       len: uint

       This.[int](array) => This{
           len: array.len(),
           array,
       }

       This.[u8](array) => This{
           len: array.len(),
           array: array.intoIter().map(int.).toArray(),
       }
   }

   mut array := IntArray.[1, 2, 3, 4]   // routes to This.[int](array)
   array = IntArray.[1_u8, 2, 3, 4]     // routes to This.[u8](array)
   assertEq(array.typeof, IntArray)
   ```

**Fallibility is signaled by the return type, with no separate keyword.** Every constructor
above implicitly returns `This` — none of them declare a return type at all. A constructor that
*can* fail (like `Limit<T, RANGE>`'s, §20) must say so by explicitly declaring
`: Res<This>` (or `: ?This`):
```sol
This.() => This{..}                          // infallible — no return type, always succeeds
This.(value: T): Res<This> => ...             // fallible — explicit Res<This> return type
```
This is the general rule for constructors, not something specific to `Limit`: an **omitted
return type means infallible**, and the only way to be fallible is to opt in by writing the
`Res<This>`/`?This` annotation out — the same way any other function already signals whether it
can fail via its declared return type (§13), just applied consistently to constructors too.

Regular methods use `&this`/`&mut this` receivers (§11), `=>` for single-expression bodies, `{ }`
for multi-statement ones.

---

## 7. Traits and `impl`

Sol supports **two `impl` spellings**, chosen by how many methods a trait needs:

```sol
use Type {
    // single-method inline form — trait named up front, no nested block
    impl Index<Out = T> index(&this, index: uint): &This.Out {
        value := &this.buffer[index]     // bounds-checked automatically
        unsafe { value.null{undefined} }
    }

    // block form — for multi-method traits
    impl MyTrait {
        method1() {}
        method2() {}
    }
}
```

Both forms work either inside a type's own body, or inside a `use` block extending a type from
outside its definition (§8). `use` and `impl` can also **fuse into one header** when the
extension's only content is that one conformance:

```sol
use Literal impl Display {
    fmt(&this, &mut f: &mut Formatter) { ... }
}
```

**A struct can implement the same generic trait multiple times if the generic parameters
differ** — including when the parameter is in output position (`Index<Out=T>`), not just input
position. Disambiguation for output-position cases comes from context/return-type inference at
the call site; **when nothing at the call site disambiguates, it's a compile error** — the same
"ambiguous associated type" failure Rust produces, rather than silently picking one impl by
declaration order. What's disallowed is implementing a trait with *identical* generic parameters
twice.

---

## 8. `use` Blocks and Extension

```sol
use Literal {
    VALUE :: 1

    tryIntoFloat(this): ?f64 {
        this.Int{f64.}.Uint{f64.}.Float{it}.else{null}
    }
}
```
`use Type { ... }` adds constants/methods to a type from outside its definition (Swift's
`extension`). Free-standing `Type.method(...)` (no `use` block) extends *any* type, including
primitives, without ceremony:
```sol
int.square(this): int => this ** 2
```
The first parameter doesn't have to be `this` — a name other than `this` makes it a namespaced
function under `Type` rather than an instance method, still called through `Type.name(...)`:
```sol
int.parse(value: &str): Res<int> { ... }   // called as int.parse("42"), not "42".parse()
```

---

## 9. Enums

`enum Name as BackingType { Variant = const expression, ... }`. `.value` gives the backing value
from the compile-time expression; `.tag` gives the ordinal index. The compiler auto-generates
`fromValue`/`fromTag` reverse lookups, both returning an optional (`null` if nothing matches).

```sol
FOR_STR :: "for"
FIRST_TAG :: 0_u32

enum KeyWords as &str {
    ForLoop = FOR_STR,
    InForLoop = "in",
    TypeMatching = "match",
}

#[test]
test_basic_enum_features() {
    variant := KeyWords.ForLoop
    assertEq(variant.typeof, KeyWords)
    assertEq(variant, KeyWords.ForLoop)

    string := variant.value
    tag := variant.tag
    assertEq(string, FOR_STR)
    assertEq(tag, FIRST_TAG)

    fromValue := KeyWords.fromValue(FOR_STR)
        .null{panic(f"KeyWords.fromValue({FOR_STR}) should not be null")}

    fromTag := KeyWords.fromTag(FIRST_TAG)
        .null{panic(f"KeyWords.fromTag({FIRST_TAG}) should not be null")}

    assertEq(variant, fromTag)
    assertEq(variant, fromValue)
}
```

---

## 10. Unions and Pattern Matching

### 10.1 Declaring a union

```sol
union Literal {
    None,
    Int(int),
    Str{tag: str, value: str},
}

parse_int(value: &str): int {
    int.parse(value).Err{panic("parse failed")}
}

#[test]
test_basic_union_features() {
    mut variant := Literal.None
    variant = Literal.Str{tag: "f".copy, value: "hello".copy}
    variant = Literal.Int(1)
    assertEq(variant.typeof, Literal)
    assertEq(variant, Literal.Int(1))

    number := variant
        .None{0}
        .Int{it}
        .Str{parse_int(it.value)}
}
```
Variants can be unit (`None`), tuple-style (`Int(int)`), or struct-style with named fields
(`Str{tag: str, value: str}`) — all three can coexist in one union.

### 10.2 Three ways to inspect a union

**The match-chain (`.Variant{}`)** — a `match` expression spelled as a chain. Each
`.Variant{ body }` is one arm; the whole chain *is* the match, not a sequence of steps:

```sol
Literal.tag(&this): &'static str {
    this
        .None{"none"}
        .Int{"int"}
        .Str{"str"}
}
```
This desugars to `match this { None => "none", Int(x) => "int", Str(s) => "str" }`.
- The payload binds to `it` by default, or an explicit name: `this.Int{x => x + 1}`.
- Chains are **never required to be exhaustive.** Any variant left out passes through
  implicitly — `.else{}` overrides that default rather than satisfying a requirement, and it
  still binds the passthrough payload as `it`, so it can transform rather than just replace it:
  ```sol
  value: Res<int> = Ok(1)

  number := value.Err{1}.Ok{it + 1}   // ≡ match value { Err(_) => 1, Ok(val) => val + 1 }
  number := value.Err{1}              // ≡ match value { Err(_) => 1, other => other }
  number := value.Err{1}.else{it + 1} // ≡ match value { Err(_) => 1, other => other + 1 }
  ```
- Chain arms are **read-only, by-value** — no `&mut` access to a payload through a chain.
  **Match-and-mutate reuses `if type` instead** (§10.2/§19.2), rather than inventing a separate
  construct: `if type Int(v) := &mut expr { ... }` binds `v: &mut int`.
- **A partial chain's unhandled arm passes through the *unwrapped payload* of whichever
  variant(s) remain**, not the whole original union value — `other` in `value.Err{1}` (Ok
  unhandled) binds to `Ok`'s `int` payload directly, which is exactly why the type-checks above
  work (`Err{1}` is `int`, the `Ok` passthrough is also `int` — both arms unify, per §4's ordinary
  rule). When more than one variant remains unhandled, `other`'s type is the union of those
  variants' payload types. For unit variants (no payload), `other` is just the unit value itself:
  ```sol
  union Foo { One, Two, Three }
  obj := Foo.One
  v := obj.One{panic("msg")}   // ≡ match obj { One => panic("msg"), other => other }
  ```

**The map-chain (`->Variant{}`)** — a completely different thing: single-variant `map`/`map_err`,
Rust-style, applied one at a time rather than as a combined match:
```sol
res: Res<int> = Ok(1)
newRes := res->Ok{str.(it)}   // like Rust's res.map(|el| el.to_string())
assertEq(newRes.typeof, Res<str>)
```
Each `->Variant{}` only transforms its one variant, passing every other variant through
unchanged; chaining several is sequential, not a single expression like `.` chains are.

**The traditional block `match`** — Rust's `match`, used directly, for nested/multi-value
patterns the chain forms can't express:
```sol
match (resultA, resultB) {
    (Ok(a), Ok(b)) => a + b,
    (Err(e), _) => panic(e),
}
```
This one *is* exhaustive, with Rust's full pattern grammar (tuples, guards, bindings).

**If-let**: `if type Err(err) := newRes { ... }`, binding `err` only inside the block.

**`typeof`**: `newRes.typeof` returns a first-class, comparable type value, comparable **both at
compile time** (e.g. specializing generic code during monomorphization) **and at runtime** (e.g.
comparing two `&any`'s runtime types, §19) — the same dual-purpose role as Odin's `typeid`, which
this design already takes as its model.

---

## 11. Ownership & Borrowing

- **Move-by-default.** Assigning or passing a value by value moves it; the old binding becomes
  invalid, exactly like Rust.
- **Aliasing: strict, static, Rust-style.** At any point a value is borrowed by exactly one
  `&mut`, or by any number of `&`, never both. Violations are compile errors.
- **Lifetimes: elided by default; Rust's `'a` syntax directly for the rare explicit case**
  (struct fields holding borrows, some higher-order signatures). No new sigil invented.
- **`&this`/`&mut this`** are borrowed/mutably-borrowed method receivers; free-standing
  `&x`/`&mut x` generalize the same rule to any binding. `[&]T` (a slice) is itself a borrow,
  subject to the same aliasing rules.

**Two duplication mechanisms, mirroring Rust's `Clone`/`Copy` split:**
- **`.copy` — a keyword, not a method call, matching `.await` (§15).** Written without
  parentheses: `b := a.copy`, not `a.copy()`. It performs a compiler-provided, independent
  duplicate of any type, on by default. Opt out with `#[!Copy]` for types where duplication
  shouldn't be allowed at all (a file handle, a mutex guard, ...).
- **`AutoCopy` (≈ Rust's `Copy`) — a real trait, *not* implemented by default; opt-in only.** A
  type implementing `AutoCopy` gets *implicit* copy-on-assignment (`a := b` duplicates rather
  than moves). Opt in with `use Type impl AutoCopy {}`. Primitive scalars are the built-in
  `AutoCopy` implementers.

  This establishes the language's **general derivation pattern**: `#[!Trait]` suppresses a
  default-on capability; `use Type impl Trait {}` opts into one that isn't on by default.

**`Drop`**, for custom cleanup, written with the single-method inline `impl` form:
```sol
impl Drop drop(&mut this) { free(this.buffer) }
```
Called automatically at scope-exit for a value's final (non-moved-from) owner.

**A moved-from binding can always be reassigned.** The slot left behind by a move is simply empty;
only *reading* a moved-from value is an error, exactly like Rust's move-then-reassign pattern —
`mut` doesn't add any extra restriction here beyond ordinary move-checking.

**Idea, not designed (arena allocation for non-escaping-from-allocating-frame values):** narrowed,
after grill-me, from a full region-inference design (general cross-scope regions, ML Kit-style) to
one specific, tractable case — a heap value (typically growable, e.g. a locally-built `List<T>`)
that's provably never returned, never moved into a longer-lived place, and never captured by an
escaping closure/spawn from the function frame that allocated it gets bump-allocated out of a
per-frame arena instead of the general allocator, with the whole arena bulk-freed at frame exit
instead of an individual free/`Drop`. This reuses M2's own move-tracking proof obligations rather
than needing a new analysis. Deliberately out of scope: a non-escaping *fixed-size* value doesn't
need this at all (it should just be a stack value); a value that does escape its frame falls back
to the ordinary allocator; full cross-scope/loop-iteration regions are not attempted (too easy to
silently keep a whole region alive via one small still-referenced object). Expected win is real
but narrow — cheaper on allocation-heavy hot paths, near-zero elsewhere — so this is
profiling-driven follow-up work, not a default. Depends on M2's move checker landing first; tracked
in [TODO.md](TODO.md#m4--everything-else-not-sequenced).

**Idea, not designed (`extern "rust"` — calling into Rust directly, not just C):** unlike
`extern "C"` (which has to trust an arbitrary C callee blindly — C has no aliasing/ownership
information to check against), Rust's own borrow checker enforces the same strict, static,
exactly-one-`&mut`-or-any-number-of-`&` rule Sol does (above). The hope is that an `extern "rust"`
boundary could therefore be checked, not just trusted — a Rust function's `&`/`&mut`/owned
signature carries real aliasing info Sol's checker could verify against, in a way `extern "C"`
fundamentally can't. Not designed at all yet: whether Sol's and rustc's in-memory
type/calling-convention representations line up enough to do this without a translation layer, how
a generic or trait-object Rust signature would even map to a Sol one, and whether "same rules" in
spec is actually "same rules" in the two checkers' implementations, or just similar-looking ones
that still need a trust boundary. Tracked in
[TODO.md](TODO.md#m4--everything-else-not-sequenced).

---

## 12. Control Flow

**`for` unifies Rust's `loop`/`while`/`for`** — one keyword, disambiguated by what follows it.

```sol
for { println("loop") }              // infinite loop — type is ! (never); no break-with-value

mut counter := 5
for counter <= 0 { counter -= 1 }     // while-loop: bare condition, no `in`

for el in &array { ... }             // el: &int      — like .iter()
for el in &mut array { ... }         // el: &mut int  — like .iter_mut()
for i, el in array { ... }           // el: int, indexed — like .into_iter().enumerate()
```

**`limit N` applies to every `for` form** — bare infinite, conditional, or for-each — capping
iterations. Exceeding the cap breaks out **with an error** rather than running forever, and the
whole construct becomes an **expression** evaluating to a `Res`, chainable directly:
```sol
result: Res<none, LimitExceeded> = for counter <= 0 limit 4 { 
    counter -= 1 
}

result.Err{panic("handle limit error")}
```
This also gives an otherwise-infinite `for {}` a way to become finite and typed, without needing
`break value` (which doesn't exist in Sol — `break` never carries a value). `LimitExceeded` is a
dedicated marker error type, distinct from a caller's own `str`/custom errors, so a `limit`
timeout can't be silently conflated with an ordinary application error.

**`break`/`continue` exist as plain, valueless keywords**, exactly like Rust minus
break-with-value: `break` exits the innermost `for`, `continue` skips to its next iteration.
**There are no labeled loops** — both always target the innermost enclosing `for`; a multi-level
exit needs a flag or an early `return` instead.

**Range syntax is Rust's, both forms**: `0..3` (exclusive) and `0..=3` (inclusive) — no new
spelling invented, and this is also the `Range<T>` shape `Limit<T, RANGE>` (§20) uses.

---

## 13. Error Handling

```sol
union Res<O = none, E = str> {
    Ok(O),
    #[pass]
    Err(E),
}
```
- **`.pass` is Sol's spelling of Rust's `?`.** It unwraps the non-`#[pass]` variant's payload,
  or early-returns the `#[pass]`-marked variant from the enclosing function. `#[pass]` marks
  which variant is the "propagate outward" case, generalizing beyond `Res` to any
  two-or-more-variant union with exactly one `#[pass]`-marked variant. Using `.pass` where the
  enclosing function's return type isn't compatible is a compile error, with no special-casing
  for `main`.
- **`assert` panics on failure.** `panic("message")` is directly callable too, independent of
  `assert` — a failed assert is sugar for "check the condition, panic if false." Panics unwind
  by default (running `Drop`s), with an abort mode available at build-configuration level, same
  as Rust.

So Sol has two tiers: `Res`/`Option` (`?T`) + `.pass` for recoverable, expected errors;
panics for programmer-error/unrecoverable conditions.

---

## 14. Operators and Auto-Derived Traits

- **`Eq`/`Ord` are auto-derived whenever every field/variant supports them** — structural
  equality/ordering, field-by-field or variant-by-variant, automatic rather than requiring a
  derive annotation. `#[!Eq]`/`#[!Ord]` presumably opt out.
- **Custom operator overloading dispatches through per-operator traits**, the same way `Index`
  already does: `+` through `Add`, `-` through `Sub`, `==` through `Eq`, `<`/`>`/... through `Ord`,
  and so on — one trait per operator, following the same "differ by generic parameter" rule
  already established for traits (§7), not a bespoke mechanism.

**`Ord` is not cross-variant on a multi-variant union.** Auto-derivation only produces `Ord` when
there's a single variant (so its fields have a well-defined field-by-field order) or the union's
variants are otherwise directly comparable; comparing values from two *different* variants
(`Err(_)` vs `Ok(_)`) is a compile error rather than picking an arbitrary declaration-order
ranking — there's no natural order between "this failed" and "this succeeded," so the derive
doesn't invent one.

**Open:** the full operator list and precedence/associativity table don't exist yet.

---

## 15. Concurrency: `async`/`await`

Sol deliberately combines pieces of Rust and Kotlin rather than copying either wholesale:

- **A built-in runtime, zero setup** — no executor crate to choose or configure; `async main()`
  just runs.
- **`async` stays an explicit keyword — function coloring included, deliberately.** Async
  functions can't be casually called from sync code and vice versa, matching Rust. This is a
  conscious trade: the ergonomics goal is removing *setup* and *lifetime* pain, not the
  sync/async boundary itself.
- **Structured concurrency — a spawned task's lifetime is tied to its spawning scope**, closing
  Rust's biggest ergonomic gap around spawning: no `'static`/`Arc` needed just to borrow local
  data into a spawned task, because the scope structurally can't end before its children do.

```sol
task {
    handleA := spawn { fetchUser(1).await }   // handleA: Task<str>
    handleB := spawn { fetchUser(2).await }

    userA := handleA.await   // both fetches ran concurrently
    userB := handleB.await
    println(f"{userA}, {userB}")
}
```

- **`task { }`** opens a structured scope; it doesn't complete until every `spawn` inside it has.
- **`spawn { }`** launches concurrent work and returns a `Task<T>` handle.
  - The handle **must be `.await`-ed inside the enclosing `task {}` block** (not deferred until
    after it closes).
  - **An un-awaited handle is a compile error** — the same spirit as Rust's `#[must_use]`.
  - **A panicking spawn surfaces through `.await` as `Res<T, PanicError>`**, not by crashing
    — a genuine asymmetry with directly-awaited calls, which still propagate panics normally,
    since a spawn has no synchronous call stack to unwind through.
- **Ordinary borrow-checker rules are the data-race rules** — two `spawn`s each taking `&data` is
  fine; two wanting `&mut data` hits the same aliasing violation as single-threaded code, just
  checked across `spawn` boundaries too.
- **`Send`/`Sync`, same jobs as Rust's:** moving a value into a `spawn` needs `Send`; sharing one
  by reference across multiple `spawn`s needs `Sync`. Both auto-derived-when-possible, following
  the same `#[!Trait]`/`use ... impl Trait {}` pattern as `Copy`/`AutoCopy`.

**`task.block { }` is the sync-side counterpart of `task { }`** — the one sanctioned way to cross
from sync into async code (besides `async main()` itself, which the runtime drives for you). It
opens the same kind of structured scope and drives any `spawn`s inside to completion, but unlike
`task { }` it's callable from ordinary sync functions, blocking the calling thread until
everything inside resolves and yielding the block's final expression:

```sol
main() {
    greeting := task.block { fetchGreeting().await }
    println(greeting)
}

async fetchGreeting(): str { ... }
```

Curly braces can be dropped when the block is a single statement:
```sol
task.block serve().await
task fn().await
```

- **`task.block` inside an already-async context is a compile error** — nesting a blocking wait
  inside code the async runtime is already driving is the classic deadlock footgun (Tokio panics
  on the equivalent). `task.block` is only valid at genuinely sync call sites.

**No detached/fire-and-forget spawns.** Every `spawn` must be awaited inside its enclosing
`task { }` — there's no opt-out that lets a task outlive its spawning scope, keeping the
structured-concurrency guarantee airtight. Fire-and-forget work is just its own top-level
`task { }`, not a variant of `spawn`.

**Cooperative cancellation.** A `Task<T>` handle gets a `.cancel()` that sets a flag checked at
the task's own `.await` points — the standard structured-concurrency pattern (Kotlin
coroutines/trio's model), fitting naturally since `task { }` already tracks child lifetimes.

**Channels and `select`, Rust-`mpsc`-style.** A `Channel<T>` with sender/receiver halves, plus a
`select { }` construct to race multiple `.await`-able sources — the standard complement to
structured `spawn`/`task`.

**No `actor` construct.** Ordinary structs plus the borrow checker's aliasing rules (§11) already
serialize access to mutable state; a dedicated `actor` primitive would duplicate that machinery
without a clear payoff, and it would also have reopened the function-coloring question (would
calling an actor method need `.await`?) for no clear benefit.

**Open:** the exact `Channel<T>`/`select { }` API surface (buffered vs. unbuffered, `select`'s
arm syntax) isn't designed yet, just the intent that it exists.

---

## 16. `unsafe` and Intrinsics

```sol
unsafe {
    ptr := toRaw<T>(this.buffer)
    toSlice(ptr, this.len())
}
```
`intrinsic.{...}` is a pseudo-module for compiler-provided primitives — but **`intrinsic`
functions aren't all unsafe**. Only the ones that are actually dangerous (raw-pointer
manipulation like `toRaw`/`toSlice` above) need to be called inside an `unsafe { }` block;
others (e.g. `intrinsic.typeinfo`, §19) are perfectly safe and callable directly, with no
`unsafe` wrapper needed. `unsafe { }` gates specific dangerous operations, not the `intrinsic`
namespace as a whole.

`RawPtr` is the bare-pointer representation underneath `Slice`/`Array`/`DynamicArray`'s `buffer`
field (§3.3) — an implementation detail those types wrap, not itself a surface-level array type.

---

## 17. Testing

No special `Test` trait. Testing is a file-naming + attribute convention:
- A source file named `test{Name}` (e.g. `testMath.sol`) is a test module.
- `#[test]` marks an individual test function:
  ```sol
  #[test]
  addsCorrectly() {
      assert(comptimeAdd(2, 3) == 5)
  }
  ```

**A failed `assert` is caught per-test.** Each `#[test]` fn runs in a context that catches its
panic and reports just that one test as failed — the rest of the suite still runs, matching
`cargo test`/`go test`'s behavior.

**Open:** exact `sol test` CLI behavior (flags, output format, parallelism).

---

## 18. Goul (Planned)

Not designed yet. The intended relationship: same syntax as Sol, but heap values are
automatically wrapped in reference-counted boxes, and the borrow checker either turns off,
stays on for cross-thread safety only, or some hybrid — undecided. Interop between Sol and Goul
modules in one project is also open.

---

## 19. Reflection: `any` and `TypeInfo`

Odin-style reflection: a value can be erased to a type that still knows what it actually is at
runtime, and that runtime type can be inspected in detail. Sol already had half of this in
place before it was designed — `.typeof` (§10) returning a first-class, comparable type value is
exactly Odin's `typeid`, just under a different name.

### 19.1 `any` is unsized — like Rust's `str`, you can only ever hold `&any`

**`any` can never be owned directly, the same way Rust's `str` can only exist behind a pointer
(`&str`, never a bare `str` local).** There's no implicit borrowing — the `&` is always written
explicitly, both in the type and at the call site, consistent with every other borrow in the
language:

```sol
printAny(value: &any) {
    if type v: int := value {
        println(f"int: {v}")
    } else if type v: str := value {
        println(f"str: {v}")
    } else {
        println(f"something else: {value.typeof}")
    }
}

x := 42
printAny(&x)
```

Because `any` is inherently non-owning, forcing it to only ever appear as `&any` makes that fact
visible directly in the type instead of being a hidden property of an otherwise-ordinary-looking
`any` — the same reasoning Rust applies to `str`. This also means **no special lifetime syntax
is needed for `any`** — it's just an ordinary borrow, so the usual `&'a` form already covers the
case of storing one in a struct field, with no `any<'a>`-style special case required:

```sol
struct Container<'a> {
    value: &'a any
}
```

### 19.2 Downcasting reuses `if type`

No new operator — `if type Pattern := expr` (already used for union-variant matching, §10) is
extended to accept a plain `name: Type` binding, tried against an `&any`'s runtime type:

```sol
if type v: int := value {
    // v is in scope here, only if `value` currently holds an int
}
```

Since `any` never owned the data, the bound `v` is itself a borrow of the underlying value
(`v: &int` here) — consistent with §19.1. For a type that implements `AutoCopy` (every
primitive does, §11), that borrow behaves like an implicit copy wherever one is needed, the same
as any other `AutoCopy` value would. **For a non-`AutoCopy` type (a struct, say), `v` is an
ordinary borrow, and `.copy` (§11) works on it exactly as it would on any other borrow** —
`v.copy` yields an owned duplicate, with no special-casing needed for the fact that `v` came from
an `any` downcast rather than an explicit `&`.

### 19.3 Full reflection: `intrinsic.typeinfo` and `TypeInfo`

Beyond just checking "is this an int," the full structural breakdown of a type — struct fields,
union variants, array element types, and so on, mirroring Odin's `Type_Info` union — comes from
an explicit intrinsic call rather than a chained property:

```sol
info := intrinsic.typeinfo(value.typeof)
```

**No `unsafe` needed** — `intrinsic.typeinfo` is a safe intrinsic (§16); only the intrinsics
that actually touch raw memory require `unsafe { }`.

```sol
union TypeInfo {
    Primitive(PrimitiveKind),
    Struct{ fields: []FieldInfo },
    Union{ variants: []VariantInfo },
    Enum{ backing: TypeInfo, variants: []EnumVariantInfo },
    Array{ elem: TypeInfo, len: ?uint },   // len is none for a heap []T
    Borrow{ inner: TypeInfo, mutable: bool },
}

struct FieldInfo {
    name: str
    type: TypeInfo
    offset: uint
}
```

This two-tier split — a cheap, comparable `typeid`-like value from `.typeof` itself, versus the
full structural breakdown only computed when `intrinsic.typeinfo` is actually called — mirrors
Odin's `typeid`/`Type_Info` split and means you don't pay for reflecting a type's full shape
unless you ask for it:

```sol
printFields(value: &any) {
    match intrinsic.typeinfo(value.typeof) {
        Struct(s) => for field in s.fields {
            println(f"{field.name}: {field.type}")
        }
        Primitive(p) => println(f"primitive: {p}"),
        _ => println("not a struct"),
    }
}
```

**`TypeInfo` is reflectable through `if type` just like any other union** — it isn't
special-cased or hidden from the `any` machinery it's built on top of, so
`if type v: TypeInfo := value` works the same way it would for any other union type (§10, §19.2).

**Open:** exact shape of `PrimitiveKind`/`VariantInfo`/`EnumVariantInfo`; whether field *values*
(not just names/types/offsets) are reachable generically for a struct behind an `&any` — that's
the piece that would make something like a generic serializer possible, and it's the part most
likely to run into the borrow-checker/ownership questions from §19.1 and §19.2 again, just
per-field instead of for the whole value.

---

## 20. Type Aliases: `type`, `distinct`, and `limit`

```sol
// typedef
type Byte := u8

// only allows numbers in range 1..u8.MAX
type NonNullU8 := u8 limit 1..u8.MAX

// distinct type
type Number := distinct f64

number := Number.(f64.(0))
double := f64.(number)
```

Three related but distinct forms:

**`type Name := T` — a transparent alias.** `Byte` and `u8` are fully interchangeable
everywhere — same representation, same type as far as the compiler's concerned. This is purely
for readability, like Rust's `type` or a C `typedef`.

**`type Name := distinct T` — a nominal, opaque wrapper.** `Number` and `f64` are *not*
interchangeable, even though `Number` has exactly `f64`'s representation underneath. Converting
between them requires going through construction, the same `Type.(arg)` dispatch mechanism
already used everywhere else (§6) — the compiler auto-generates the wrap (`Number.(value: f64)`)
and unwrap (`f64.(value: Number)`, an additional constructor overload on `f64` itself) the same
way it auto-generates `From`-style conversions for any other type-dispatched constructor. This
is Odin's `distinct` directly.

**`type Name := T limit RANGE` — sugar for a real generic wrapper type, `Limit<T, RANGE>`.**
`NonNullU8` is really `Limit<u8, 1..u8.MAX>` under the hood — not a separate mechanism from
`distinct`, but arriving at the same nominal-separation behavior *because* `Limit<T, RANGE>` is
a genuine wrapper struct, not a bare alias. Its constructor is declared fallible the same way any
other constructor would be (§6) — an explicit `Res<This>` return type, nothing `Limit`-specific:
```sol
struct Limit<T, RANGE> {
    value: T
    This.(value: T): Res<This> => ...   // explicit Res<This> — this constructor can fail
}
```
which is exactly why it can be called like this:
```sol
value: Res<NonNullU8> = NonNullU8.(0)     // Err — 0 is outside 1..u8.MAX
value: Res<NonNullU8> = NonNullU8.(5)     // Ok(NonNullU8(5))
```

**Construction returns a `Res` rather than panicking** — matching `for ... limit N`'s
Result-producing behavior (§12), so both uses of the `limit` keyword in the language share the
same "bounded, and the violation is recoverable" shape rather than one panicking and the other
not.

This introduces a mechanism the rest of the spec hasn't needed yet: **`RANGE` in
`Limit<T, RANGE>` is a compile-time *value*, not a type** — a const generic parameter. Up to
this point every generic parameter in the language (`List<T>`, `Result<O, E>`, ...) has been a
type. `Limit` needs its second parameter to be a range value instead, so const generics are now
a real prerequisite, not just a `Limit`-specific detail.

**Const generics use an inline `const` marker, mirroring Rust's `const N: usize`:**
```sol
struct Limit<T, const RANGE: Range<T>> {
    mut value: T
    This.(value: T): Res<This> => ...
}
```
`Range<T>` is exactly the `0..3`/`0..=3` type from §12's range syntax — so `Limit`'s const
parameter is just an ordinary value of that type, not a new kind of thing.

**Unwrapping a `Limit<T, RANGE>` back to `T` mirrors `distinct`'s own unwrap** (above) rather than
exposing a public field: `T.(value: Limit<T, RANGE>)`, the same `Type.(arg)` dispatch mechanism
used everywhere else (§6). This keeps `Limit`'s internal field private and the conversion
consistent with the pattern the doc already established for `distinct`.

**A `Limit<T, RANGE>`'s value is mutable in place, re-checked on every mutation.** A `mut
Limit<T, RANGE>` allows direct mutation of the wrapped value, but each mutation re-runs the range
check — so a plain-looking assignment can fail (or panic), the trade accepted for the ergonomics
of not having to reconstruct the wrapper on every change (e.g. in a hot loop).

**Open:** how broadly const generics are allowed beyond this one use case (arbitrary const
expressions as parameters elsewhere in the language, or just this one `Limit`-shaped need).

---

## Appendix: Open Design Questions

Collected from inline **Open:** markers above, for at-a-glance status. Most of what used to be
listed here has been resolved (see the relevant sections); what's left is deliberately deferred,
either because it's a large standalone design surface or because nothing currently depends on it:

- **Operators** — the full operator list and precedence/associativity table (the trait-dispatch
  *shape* itself is now settled, §14).
- **Concurrency** — the exact `Channel<T>`/`select { }` API surface (buffered vs. unbuffered,
  `select`'s arm syntax) — the intent that they exist is settled, §15.
- **Testing** — exact `sol test` CLI behavior (flags, output format, parallelism) — per-test
  isolation itself is settled, §17.
- **Reflection** — exact shape of `PrimitiveKind`/`VariantInfo`/`EnumVariantInfo`; whether generic
  per-field *value* access through an `&any` is possible (needed for a generic serializer) —
  deliberately left for a dedicated future session, §19.3.
- **Goul** — essentially everything (§18) — deliberately out of scope until Sol itself settles.
- **Type aliases** — how broadly const generics are allowed beyond `Limit<T, RANGE>`'s one need
  (§20); Sol's general operator-precedence table (above) also covers how `..`/`..=` interact with
  other operators.
- **Traits** — whether disambiguating an output-position generic conflict some day gets a
  turbofish-equivalent syntax, rather than always requiring an outer type annotation (§7).
