// expect: 7
helper(): i32 {
    n: i32 = 7
    p := new(n)
    v := *p
    return v
}

main(): i32 {
    return helper()
}
