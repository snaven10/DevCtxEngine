use std::collections::HashMap;
use crate::store::{self, Store as S, model::*};

pub const LIMIT: usize = 8;
static NAME: &str = "c";
pub type Map = HashMap<String, u32>;

/// A value with a hit counter.
pub struct Cached<T> {
    pub inner: T,
    hits: u32,
}

pub trait Named: Clone + Send {
    fn name(&self) -> String;
}

impl<T: Clone> Cached<T> {
    pub fn new(
        inner: T,
    ) -> Self {
        Self::build(inner)
    }

    fn build(inner: T) -> Cached<T> {
        Cached { inner, hits: 0 }
    }
}

impl<T: Clone> Named for Cached<T> {
    fn name(&self) -> String {
        Foo::bar();
        store::open();
        String::new()
    }
}
