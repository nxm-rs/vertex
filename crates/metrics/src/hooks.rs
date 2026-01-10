//! Metrics hooks for executing custom metrics collection code

use std::sync::Arc;

/// A trait for metrics hooks that can be executed periodically
pub trait Hook: Fn() + Send + Sync + 'static {}
impl<T: Fn() + Send + Sync + 'static> Hook for T {}

/// A builder for creating hooks
#[derive(Debug, Clone, Default)]
pub struct HooksBuilder {
    hooks: Vec<Arc<dyn Hook>>,
}

impl HooksBuilder {
    /// Create a new hooks builder
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a hook to the builder
    pub fn with_hook<F>(mut self, hook: F) -> Self
    where
        F: Hook,
    {
        self.hooks.push(Arc::new(hook));
        self
    }

    /// Build the hooks
    pub fn build(self) -> Hooks {
        Hooks { hooks: self.hooks }
    }
}

/// Collection of metrics hooks
#[derive(Debug, Clone, Default)]
pub struct Hooks {
    hooks: Vec<Arc<dyn Hook>>,
}

impl Hooks {
    /// Create a new hooks builder
    pub fn builder() -> HooksBuilder {
        HooksBuilder::new()
    }

    /// Add a hook
    pub fn with_hook<F>(mut self, hook: F) -> Self
    where
        F: Hook,
    {
        self.hooks.push(Arc::new(hook));
        self
    }

    /// Execute all hooks
    pub fn execute_all(&self) {
        for hook in &self.hooks {
            hook();
        }
    }

    /// Returns the number of hooks
    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// Returns true if there are no hooks
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}
