{-# LANGUAGE GADTs #-}
-- | Pattern.Wake — register and unregister wake conditions.
--
-- An agent uses 'register' to ask the runtime to deliver a wake-up
-- activation when some external event occurs (a timer fires, a memory
-- block changes, a task completes, or a custom Haskell condition
-- evaluates to True). Each registration returns a 'WakeId' that can
-- later be passed to 'unregister' to cancel the wake.
--
-- The wake condition variants are typed records on the Rust mirror in
-- @crates/pattern_runtime/src/sdk/requests/wake.rs@. Constructor
-- naming follows the @Pattern.Spawn@ convention: the wire-side
-- variants of 'WakeCondition' carry a @Wake@ prefix so they don't
-- collide with the GADT constructor names ('Register', 'Unregister').
--
-- Capability gate
--
-- 'register' and 'unregister' both require
-- @CapabilityFlag::WakeConditionRegistration@ in the dispatching
-- agent's capability set. Calls without the flag fail with an
-- @EffectError@ whose message starts with @\"CapabilityDenied: \"@
-- — see @policy::CAPABILITY_DENIED_PREFIX@ on the Rust side.
--
-- All condition variants deliver activations when their trigger fires.
-- 'WakeCustom' runs a user-supplied Haskell program against a read-only
-- restricted bundle (Observe-class effects only) and pokes the mailbox
-- when the result is True.
module Pattern.Wake where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Opaque identifier returned by 'register'. Used to 'unregister'
--   the same condition later.
type WakeId = Text

-- | Reference to a memory block (label + storage id + owning agent).
--   Locally declared because the SDK keeps each module's wire types
--   self-contained — the Rust mirror has its own @WireBlockRef@
--   under @module = "Pattern.Wake"@.
data BlockRef = BlockRef
  { blockRefLabel   :: Text
  , blockRefBlockId :: Text
  , blockRefAgentId :: Text
  }

-- | Reference to a task — either to a whole 'TaskList' block (when
--   @taskEdgeItem@ is @Nothing@) or to a specific item within it
--   (when @taskEdgeItem@ is @Just itemId@). The 'WakeTaskTimeout'
--   constructor uses a 'BlockRef' (whole block) but
--   'WakeTaskDependencyResolved' requires an item-level reference;
--   block-level refs are rejected at registration with a clear
--   error.
data TaskEdgeRef = TaskEdgeRef
  { taskEdgeBlock :: Text
  , taskEdgeItem  :: Maybe Text
  }

-- | A wake condition. Each variant pairs a trigger with the data the
--   evaluator needs to fire the right wake.
--
--   The constructor names carry a @Wake@ prefix so they remain
--   distinct from any GADT constructors that might be in scope.
data WakeCondition
  -- | Fire repeatedly every @period_ms@ milliseconds. The runtime
  --   rejects sub-second periods to prevent runaway polling.
  = WakeInterval Int                 -- ^ @WakeInterval period_ms@.
  -- | Fire once after @deadline_ms@ milliseconds elapse, with
  --   @task@ as the timed-out unit. The agent reads the task on
  --   wake to decide what to do (chase, escalate, drop).
  | WakeTaskTimeout BlockRef Int     -- ^ @WakeTaskTimeout task deadline_ms@.
  -- | Fire when @block@'s rendered content changes (any author).
  --   Self-edits are filtered out by the subscriber.
  | WakeBlockChanged BlockRef        -- ^ @WakeBlockChanged block@.
  -- | Fire when @task@ transitions to @Completed@. Requires an
  --   item-level reference (@taskEdgeItem = Just _@); block-level
  --   refs are rejected.
  | WakeTaskDependencyResolved TaskEdgeRef
  -- | Fire when @program@ (a Haskell condition compiled by the
  --   runtime) returns @True@. Evaluated on a read-only restricted
  --   bundle (Observe-class effects only).
  | WakeCustom Text Text             -- ^ @WakeCustom id program@.

-- | Effect algebra.
data Wake a where
  -- | Register a condition; returns the wake id for later
  --   'unregister'. Capability-gated on
  --   @WakeConditionRegistration@.
  Register   :: WakeCondition -> Wake WakeId
  -- | Unregister by id. Returns whether the id was actually
  --   registered (@False@ for unknown ids — no error).
  Unregister :: WakeId -> Wake Bool

-- | Register a wake condition. Capability-gated.
register :: Member Wake effs => WakeCondition -> Eff effs WakeId
register cond = send (Register cond)

-- | Unregister a previously-registered wake by id.
unregister :: Member Wake effs => WakeId -> Eff effs Bool
unregister wid = send (Unregister wid)
