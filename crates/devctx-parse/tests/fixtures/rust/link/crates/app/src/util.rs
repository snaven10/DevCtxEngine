pub fn tidy() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidies() {
        tidy();
    }
}

pub fn polish() {}

#[cfg(test)]
mod deep {
    use super::super::*;
}
