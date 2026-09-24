// expect: 7
// Moving `b.p` out of `b` hands its allocation to `read`, which frees it
// exactly once when its own parameter is dropped.
struct Box2 {
    p: *i32
}

read(p: *i32): i32 {
    return *p
}

main(): i32 {
    n: i32 = 7
    p := new(n)
    b := Box2{p: p}
    return read(b.p)
}
