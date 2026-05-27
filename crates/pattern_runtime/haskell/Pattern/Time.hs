-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE GADTs #-}
-- | Pattern.Time — time-oriented agent effects.
--
-- `Now` returns the current UTC timestamp as an RFC 3339 string.
-- `NowNanos` returns epoch nanoseconds (Int) for duration arithmetic.
-- `Sleep` performs a bounded sleep (milliseconds).
module Pattern.Time
  ( -- * Effect algebra (internal)
    Time(..)
    -- * Rich newtypes
  , Instant(..)
  , Duration(..)
    -- * Smart constructors
  , now
  , nowNanos
  , sleep
    -- * Duration builders
  , nanoseconds
  , microseconds
  , milliseconds
  , seconds
  , minutes
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text, unpack)

-- | Time effect algebra.
data Time a where
  -- | Current wall-clock instant as RFC 3339 text (e.g. "2026-05-06T18:21:00Z").
  Now      :: Time Text
  -- | Current wall-clock instant as epoch nanoseconds (Int).
  NowNanos :: Time Int
  -- | Sleep for the given number of nanoseconds.
  Sleep    :: Int -> Time ()

-- | An absolute point in time. The Show instance displays the
-- human-readable formatted timestamp.
newtype Instant = Instant { instantFormatted :: Text }

instance Show Instant where
  show (Instant t) = unpack t

-- | A non-negative time span (nanoseconds).
newtype Duration = Duration { durationNanos :: Int }
  deriving Show

-- | Get the current wall-clock instant as a formatted string.
now :: Member Time effs => Eff effs Instant
now = Instant <$> send Now

-- | Get current epoch nanoseconds (for duration arithmetic).
nowNanos :: Member Time effs => Eff effs Int
nowNanos = send NowNanos

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
