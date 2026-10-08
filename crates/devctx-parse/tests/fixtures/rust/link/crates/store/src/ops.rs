use std::fmt;

use crate::store::Store;

pub trait Named {
    fn name(&self) -> String;

    fn shout(&self) -> String {
        self.name()
    }
}

impl Store {
    pub fn save(&self) -> bool {
        true
    }
}

impl Named for Store {
    fn name(&self) -> String {
        String::from("store")
    }
}

impl fmt::Display for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "store")
    }
}
