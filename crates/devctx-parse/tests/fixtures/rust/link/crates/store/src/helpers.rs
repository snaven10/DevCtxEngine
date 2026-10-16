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

pub struct Value;

#[cfg(unix)]
pub fn platform() -> u8 {
    1
}

#[cfg(not(unix))]
pub fn platform() -> u8 {
    2
}

pub enum Mode {
    Fast,
}

impl Mode {
    #[allow(non_snake_case)]
    pub fn Parse() -> Mode {
        Mode::Fast
    }
}
