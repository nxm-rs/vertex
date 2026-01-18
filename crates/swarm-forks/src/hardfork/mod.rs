mod macros;

mod swarm;
pub use swarm::SwarmHardfork;

mod dev;
pub use dev::DEV_HARDFORKS;

use alloc::boxed::Box;
use core::{
    any::Any,
    hash::{Hash, Hasher},
};
use dyn_clone::DynClone;

/// Generic hardfork trait.
///
/// This trait is implemented by all hardfork types and provides a common
/// interface for working with hardforks.
#[auto_impl::auto_impl(&, Box)]
pub trait Hardfork: Any + DynClone + Send + Sync + 'static {
    /// Returns the fork name.
    fn name(&self) -> &'static str;

    /// Returns boxed value.
    fn boxed(&self) -> Box<dyn Hardfork + '_> {
        Box::new(self)
    }
}

dyn_clone::clone_trait_object!(Hardfork);

impl core::fmt::Debug for dyn Hardfork + 'static {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct(self.name()).finish()
    }
}

impl PartialEq for dyn Hardfork + 'static {
    fn eq(&self, other: &Self) -> bool {
        self.name() == other.name()
    }
}

impl Eq for dyn Hardfork + 'static {}

impl Hash for dyn Hardfork + 'static {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name().hash(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn check_hardfork_from_str() {
        let hardfork_str = ["aCcOrD"];
        let expected_hardforks = [SwarmHardfork::Accord];

        let hardforks: Vec<SwarmHardfork> = hardfork_str
            .iter()
            .map(|h| SwarmHardfork::from_str(h).unwrap())
            .collect();

        assert_eq!(hardforks, expected_hardforks);
    }

    #[test]
    fn check_nonexistent_hardfork_from_str() {
        assert!(SwarmHardfork::from_str("not a hardfork").is_err());
    }
}
