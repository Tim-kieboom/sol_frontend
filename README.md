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

**Ownership without the boilerplate.** Values are move-by-default, and the compiler enforces
Rust's aliasing rule statically — exactly one mutable borrow or any number of shared borrows,
never both, checked entirely at compile time with no runtime cost. Lifetimes are elided almost
everywhere; the explicit `'a` syntax only shows up in the rare cases that actually need it, like
a struct holding a borrow.

```sol
impl Drop drop(&mut this) { free(this.buffer) }   // runs automatically at scope-exit

b := a.copy   // explicit, independent duplicate — a keyword, not a method call
```

**Expression-oriented, and terser about it than Rust.** There's no `fn` keyword — a function is
just its name and parameter list. `if`, `match`, and blocks are all expressions whose last line is
their value. One `for` keyword covers Rust's `loop`/`while`/`for`, disambiguated by what follows
it, and it can be capped with `limit N` to turn an otherwise-infinite loop into a fallible,
typed expression instead of hanging forever.

```sol
add(a: int, b: int): int => a + b            // no `fn`, no `return`

x := if cond { 1 } else { 2 }                // if/else is an expression

for el in &mut array { ... }                 // one `for`, disambiguated by what follows

result := for counter <= 0 limit 4 { counter -= 1 }   // capped loop → Res<none, LimitExceeded>
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

variant
    .None{"none"}
    .Int{"int"}
    .Str{"str"}       // exhaustive here, but chains never require it — unhandled variants pass through
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
    value := int.parse(input).pass   // early-returns Err, or unwraps Ok
    Ok(value * 2)
}
```

**Structured concurrency, batteries included.** `async`/`await` ships with a built-in runtime — no
executor crate to pick or wire up — and a spawned task's lifetime is tied to the scope that
spawned it, so borrowing local data into concurrent work doesn't require `'static` or an `Arc`
the way it does in Rust today.

```sol
task {
    handleA := spawn { fetchUser(1).await }   // both fetches run concurrently
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
