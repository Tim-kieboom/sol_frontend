# Sol

Sol is a systems language with a Rust-style borrow checker and a terser, more
expression-oriented surface syntax — one keyword doing the job Rust splits across several,
match-chains instead of verbose `match` blocks, and structured concurrency with a built-in
runtime, among other things.

Its planned companion, **Gol**, is a transpile target that swaps static borrow checking for
ARC-style reference counting (à la Swift) — same language, two backends. Gol isn't designed yet.

## Status

**Sol doesn't compile or run real programs yet.** The pipeline (lexer → parser → AST →
name/typecheck → MIR → LLVM IR → `.exe`) works end-to-end for a concrete, non-generic subset —
ints, structs, non-generic traits, no borrow checking. Move/borrow checking itself is **in
progress**, not shipped: it's runnable against test files today, but not yet wired through the
full pipeline. Generics, unions, `Res`/`.pass`, and `?T` aren't implemented yet either.

Treat every ownership/borrowing claim above as the design intent, not a guarantee about what the
compiler enforces today.

For exactly what's done, what's in progress, and how to build/test the compiler, see
[TODO.md](TODO.md) — it's the live source of truth and is updated far more often than this file.

## The language

Sol borrows its safety model from Rust and its ergonomics goals from Kotlin and Swift, but it
isn't a clone of any of them — it's a deliberate combination, aimed at keeping strict static
guarantees while cutting down on the ceremony usually needed to get them.

**Three punctuation marks, three kinds of binding.** `::` declares an associated constant or global static, `:=`
declares an immutable local with its type inferred, and `mut` opts a binding into mutability —
immutable-by-default, with nothing extra needed for the common case.

```sol
CONST :: 1        // associated constant or global static
immut := 1        // immutable local
mut mutable := 1  // mutable local
```

**Any type can grow new methods, from anywhere.** `Type.method(...)` extends *any* type —
including primitives — without a `use` block or an `extension` keyword. The first parameter
doesn't even have to be called `this`; when it isn't, the same declaration becomes a namespaced
function instead of an instance method, still reached through the same `Type.name(...)` syntax.

```sol
// called as 5.add(3), an instance method
int.add(this, a: int): int => this + a   

// called as int.parse("42"), a namespaced function
int.parse(value: &str): Res<int> { ... }  
```

**One construction syntax, dispatched by shape.** `Type.(...)` builds a value, and which
constructor runs is decided by arity and argument type rather than by name — no ad-hoc
overloading anywhere else in the language, but this one construction site gets it. No args means
the default constructor; one arg dispatches on that argument's type (the same mechanism behind
built-in conversions like `f32.(2)`); an array-literal argument routes to its own per-element-type
constructor.

```sol
struct Number {
    mut inner: f64
    // Number.()          → the default constructor
    This.() => This{inner: 0.0}                   
    // Number.(f64.(2))   → dispatches on the f64 arg
    This.(float: f64) => This{inner: float}     
    // Number.(int.(1))   → dispatches on the int arg
    This.(num: int) => This{inner: f64.(num)}   
}

struct IntArray {
    array: []int
    // IntArray.[1, 2, 3] → array-literal constructor
    This.[int](array) => This{array: []int.(array)}            
}

main() {
    mut num := Number.()
    num = Number.(1_int)
    num = Number.(1_f64)

    arr := IntArray.[1, 2, 3]
}
```

**A strong, static type system, with room to narrow further.** Beyond the usual primitives and
generics, `type` gives you transparent aliases, `distinct` wraps a type in a nominal identity that
can't be silently mixed up with its underlying representation even though it's laid out
identically, and `limit` narrows a type down to a validated range, enforced at construction time.

```sol
// transparent alias — Byte and u8 are interchangeable
type Byte := u8                        

// nominal wrapper — UserId and u64 are NOT interchangeable
type UserId := distinct u64            

// constructing an out-of-range Percent returns Err
type Percent := u8 limit 0..=100       
```

**Ownership without the boilerplate.** Values are move-by-default, and the compiler enforces
Rust's aliasing rule statically — exactly one mutable borrow or any number of shared borrows,
never both, checked entirely at compile time with no runtime cost. Lifetimes are elided almost
everywhere; the explicit `'a` syntax only shows up in the rare cases that actually need it, like
a struct holding a borrow.

```sol
// runs automatically at scope-exit
impl Drop drop(&mut this) { free(this.buffer) }   

// explicit, independent duplicate — a keyword, not a method call
b := a.copy   
```

**Expression-oriented, and terser about it than Rust.** There's no `fn` keyword — a function is
just its name and parameter list. `if`, `match`, and blocks are all expressions whose last line is
their value. One `for` keyword covers Rust's `loop`/`while`/`for`, disambiguated by what follows
it, and it can be capped with `limit N` to turn an otherwise-infinite loop into a fallible,
typed expression instead of hanging forever.

```sol
// no `fn`, no `return`
add(a: int, b: int): int => a + b            

// if/else is an expression
x := if cond { 
    1 
} else { 
    2 
}                

// one `for`, disambiguated by what follows
for el in &mut array { ... }    // foreach loop
for condition { ... }           // while loop
for { ... }                     // infinite loop (while true)

// capped loop → Res<none, LimitExceeded>
result := for counter <= 0 limit 4 { 
    counter -= 1 
}

result.Err{err => panic(f"handle error: {err}")}
```

**Unions and match-chains instead of verbose `match` blocks.** Unions (Sol's sum types) support a
`.Variant{ body }` chain syntax for the common case — pick apart the variants you care about,
anything unhandled passes through automatically — alongside a `->Variant{}` map form and Rust's
full `match` for when you need real exhaustiveness or multi-value patterns.

```sol
union Literal {
    None,
    Int(int),
    Str{tag: str, value: str},
}

Literal.variantName(&this): &'static str {
    // exhaustive here, but chains never require it — unhandled variants pass through
    variant
        .None{"none"}
        .Int{"int"}
        .Str{"str"} 
}      
```

**Errors are just a union, with `.pass` standing in for Rust's `?`.** `Res<O, E>` is an ordinary
two-variant union, and `#[pass]` marks which variant means "propagate this outward" — a pattern
that generalizes to any similarly-shaped union, not something hardcoded to `Res`. Panics exist
too, but strictly for programmer errors, not expected failure.

```sol
union Res<O = none, E = str> {
    Ok(O),
    #[pass]
    Err(E),
}

parseAndDouble(input: &str): Res<int> {
    // early-returns Err, or unwraps Ok
    value := int.parse(input).pass   
    Ok(value * 2)
}
```

**Structured concurrency, batteries included.** `async`/`await` ships with a built-in runtime — no
executor crate to pick or wire up — and a spawned task's lifetime is tied to the scope that
spawned it, so borrowing local data into concurrent work doesn't require `'static` or an `Arc`
the way it does in Rust today.

```sol
task {
    // both fetches run concurrently
    handleA := spawn { fetchUser(1).await }   
    handleB := spawn { fetchUser(2).await }

    println(f"{handleA.await}, {handleB.await}")
}
```

**Odin-style reflection**, `unsafe`-gated raw-pointer intrinsics for the rare moments you need to
drop below the safe abstractions, and a small, orthogonal set of derivable traits (`Copy`, `Eq`,
`Ord`, `Send`/`Sync`, ...) round out the rest.

```sol
printAny(value: &any) {
    if type v: int := value { println(f"int: {v}") }
    else { println(f"something else: {value.typeof}") }
}
```

None of this is exhaustive — modules and visibility, generics, structs and traits, the full array
type family, type aliases, and every open design question still being worked through are all in
[sol-lang.md](sol-lang.md), which is both the reference and the best way to actually learn the
language.
