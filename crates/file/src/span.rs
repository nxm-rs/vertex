#[derive(Debug, Clone)]
pub struct Span {
    value: u64,
}

impl Span {
    pub fn new(value: u64) -> Span {
        match value {
            0 => {
                panic!("invalid length for span: {}", value);
            }
            _ => Span { value },
        }
    }

    pub fn value(&self) -> u64 {
        self.value
    }

    pub fn to_bytes(&self) -> [u8; 8] {
        self.value.to_le_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_span_size() {
        let span = Span::new(4096);
        assert_eq!(u64::from_le_bytes(span.to_bytes()), 4096);
    }

    #[test]
    fn max_span_size() {
        const MAX_SPAN_SIZE: u64 = (2 ^ 32) - 1;
        let span = Span::new(MAX_SPAN_SIZE);
        assert_eq!(u64::from_le_bytes(span.to_bytes()), MAX_SPAN_SIZE);
    }

    #[test]
    fn one_span_size() {
        let span = Span::new(1);
        assert_eq!(u64::from_le_bytes(span.to_bytes()), 1);
    }

    #[test]
    #[should_panic(expected = "invalid length for span")]
    fn invalid_span() {
        let _span = Span::new(0);
    }
}
