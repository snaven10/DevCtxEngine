pub struct Logger;

impl Logger {
    pub fn info(&self) {}
}

pub struct Svc;

impl Svc {
    pub fn run(&self) {}
}

pub fn make() -> Svc {
    Svc
}

pub fn go() {}

pub fn spawn() {}
