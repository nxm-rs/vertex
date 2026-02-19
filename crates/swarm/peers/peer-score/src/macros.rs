//! Internal macro for generating scoring events and config.

/// Generates [`SwarmScoringEvent`], [`SwarmScoringConfig`], and
/// [`SwarmScoringConfigBuilder`] from a single declaration table.
///
/// Each entry maps an event variant (with optional fields) to a config
/// field name and its default weight. Extra config fields (e.g. thresholds)
/// are declared after a `;` separator.
///
/// The macro produces:
/// - The event enum with doc comments and variant fields
/// - `default_weight()` on the event
/// - The config struct with one `f64` field per event, plus extra fields
/// - `Default` impl using the declared defaults
/// - A getter for every field
/// - `weight_for()` dispatching events to their config field
/// - A builder struct with a fluent setter per field
macro_rules! scoring_events {
    (
        $(
            $(#[doc = $doc:expr])*
            $variant:ident $({ $($field:ident : $fty:ty),* $(,)? })?
                => $config_field:ident = $default:expr
        ),* $(,)?
        ;
        $( $extra_field:ident = $extra_default:expr ),* $(,)?
    ) => {
        /// Swarm-specific peer scoring events.
        #[derive(Debug)]
        pub enum SwarmScoringEvent {
            $(
                $(#[doc = $doc])*
                $variant $({ $($field: $fty),* })?,
            )*
        }

        impl SwarmScoringEvent {
            /// Get the default weight for this event.
            ///
            /// Positive weights improve score, negative weights decrease it.
            /// These are default values; use [`SwarmScoringConfig`] for customization.
            pub fn default_weight(&self) -> f64 {
                match self {
                    $( Self::$variant $({ $($field: _),* })? => $default, )*
                }
            }
        }

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
                    $( $config_field: $default, )*
                    $( $extra_field: $extra_default, )*
                }
            }
        }

        impl SwarmScoringConfig {
            $(
                #[must_use]
                pub fn $config_field(&self) -> f64 { self.$config_field }
            )*
            $(
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
                #[must_use]
                pub fn $config_field(mut self, value: f64) -> Self {
                    self.config.$config_field = value;
                    self
                }
            )*
            $(
                #[must_use]
                pub fn $extra_field(mut self, value: f64) -> Self {
                    self.config.$extra_field = value;
                    self
                }
            )*
        }
    };
}
