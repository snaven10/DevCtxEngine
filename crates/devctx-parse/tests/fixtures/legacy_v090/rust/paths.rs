mod store;
mod util;

use store::Store;

pub struct Svc;

impl Svc {
    pub fn new() -> Self {
        Svc
    }

    pub fn by_self() -> Self {
        Self::new()
    }
}

pub fn mod_crate() {
    crate::util::helper();
}

pub fn type_used(p: &str) -> Store {
    Store::open(p)
}

pub fn turbofish() -> Vec<u8> {
    Vec::<u8>::new()
}

pub struct Cached<T> {
    value: T,
}

impl<T: Clone> Cached<T> {
    pub fn touch(&self) -> T {
        self.read()
    }

    fn read(&self) -> T {
        self.value.clone()
    }
}

pub struct Guard<'a> {
    name: &'a str,
}

impl<'a> Drop for Guard<'a> {
    fn drop(&mut self) {
        self.release();
    }
}

impl Guard<'_> {
    fn release(&self) {
        crate::util::helper();
    }
}

pub fn typed(store: Store) -> Store {
    let other: Store = store.reopen();
    other.reopen()
}
