pub struct Failure;

impl From<std::io::Error> for Failure {
    fn from(_e: std::io::Error) -> Self {
        Failure
    }
}

impl From<String> for Failure {
    fn from(_s: String) -> Self {
        Failure
    }
}

impl Failure {
    pub fn name(&self) -> String {
        String::new()
    }
}
