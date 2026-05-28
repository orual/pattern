// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Hook event lifecycle system.
//!
//! Open string-tag dispatch — adding a hook point is non-breaking.
//! See `tags` for the catalog of well-known events emitted by Pattern.

pub mod bus;
pub mod cc_aliases;
pub mod event;
pub mod filter;
pub mod gate;
pub mod payload_value;
pub mod payloads;
pub mod tags;

pub use event::{HookEvent, HookEventMetadata, HookResponse, HookSemantics};
pub use bus::{BlockingDelivery, HookBus, SubscriptionId};
pub use filter::{HookFilter, HookFilterError};
pub use gate::{GateDecision, GateKind, GateRequest, GateResponse};
pub use payload_value::{HookPayload, PayloadValue};
