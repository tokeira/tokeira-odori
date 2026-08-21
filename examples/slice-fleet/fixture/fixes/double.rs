pub fn double(value: i32) -> i32 {
    value * 2
}

#[cfg(test)]
mod tests {
    #[test]
    fn doubles() {
        assert_eq!(super::double(4), 8);
    }
}
