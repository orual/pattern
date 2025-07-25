//! Agent selection strategies for dynamic coordination

use std::{collections::HashMap, sync::Arc};

use super::{groups::AgentWithMembership, types::SelectionContext};
use crate::{Result, agent::Agent};

mod capability;
mod load_balancing;
mod random;

use async_trait::async_trait;
pub use capability::CapabilitySelector;
use dashmap::DashMap;
pub use load_balancing::LoadBalancingSelector;
pub use random::RandomSelector;

#[async_trait]
pub trait AgentSelector: Send + Sync {
    async fn select_agents<'a>(
        &'a self,
        agents: &'a [AgentWithMembership<Arc<dyn Agent>>],
        _context: &SelectionContext,
        config: &HashMap<String, String>,
    ) -> Result<Vec<&'a AgentWithMembership<Arc<dyn Agent>>>>;

    fn name(&self) -> &str;

    fn description(&self) -> &str;
}

/// Registry for agent selectors
pub trait SelectorRegistry: Send + Sync {
    /// Get a selector by name
    fn get(&self, name: &str) -> Option<Arc<dyn AgentSelector>>;

    /// Register a new selector
    fn register(&mut self, name: String, selector: Arc<dyn AgentSelector>);

    /// List all available selectors
    fn list(&self) -> Vec<String>;
}

/// Default implementation of SelectorRegistry
pub struct DefaultSelectorRegistry {
    selectors: Arc<DashMap<String, Arc<dyn AgentSelector>>>,
}

impl DefaultSelectorRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            selectors: Arc::new(DashMap::new()),
        };

        // Register default selectors
        registry.register("random".to_string(), Arc::new(RandomSelector));
        registry.register("capability".to_string(), Arc::new(CapabilitySelector));
        registry.register(
            "load_balancing".to_string(),
            Arc::new(LoadBalancingSelector),
        );

        registry
    }
}

impl SelectorRegistry for DefaultSelectorRegistry {
    fn get(&self, name: &str) -> Option<Arc<dyn AgentSelector>> {
        self.selectors.get(name).map(|r| r.clone())
    }

    fn register(&mut self, name: String, selector: Arc<dyn AgentSelector>) {
        self.selectors.insert(name, selector);
    }

    fn list(&self) -> Vec<String> {
        self.selectors.iter().map(|s| s.key().clone()).collect()
    }
}

impl Default for DefaultSelectorRegistry {
    fn default() -> Self {
        Self::new()
    }
}
