pub fn leaf2() {}

pub fn top() {
    leaf2();
}

#[cfg(test)]
mod tests {
    fn helper() {
        super::leaf2();
    }

    #[test]
    fn t() {
        helper();
    }
}
