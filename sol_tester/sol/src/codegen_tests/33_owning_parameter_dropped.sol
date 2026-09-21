// expect: 0
consume(v: *int) {}

main(): i32 {
    n: int = 1
    num: *int = new(n)
    consume(num)
    return 0
}
