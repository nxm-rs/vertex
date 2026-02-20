//! Proto codecs for pingpong protocol.

/// Format a pong response from a ping greeting (wraps in braces like Bee).
pub(crate) fn format_pong_response(greeting: &str) -> String {
    format!("{{{}}}", greeting)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pong_format() {
        assert_eq!(format_pong_response("ping"), "{ping}");
        assert_eq!(format_pong_response("hello"), "{hello}");
    }
}
