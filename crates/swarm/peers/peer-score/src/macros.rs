//! Internal macro for generating the scoring config from the event table.

/// Generates `SwarmScoringConfig`, `SwarmScoringConfigBuilder`, and the
/// per-event `record_*` convenience methods from a declaration table mapping
/// `SwarmScoringEvent` variants to config fields.
///
/// Default weights come from [`SwarmScoringEvent::default_weight`], so the
/// event vocabulary and its defaults are declared once in `vertex-swarm-api`.
macro_rules! scoring_events {
    (
        $(
            $variant:ident $({ $($field:ident : $fty:ty),* $(,)? })? => $config_field:ident
        ),* $(,)?
        ;
        $( $extra_field:ident = $extra_default:expr ),* $(,)?
    ) => {
        /// Configuration for Swarm peer scoring weights.
        ///
        /// All weights can be customized. Positive values improve score,
        /// negative values decrease it. Use [`SwarmScoringConfigBuilder`] for
        /// ergonomic configuration.
        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
        pub struct SwarmScoringConfig {
            $( $config_field: f64, )*
            $( $extra_field: f64, )*
        }

        impl Default for SwarmScoringConfig {
            fn default() -> Self {
                Self {
                    $(
                        $config_field: SwarmScoringEvent::$variant
                            $({ $($field: Default::default()),* })?
                            .default_weight(),
                    )*
                    $( $extra_field: $extra_default, )*
                }
            }
        }

        impl SwarmScoringConfig {
            $(
                #[doc = concat!("Weight applied for [`SwarmScoringEvent::", stringify!($variant), "`].")]
                #[must_use]
                pub fn $config_field(&self) -> f64 { self.$config_field }
            )*
            $(
                #[doc = concat!("The configured `", stringify!($extra_field), "` value.")]
                #[must_use]
                pub fn $extra_field(&self) -> f64 { self.$extra_field }
            )*

            /// Get weight for a specific event type.
            #[must_use]
            pub fn weight_for(&self, event: &SwarmScoringEvent) -> f64 {
                match event {
                    $( SwarmScoringEvent::$variant $({ $($field: _),* })? => self.$config_field, )*
                }
            }
        }

        /// Builder for [`SwarmScoringConfig`] with fluent API.
        #[derive(Debug, Clone)]
        pub struct SwarmScoringConfigBuilder {
            config: SwarmScoringConfig,
        }

        impl Default for SwarmScoringConfigBuilder {
            fn default() -> Self { Self::new() }
        }

        impl SwarmScoringConfigBuilder {
            /// Create a new builder with default values.
            #[must_use]
            pub fn new() -> Self {
                Self { config: SwarmScoringConfig::default() }
            }

            /// Build the configuration.
            #[must_use]
            pub fn build(self) -> SwarmScoringConfig { self.config }

            $(
                #[doc = concat!("Set the weight for [`SwarmScoringEvent::", stringify!($variant), "`].")]
                #[must_use]
                pub fn $config_field(mut self, value: f64) -> Self {
                    self.config.$config_field = value;
                    self
                }
            )*
            $(
                #[doc = concat!("Set the `", stringify!($extra_field), "` value.")]
                #[must_use]
                pub fn $extra_field(mut self, value: f64) -> Self {
                    self.config.$extra_field = value;
                    self
                }
            )*
        }

        // Auto-generated convenience methods on SwarmPeerScore.
        paste::paste! {
            impl crate::score::SwarmPeerScore {
                $(
                    #[doc = concat!("Record a [`SwarmScoringEvent::", stringify!($variant), "`] event.")]
                    pub fn [<record_ $config_field>](&self $(, $($field: $fty),*)?) {
                        self.record_event(SwarmScoringEvent::$variant $({ $($field),* })?);
                    }
                )*
            }
        }
    };
}
