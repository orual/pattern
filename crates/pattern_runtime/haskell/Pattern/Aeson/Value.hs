{-# LANGUAGE OverloadedStrings #-}
-- | Vendored aeson Value type with construction.
--
-- This module provides the core JSON Value type and construction helpers.
--
-- Differences from upstream aeson:
--   - Array uses [Value] instead of V.Vector Value (avoids Array# primop)
--   - KeyMap uses Data.Map.Strict instead of HashMap (avoids hash primops)
module Pattern.Aeson.Value
  ( -- * Core types
    Value(..)
  , Key(..)
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
  ) where

import Prelude
import Data.Text (Text)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set

-- | A JSON key — thin wrapper around Text.
newtype Key = Key Text
  deriving (Eq, Ord, Show)

-- | Convert Text to a Key.
fromText :: Text -> Key
fromText = Key

-- | Convert a Key back to Text.
toText :: Key -> Text
toText (Key t) = t

-- | KeyMap backed by Data.Map.Strict (avoids HashMap primop issues).
type KeyMap v = Map.Map Key v

-- | A JSON object.
type Object = KeyMap Value

-- | A JSON array (uses list instead of Vector to avoid Array# primops).
type Array = [Value]

-- | A key-value pair for building objects.
type Pair = (Key, Value)

-- | A JSON value.
data Value
  = Object !Object
  | Array Array
  | String !Text
  | Number !Double
  | Bool !Bool
  | Null
  deriving (Eq, Ord, Show)

-- | Construct a JSON object from key-value pairs.
object :: [Pair] -> Value
object = Object . Map.fromList

-- | Pair a text key with a JSON-encodable value.
(.=) :: ToJSON v => Text -> v -> Pair
k .= v = (Key k, toJSON v)
infixr 8 .=

-- | Empty JSON object.
emptyObject :: Value
emptyObject = Object Map.empty

-- | Empty JSON array.
emptyArray :: Value
emptyArray = Array []

-- | A class for types that can be converted to JSON Value.
class ToJSON a where
  toJSON :: a -> Value

instance ToJSON Value where
  toJSON = id

instance ToJSON Text where
  toJSON = String

instance ToJSON Int where
  toJSON n = Number (fromIntegral n)

instance ToJSON Double where
  toJSON = Number

instance ToJSON Float where
  toJSON = Number . realToFrac

instance ToJSON Bool where
  toJSON = Bool

instance {-# OVERLAPPABLE #-} ToJSON a => ToJSON [a] where
  toJSON = Array . map toJSON

instance {-# OVERLAPPING #-} ToJSON [Char] where
  toJSON cs = String (T.pack cs)

instance ToJSON a => ToJSON (Maybe a) where
  toJSON Nothing  = Null
  toJSON (Just a) = toJSON a

instance ToJSON () where
  toJSON () = Null

instance ToJSON Integer where
  toJSON n = Number (fromIntegral n)

instance ToJSON Word where
  toJSON n = Number (fromIntegral n)

instance ToJSON Char where
  toJSON c = String (T.singleton c)

instance ToJSON Ordering where
  toJSON LT = String "LT"
  toJSON EQ = String "EQ"
  toJSON GT = String "GT"

instance (ToJSON a, ToJSON b) => ToJSON (Either a b) where
  toJSON (Left a)  = Object (Map.singleton (Key "Left") (toJSON a))
  toJSON (Right b) = Object (Map.singleton (Key "Right") (toJSON b))

instance (ToJSON a, ToJSON b) => ToJSON (a, b) where
  toJSON (a, b) = Array [toJSON a, toJSON b]

instance (ToJSON a, ToJSON b, ToJSON c) => ToJSON (a, b, c) where
  toJSON (a, b, c) = Array [toJSON a, toJSON b, toJSON c]

instance (ToJSON a, ToJSON b, ToJSON c, ToJSON d) => ToJSON (a, b, c, d) where
  toJSON (a, b, c, d) = Array [toJSON a, toJSON b, toJSON c, toJSON d]

instance (ToJSON a, ToJSON b, ToJSON c, ToJSON d, ToJSON e) => ToJSON (a, b, c, d, e) where
  toJSON (a, b, c, d, e) = Array [toJSON a, toJSON b, toJSON c, toJSON d, toJSON e]

instance ToJSON a => ToJSON (Map.Map Text a) where
  toJSON m = Object (Map.mapKeys Key (Map.map toJSON m))

instance ToJSON a => ToJSON (Set.Set a) where
  toJSON = Array . map toJSON . Set.toList

