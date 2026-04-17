{-# LANGUAGE GADTs #-}
-- | Pattern.Memory — persistent memory-block operations.
--
-- Variant names mirror @Pattern.sdk::requests::memory::MemoryReq@ (Rust)
-- byte-for-byte; the @FromCore@ derive on the Rust side dispatches by
-- unqualified DataCon name. The prefix on 'BlockType' / 'SchemaKind'
-- constructors (e.g. 'BlockCore', 'SchemaText') is deliberate — a bare
-- @Core@ / @Text@ / @Log@ would collide with other SDK modules'
-- constructor namespaces once this effect is imported unqualified.
module Pattern.Memory where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Opaque handle referencing a memory block (Rust side uses SmolStr).
type BlockHandle = Text

-- | Content blob associated with a memory block.
type Content = Text

-- | Search query string for recall/search operations.
type Query = Text

-- | Block classification. Mirrored by Rust @BlockTypeReq@; the
-- @Block@-prefix avoids DataCon-name collisions with other SDK modules.
data BlockType
  = BlockCore
  | BlockWorking
  | BlockArchival
  | BlockLog

-- | Schema shape (tag only; handler fills the nested defaults).
-- Mirrored by Rust @SchemaKindReq@; the @Schema@-prefix avoids
-- DataCon-name collisions.
data SchemaKind
  = SchemaText
  | SchemaMap
  | SchemaList
  | SchemaLog

-- | Memory effect algebra.
--
-- 'Write' takes an optional description — 'Nothing' leaves existing
-- metadata untouched (and falls through to a default description only
-- when auto-creating a missing block); 'Just' updates/sets it.
--
-- 'Create' explicitly creates a new block with full metadata control.
-- 'Replace' does string-replace within an existing block's text.
data Memory a where
  Read    :: BlockHandle -> Memory Content
  Write   :: BlockHandle -> Content -> Maybe Text -> Memory ()
  Create  :: BlockHandle
          -> Text              -- description
          -> BlockType         -- block type
          -> SchemaKind        -- schema kind (handler fills nested defaults)
          -> Maybe Int         -- char_limit (Nothing = runtime default)
          -> Content           -- initial content
          -> Memory ()
  Append  :: BlockHandle -> Content -> Memory ()
  Replace :: BlockHandle -> Text -> Text -> Memory ()  -- label, old, new
  Search  :: Query -> Memory [BlockHandle]
  Recall  :: BlockHandle -> Memory Content
  Archive :: BlockHandle -> Memory ()

read_ :: Member Memory effs => BlockHandle -> Eff effs Content
read_ h = send (Read h)

-- | Write content to a block. Auto-creates the block (Working, text
-- schema) if it doesn't exist. Leaves existing description metadata
-- untouched.
write :: Member Memory effs => BlockHandle -> Content -> Eff effs ()
write h c = send (Write h c Nothing)

-- | Like 'write', but also sets/updates the block's description.
writeWithDesc :: Member Memory effs => BlockHandle -> Content -> Text -> Eff effs ()
writeWithDesc h c d = send (Write h c (Just d))

-- | Create a block with full metadata control. Fails if a block with
-- the same handle already exists.
create :: Member Memory effs
       => BlockHandle -> Text -> BlockType -> SchemaKind -> Maybe Int -> Content -> Eff effs ()
create h d bt sk cl ic = send (Create h d bt sk cl ic)

append :: Member Memory effs => BlockHandle -> Content -> Eff effs ()
append h c = send (Append h c)

-- | Replace all occurrences of @old@ with @new@ in the block's rendered
-- text. Errors if the block does not exist.
replace :: Member Memory effs => BlockHandle -> Text -> Text -> Eff effs ()
replace h old new = send (Replace h old new)

search :: Member Memory effs => Query -> Eff effs [BlockHandle]
search q = send (Search q)

recall :: Member Memory effs => BlockHandle -> Eff effs Content
recall h = send (Recall h)

archive :: Member Memory effs => BlockHandle -> Eff effs ()
archive h = send (Archive h)
