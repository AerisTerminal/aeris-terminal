pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_adds() {
        assert_eq!(super::add(2, 2), 5);
    }
}
