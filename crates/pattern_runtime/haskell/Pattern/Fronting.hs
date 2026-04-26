{-# LANGUAGE GADTs #-}
-- | Pattern.Fronting — read and mutate the constellation's active fronting
-- set and routing rules.
--
-- An agent with the @FrontingControl@ capability flag can use 'current' to
-- inspect the active fronting state (returned as a JSON-encoded 'Text') and
-- 'set' / 'route' / 'clear' to update it. Mutations take effect immediately
-- in the daemon's in-memory @FrontingSet@ and are persisted by the daemon's
-- Block B wiring (T3 of Phase 5).
--
-- Capability gate
--
-- All constructors require @CapabilityFlag::FrontingControl@ in the
-- dispatching agent's capability set. Calls without the flag fail with an
-- @EffectError@ whose message starts with @\"CapabilityDenied: \"@
-- — see @policy::CAPABILITY_DENIED_PREFIX@ on the Rust side.
--
-- 'Current' response encoding
--
-- 'current' returns the fronting state as a JSON-encoded 'Text' string.
-- The JSON object has shape:
-- @{ \"active\": [PersonaId], \"fallback\": PersonaId | null, \"rules\": [RuleObject] }@
-- where each @RuleObject@ has fields @id@, @pattern_type@, @pattern_value@,
-- @target@, and @priority@. Decode with @Aeson.decode@ if structured access
-- is needed.
--
-- Constructor naming
--
-- 'MessagePattern' constructors are @Pattern@-prefixed to avoid collisions
-- with any other @Prefix@ / @Contains@ constructors that might be in scope
-- (same convention as @Pattern.Spawn@'s @Cat@ / @Flag@ prefix).
module Pattern.Fronting where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Persona identifier. A @Text@ string naming a persona in the constellation.
type PersonaId = Text

-- | A routing rule that maps a message pattern to a target persona.
--
--   Positional fields match the Rust @WireRoutingRule@ record.
data RoutingRule = RoutingRule
  { ruleId       :: Text          -- ^ Stable rule identifier.
  , rulePattern  :: MessagePattern -- ^ Match criterion.
  , ruleTarget   :: PersonaId     -- ^ Delivery target when the pattern matches.
  , rulePriority :: Int           -- ^ Higher values are evaluated first.
  }

-- | The matching criterion for a routing rule.
--
--   Constructor names carry a @Pattern@ prefix to avoid shadowing generic
--   Haskell names (@Prefix@, @Contains@) that may be in scope.
data MessagePattern
  -- | Matches when the message body /starts with/ the given string.
  = PatternPrefix Text
  -- | Matches when the message body /contains/ the given string.
  | PatternContains Text
  -- | Matches when the body contains @#\<tag\>@ at a word boundary.
  | PatternTopicTag Text
  -- | Matches when the compiled regex is found anywhere in the body.
  | PatternRegex Text

-- | Effect algebra.
data Fronting a where
  -- | Read the current fronting state; returns a JSON-encoded snapshot.
  --
  --   The JSON has shape:
  --   @{ "active": [PersonaId], "fallback": PersonaId | null, "rules": [...] }@
  --
  --   Capability-gated on @FrontingControl@.
  Current :: Fronting Text
  -- | Set the active fronting personas and optional fallback.
  --
  --   @Set personas (Just fallback)@ — specific fallback.
  --   @Set personas Nothing@ — fan-out to all active on no-match.
  --
  --   Capability-gated on @FrontingControl@.
  Set     :: [PersonaId] -> Maybe PersonaId -> Fronting ()
  -- | Replace the routing rules. Invalid regex patterns are rejected; the
  --   existing rules are left unchanged on compile failure.
  --
  --   Capability-gated on @FrontingControl@.
  Route   :: [RoutingRule] -> Fronting ()
  -- | Clear the fronting set entirely (active personas, fallback, rules).
  --
  --   Capability-gated on @FrontingControl@.
  Clear   :: Fronting ()

-- | Read the current fronting state as a JSON-encoded 'Text' snapshot.
--   Capability-gated on @FrontingControl@.
current :: Member Fronting effs => Eff effs Text
current = send Current

-- | Set the active fronting personas and optional fallback persona.
--   Capability-gated on @FrontingControl@.
set :: Member Fronting effs => [PersonaId] -> Maybe PersonaId -> Eff effs ()
set personas fb = send (Set personas fb)

-- | Replace the routing rules.
--   Capability-gated on @FrontingControl@.
route :: Member Fronting effs => [RoutingRule] -> Eff effs ()
route rules = send (Route rules)

-- | Clear the fronting set entirely.
--   Capability-gated on @FrontingControl@.
clear :: Member Fronting effs => Eff effs ()
clear = send Clear
