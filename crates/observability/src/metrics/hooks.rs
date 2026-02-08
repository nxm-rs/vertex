//! Metrics hooks for executing custom metrics collection code.

use std::sync::Arc;

/// Executed periodically to collect metrics.
pub trait Hook: Fn() + Send + Sync + 'static {}
impl<T: Fn() + Send + Sync + 'static> Hook for T {}

/// Builder for creating hooks.
#[derive(Clone, Default)]
pub struct HooksBuilder {
    hooks: Vec<Arc<dyn Hook>>,
}

impl std::fmt::Debug for HooksBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HooksBuilder")
            .field("hooks_count", &self.hooks.len())
            .finish()
    }
}

impl HooksBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_hook<F>(mut self, hook: F) -> Self
    where
        F: Hook,
    {
        self.hooks.push(Arc::new(hook));
        self
    }

    pub fn build(self) -> Hooks {
        Hooks { hooks: self.hooks }
    }
}

/// Collection of metrics hooks.
#[derive(Clone, Default)]
pub struct Hooks {
    hooks: Vec<Arc<dyn Hook>>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks")
            .field("hooks_count", &self.hooks.len())
            .finish()
    }
}

impl Hooks {
    pub fn builder() -> HooksBuilder {
        HooksBuilder::new()
    }

    pub fn with_hook<F>(mut self, hook: F) -> Self
    where
        F: Hook,
    {
        self.hooks.push(Arc::new(hook));
        self
    }

    pub fn execute_all(&self) {
        for hook in &self.hooks {
            hook();
        }
    }

    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}
