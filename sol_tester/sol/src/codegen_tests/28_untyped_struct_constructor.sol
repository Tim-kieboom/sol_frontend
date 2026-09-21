// expect: 5
struct Point {
    x: i32
    y: i32
}

main(): i32 {
    p := Point{x: 5, y: 9}
    return p.x
}
