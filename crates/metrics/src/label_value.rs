//! Type-safe conversion from domain types to metric label strings.

/// Convert a value to a metric label string.
///
/// Automatically implemented for types deriving `strum::IntoStaticStr`.
pub trait LabelValue {
    fn label_value(&self) -> &'static str;
}

/// Blanket impl: any type with `strum::IntoStaticStr` gets `LabelValue` for free.
impl<T> LabelValue for T
where
    for<'a> &'a T: Into<&'static str>,
{
    #[inline]
    fn label_value(&self) -> &'static str {
        self.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoStaticStr;

    #[derive(IntoStaticStr)]
    #[strum(serialize_all = "snake_case")]
    enum TestDirection {
        Inbound,
        Outbound,
    }

    #[derive(IntoStaticStr)]
    enum TestOutcome {
        #[strum(serialize = "success")]
        Ok,
        #[strum(serialize = "failure")]
        Err,
    }

    #[test]
    fn strum_snake_case() {
        assert_eq!(TestDirection::Inbound.label_value(), "inbound");
        assert_eq!(TestDirection::Outbound.label_value(), "outbound");
    }

    #[test]
    fn strum_custom_serialize() {
        assert_eq!(TestOutcome::Ok.label_value(), "success");
        assert_eq!(TestOutcome::Err.label_value(), "failure");
    }
}
