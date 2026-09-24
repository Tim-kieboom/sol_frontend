// expect_fail
// expect_fault: a reference to a local escapes the function it doesn't outlive
struct Obj {}

lifetime(obj: &Obj): &Obj {
    return obj
}

wrapper(): &Obj {
    t := Obj{}
    p := &t
    return lifetime(p)
}

main(): i32 {
    return 0
}
