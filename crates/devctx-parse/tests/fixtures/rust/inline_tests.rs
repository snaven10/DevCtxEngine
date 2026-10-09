pub fn mark() {}

#[cfg(test)]
mod tests {
    #[test]
    fn marks() {
        super::mark();
    }
}

#[cfg(not(test))]
pub fn shipped() {
    mark();
}

#[cfg(any(test, feature = "mock"))]
pub fn fake_client() {
    mark();
}
