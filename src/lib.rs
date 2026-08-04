pub fn hello() -> &'static str {
    "rillet"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_works() {
        assert_eq!(hello(), "rillet");
    }
}
