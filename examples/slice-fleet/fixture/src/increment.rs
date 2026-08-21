pub fn increment(value: i32) -> i32 {
    value
}

#[cfg(test)]
mod tests {
    #[test]
    fn increments() {
        assert_eq!(super::increment(4), 5);
    }
}
