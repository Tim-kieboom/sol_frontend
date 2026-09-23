// expect: 0
// `p` is moved into `consume` on the taken branch only; the shared
// end-of-function Drop for `p` is neither definitely-moved (pruned) nor
// definitely-not-moved (left unconditional) at that point, so it's emitted
// `guarded` — codegen checks `p`'s own runtime drop flag before freeing.
// Proves the guarded path doesn't double-free when the flag says "already
// moved."
consume(p: *int) {}

f(cond: bool) {
    p := new(1)
    if cond {
        consume(p)
    }
}

main(): i32 {
    f(true)
    return 0
}
