pub fn leaf() -> u8 {
    1
}

pub fn with_closure() -> u8 {
    let c = || leaf();
    c()
}

pub fn outer() -> u8 {
    fn inner() -> u8 {
        leaf()
    }
    inner()
}

pub fn run_closure() -> u8 {
    with_closure()
}

pub fn run_outer() -> u8 {
    outer()
}
