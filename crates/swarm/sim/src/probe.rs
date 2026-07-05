//! One-slot channel from a host future to the world driver.

use std::sync::Arc;

use parking_lot::Mutex;

/// Shared slot a host future publishes live handles through.
///
/// A whole-node host builds its handles inside the simulation; the driver
/// clones a probe into the host closure, the host publishes, and the driver
/// reads between world steps. The single-threaded scheduler parks every host
/// at an await point while the driver runs, so reads observe settled state.
pub struct Probe<T>(Arc<Mutex<Option<T>>>);

impl<T> Default for Probe<T> {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }
}

impl<T> Clone for Probe<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Probe<T> {
    /// Publish a value, replacing any previous one.
    pub fn publish(&self, value: T) {
        *self.0.lock() = Some(value);
    }

    /// The latest published value.
    pub fn get(&self) -> Option<T>
    where
        T: Clone,
    {
        self.0.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_then_get() {
        let probe = Probe::default();
        assert_eq!(probe.get(), None::<u32>);
        probe.clone().publish(7u32);
        assert_eq!(probe.get(), Some(7));
        probe.publish(9);
        assert_eq!(probe.get(), Some(9));
    }
}
