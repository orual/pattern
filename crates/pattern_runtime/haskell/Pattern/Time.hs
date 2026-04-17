{-# LANGUAGE GADTs #-}
-- | Pattern.Time — time-oriented agent effects.
--
-- Fully implemented in Phase 3. The Rust-side `TimeHandler` dispatches
-- `Now` by reading `jiff::Timestamp::now()` (UTC, nanosecond precision,
-- narrowed to Haskell `Int`) and `Sleep` by bounded `std::thread::sleep`.
module Pattern.Time where

import Control.Monad.Freer (Eff, Member, send)

-- | Time effect algebra. Variant names are mirrored byte-for-byte by
-- @Pattern.sdk::requests::time::TimeReq@ (Rust).
data Time a where
  -- | Current wall-clock instant, in nanoseconds since the Unix epoch.
  Now   :: Time Integer
  -- | Sleep for the given number of nanoseconds. Handler enforces an
  -- upper bound; for longer waits use the scheduler effect (future).
  Sleep :: Integer -> Time ()

-- | Smart constructor for 'Now'.
now :: Member Time effs => Eff effs Integer
now = send Now

-- | Smart constructor for 'Sleep'.
sleep :: Member Time effs => Integer -> Eff effs ()
sleep ns = send (Sleep ns)
