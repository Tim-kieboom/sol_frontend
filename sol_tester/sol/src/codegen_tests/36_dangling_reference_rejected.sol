// expect_fail
// expect_fault: returns a reference to a local that doesn't outlive this function
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
