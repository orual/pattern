-- | Vendored aeson — re-exports construction types and lens accessors.
--
-- Drop-in replacement for Data.Aeson + Data.Aeson.Lens.
module Pattern.Aeson
  ( -- * Core types (from Pattern.Aeson.Value)
    Value(..)
  , Key
  , KeyMap
  , Object
  , Array
  , Pair
    -- * Key construction
  , fromText
  , toText
    -- * Value construction
  , object
  , (.=)
  , emptyObject
  , emptyArray
    -- * ToJSON class
  , ToJSON(..)
    -- * JSON serialisation
  , encode
    -- * Lens accessors (from Pattern.Aeson.Lens)
  , key
  , members
  , nth
  , values
  , _String
  , _Number
  , _Bool
  , _Array
  , _Object
  , _Int
  , _Double
  , _Null
  ) where

import Pattern.Aeson.Value
import Pattern.Aeson.Lens
