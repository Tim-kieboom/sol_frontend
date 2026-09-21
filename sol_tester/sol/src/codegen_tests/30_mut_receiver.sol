// expect: 12
struct Counter {
    n: i32
    increment(&mut this) {
        this.n = this.n + 1
    }
    get(&this): i32 {
        return this.n
    }
}

main(): i32 {
    mut c: Counter = Counter{n: 10}
    c.increment()
    c.increment()
    return c.get()
}
