{-# LANGUAGE GADTs #-}
-- | Pattern.Time — time-oriented agent effects.
--
-- Fully implemented in Phase 3. The Rust-side `TimeHandler` dispatches
-- `Now` by reading `jiff::Timestamp::now()` (UTC, nanosecond precision,
-- narrowed to Haskell `Int`) and `Sleep` by bounded `std::thread::sleep`.
--
-- Agent programs interact with the rich 'Instant' and 'Duration' newtypes
-- via the smart constructors below; the raw 'Int' wire format is an
-- internal detail of the freer-simple effect algebra.
module Pattern.Time
  ( -- * Effect algebra (internal)
    Time(..)
    -- * Rich newtypes
  , Instant(..)
  , Duration(..)
    -- * Smart constructors
  , now
  , sleep
    -- * Duration builders
  , nanoseconds
  , microseconds
  , milliseconds
  , seconds
  , minutes
    -- * Instant/Duration arithmetic
  , addDuration
  , diffInstant
  ) where

import Control.Monad.Freer (Eff, Member, send)

-- | Time effect algebra. Variant names are mirrored byte-for-byte by
-- @Pattern.sdk::requests::time::TimeReq@ (Rust).
--
-- NOTE: We use 'Int' (machine-width, 64-bit) rather than 'Integer'
-- (arbitrary-precision) because (a) the Rust handler returns @i64@, and
-- (b) GHC's 'Integer' type has multiple internal constructors (IS\/IP\/IN)
-- that the tidepool JIT codegen does not yet support. 'Int' fits epoch
-- nanoseconds until approximately year 2262.
data Time a where
  -- | Current wall-clock instant, in nanoseconds since the Unix epoch.
  Now   :: Time Int
  -- | Sleep for the given number of nanoseconds. Handler enforces an
  -- upper bound; for longer waits use the scheduler effect (future).
  Sleep :: Int -> Time ()

-- | An absolute point in time (epoch nanoseconds). Agent-facing wrapper
-- around the raw 'Int' wire format.
newtype Instant = Instant { instantNanos :: Int }

-- | A non-negative time span (nanoseconds). Agent-facing wrapper.
newtype Duration = Duration { durationNanos :: Int }

-- | Get the current wall-clock instant.
now :: Member Time effs => Eff effs Instant
now = Instant <$> send Now

-- | Sleep for the given duration.
sleep :: Member Time effs => Duration -> Eff effs ()
sleep (Duration ns) = send (Sleep ns)

-- | Build a 'Duration' from nanoseconds.
nanoseconds :: Int -> Duration
nanoseconds = Duration

-- | Build a 'Duration' from microseconds.
microseconds :: Int -> Duration
microseconds n = Duration (n * 1000)

-- | Build a 'Duration' from milliseconds.
milliseconds :: Int -> Duration
milliseconds n = Duration (n * 1000000)

-- | Build a 'Duration' from seconds.
seconds :: Int -> Duration
seconds n = Duration (n * 1000000000)

-- | Build a 'Duration' from minutes.
minutes :: Int -> Duration
minutes n = Duration (n * 60 * 1000000000)

-- | Add a 'Duration' to an 'Instant'.
addDuration :: Instant -> Duration -> Instant
addDuration (Instant a) (Duration b) = Instant (a + b)

-- | Compute the 'Duration' between two 'Instant's.
diffInstant :: Instant -> Instant -> Duration
diffInstant (Instant a) (Instant b) = Duration (a - b)
