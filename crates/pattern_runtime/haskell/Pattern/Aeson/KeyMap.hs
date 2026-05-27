-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

-- | Vendored aeson KeyMap — thin wrapper around Data.Map.Strict.
--
-- Provides the same API shape as Data.Aeson.KeyMap but backed by
-- Data.Map.Strict (Key v) to avoid HashMap primop issues.
module Pattern.Aeson.KeyMap
  ( KeyMap
  , Key
  , fromText
  , toText
    -- * Query
  , lookup
  , member
  , size
    -- * Construction
  , empty
  , singleton
  , insert
  , delete
  , fromList
    -- * Conversion
  , toList
  , toAscList
  , keys
  , elems
    -- * Traversal
  , map
  , mapWithKey
  , foldlWithKey'
  , foldrWithKey
  , filter
  , filterWithKey
  , unionWith
  , difference
  , intersection
  ) where

import Prelude (Bool, Int, Maybe, Eq, Ord, Show, (.), ($))
import qualified Data.Map.Strict as Map
import Pattern.Aeson.Value (Key, KeyMap, fromText, toText)

lookup :: Key -> KeyMap v -> Maybe v
lookup = Map.lookup

member :: Key -> KeyMap v -> Bool
member = Map.member

size :: KeyMap v -> Int
size = Map.size

empty :: KeyMap v
empty = Map.empty

singleton :: Key -> v -> KeyMap v
singleton = Map.singleton

insert :: Key -> v -> KeyMap v -> KeyMap v
insert = Map.insert

delete :: Key -> KeyMap v -> KeyMap v
delete = Map.delete

fromList :: [(Key, v)] -> KeyMap v
fromList = Map.fromList

toList :: KeyMap v -> [(Key, v)]
toList = Map.toList

toAscList :: KeyMap v -> [(Key, v)]
toAscList = Map.toAscList

keys :: KeyMap v -> [Key]
keys = Map.keys

elems :: KeyMap v -> [v]
elems = Map.elems

map :: (a -> b) -> KeyMap a -> KeyMap b
map = Map.map

mapWithKey :: (Key -> a -> b) -> KeyMap a -> KeyMap b
mapWithKey = Map.mapWithKey

foldlWithKey' :: (a -> Key -> b -> a) -> a -> KeyMap b -> a
foldlWithKey' = Map.foldlWithKey'

foldrWithKey :: (Key -> a -> b -> b) -> b -> KeyMap a -> b
foldrWithKey = Map.foldrWithKey

filter :: (v -> Bool) -> KeyMap v -> KeyMap v
filter = Map.filter

filterWithKey :: (Key -> v -> Bool) -> KeyMap v -> KeyMap v
filterWithKey = Map.filterWithKey

unionWith :: (v -> v -> v) -> KeyMap v -> KeyMap v -> KeyMap v
unionWith = Map.unionWith

difference :: KeyMap v -> KeyMap v -> KeyMap v
difference = Map.difference

intersection :: KeyMap v -> KeyMap v -> KeyMap v
intersection = Map.intersection
