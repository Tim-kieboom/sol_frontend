// expect: 5
trait Greeter {
    greet(&this): i32
}

struct Bar {
    n: i32
}

use Bar impl Greeter greet(&this): i32 {
    return this.n
}

main(): i32 {
    b: Bar = Bar{n: 5}
    return b.greet()
}
