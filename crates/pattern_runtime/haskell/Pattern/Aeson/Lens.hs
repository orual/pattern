{-# LANGUAGE RankNTypes #-}
-- | Vendored lens-aeson — Prisms and Traversals for JSON Value.
--
-- Provides the same API as Data.Aeson.Lens but operates on our
-- vendored Pattern.Aeson.Value type (which uses [Value] for arrays
-- and Map Key Value for objects).
module Pattern.Aeson.Lens
  ( -- * Object access
    key
  , members
    -- * Array access
  , nth
  , values
    -- * Value prisms
  , _String
  , _Number
  , _Bool
  , _Array
  , _Object
  , _Int
  , _Double
  , _Null
  ) where

import Prelude
import Data.Text (Text)
import qualified Data.Map.Strict as Map
import Control.Lens (Traversal', Prism', prism')

import Pattern.Aeson.Value (Value(..), KeyMap, fromText)

-- | Access a value at a given key in a JSON object.
key :: Text -> Traversal' Value Value
key k f (Object o) = case Map.lookup (fromText k) o of
  Nothing -> pure (Object o)
  Just v  -> (\v' -> Object (Map.insert (fromText k) v' o)) <$> f v
key _ _ v = pure v

-- | Traverse all values in a JSON object.
members :: Traversal' Value Value
members f (Object o) = Object <$> traverse f o
members _ v          = pure v

-- | Access the nth element of a JSON array.
nth :: Int -> Traversal' Value Value
nth i f (Array a)
  | i >= 0 && i < length a =
      let (before, x:after) = splitAt i a
      in (\v' -> Array (before ++ [v'] ++ after)) <$> f x
nth _ _ v = pure v

-- | Traverse all values in a JSON array.
values :: Traversal' Value Value
values f (Array a) = Array <$> traverse f a
values _ v         = pure v

-- | Prism into a Text value.
_String :: Prism' Value Text
_String = prism' String $ \v -> case v of
  String s -> Just s
  _        -> Nothing

-- | Prism into a Double number.
_Number :: Prism' Value Double
_Number = prism' Number $ \v -> case v of
  Number n -> Just n
  _        -> Nothing

-- | Prism into a Bool value.
_Bool :: Prism' Value Bool
_Bool = prism' Bool $ \v -> case v of
  Bool b -> Just b
  _      -> Nothing

-- | Prism into a list of Values (JSON array).
_Array :: Prism' Value [Value]
_Array = prism' Array $ \v -> case v of
  Array a -> Just a
  _       -> Nothing

-- | Prism into a KeyMap of Values (JSON object).
_Object :: Prism' Value (KeyMap Value)
_Object = prism' Object $ \v -> case v of
  Object o -> Just o
  _        -> Nothing

-- | Prism that extracts an Int from a Number value (truncates).
_Int :: Prism' Value Int
_Int = prism' (Number . fromIntegral) $ \v -> case v of
  Number d -> Just (truncate d)
  _        -> Nothing

-- | Prism that extracts a Double from a Number value.
_Double :: Prism' Value Double
_Double = prism' Number $ \v -> case v of
  Number d -> Just d
  _        -> Nothing

-- | Prism into a Null value.
_Null :: Prism' Value ()
_Null = prism' (const Null) $ \v -> case v of
  Null -> Just ()
  _    -> Nothing
