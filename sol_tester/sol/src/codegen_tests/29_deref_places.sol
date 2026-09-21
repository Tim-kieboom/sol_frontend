// expect: 14
struct Point {
    x: i32
}

main(): i32 {
    mut x: i32 = 5
    p := &mut x
    *p = 9
    a := *p

    point: Point = Point{x: 5}
    r := &point
    b := r.x

    return a + b
}
