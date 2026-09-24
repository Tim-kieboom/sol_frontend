// expect_fail
// expect_fault: value may have already been moved out of
// `consume` takes ownership of `b.p` and frees it; reading `*b.p`
// afterwards would be a use-after-free.
struct Box2 {
    p: *i32
}

consume(p: *i32) {}

main(): i32 {
    n: i32 = 7
    p := new(n)
    b := Box2{p: p}
    consume(b.p)
    return *b.p
}
