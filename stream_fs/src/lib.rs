/// Returns a hello world greeting from the Stream File System library.
pub fn hello() -> String {
    "Hello from Stream File System!".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hello() {
        assert_eq!(hello(), "Hello from Stream File System!");
    }
}
