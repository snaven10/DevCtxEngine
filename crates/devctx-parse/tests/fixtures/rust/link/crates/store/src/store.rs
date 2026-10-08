use std::collections::HashMap;

use crate::helpers::Logger;

pub struct Store {
    pub items: HashMap<String, u32>,
    pub log: Logger,
}

impl Store {
    pub fn open(path: &str) -> Result<Self, String> {
        if path.is_empty() {
            return Err(String::new());
        }
        Ok(Self::new())
    }

    pub fn new() -> Self {
        Self::helper();
        Store {
            items: HashMap::new(),
            log: Logger,
        }
    }

    fn helper() {}

    pub fn get(&self, key: &str) -> Option<&u32> {
        self.log.info();
        self.items.get(key)
    }
}
