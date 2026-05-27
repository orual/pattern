// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Pattern ND - Neurodivergent Support Tools
//!
//! This crate provides ADHD-specific tools, agent personalities,
//! and cognitive support features tailored for neurodivergent users.

pub mod agents;
pub mod entities;
pub mod sleeptime;
pub mod tools;

pub use agents::{ADHDAgent, AgentPersonality};
pub use sleeptime::{SleeptimeMonitor, SleeptimeTrigger};
pub use tools::{ADHDTool, EnergyState, TaskBreakdown};

/// ADHD-specific agent types
pub mod agent_types {
    pub const PATTERN: &str = "pattern";
    pub const ENTROPY: &str = "entropy";
    pub const FLUX: &str = "flux";
    pub const ARCHIVE: &str = "archive";
    pub const MOMENTUM: &str = "momentum";
    pub const ANCHOR: &str = "anchor";
}

/// Re-export commonly used types
pub mod prelude {
    pub use crate::{
        ADHDAgent, ADHDTool, AgentPersonality, EnergyState, SleeptimeMonitor, SleeptimeTrigger,
        TaskBreakdown, agent_types,
    };
}

#[cfg(test)]
mod tests {

    #[test]
    fn it_works() {
        // Basic smoke test
        assert_eq!(2 + 2, 4);
    }
}
