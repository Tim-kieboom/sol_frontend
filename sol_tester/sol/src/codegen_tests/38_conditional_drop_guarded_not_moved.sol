// expect: 0
// Same shape as 37, untaken branch: `p` is never moved, so the shared
// guarded Drop's runtime flag is still true and it actually frees `p` —
// this is the case that used to leak silently before real per-edge drop
// state existed (elaborate_drops used to prune this same Drop outright
// just because `p` was *maybe* moved somewhere in the function, regardless
// of which path was actually taken at runtime).
consume(p: *int) {}

f(cond: bool) {
    p := new(1)
    if cond {
        consume(p)
    }
}

main(): i32 {
    f(false)
    return 0
}
