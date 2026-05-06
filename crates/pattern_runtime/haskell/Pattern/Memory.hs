{-# LANGUAGE GADTs #-}
-- | Pattern.Memory — persistent memory-block operations.
--
-- Memory uses @Get@/@Put@ rather than @Read@/@Write@ to avoid Haskell-level
-- name collisions with 'Pattern.File' — agents can freely
-- @import Pattern.Prelude@ or mix the two effects without qualification.
-- 'BlockType' and 'SchemaKind' constructors keep their @Block@- / @Schema@-
-- prefix for the same reason (would otherwise clash with other SDK
-- modules' own @Core@ / @Text@ / @Log@ tags).
module Pattern.Memory where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Opaque handle referencing a memory block (runtime uses SmolStr).
type BlockHandle = Text

-- | Content blob associated with a memory block.
type Content = Text

-- | Search query string for recall/search operations.
type Query = Text

-- | Block classification. The @Block@-prefix avoids DataCon-name
-- collisions with other SDK modules.
data BlockType
  = BlockCore
  | BlockWorking
  | BlockArchival
  | BlockLog

-- | Schema shape (tag only; the runtime fills the nested defaults).
-- The @Schema@-prefix avoids DataCon-name collisions with other SDK
-- modules.
data SchemaKind
  = SchemaText
  | SchemaMap
  | SchemaList
  | SchemaLog

-- | Agent identifier (for shared-block access across agents).
type Owner = Text

-- | Memory effect algebra.
--
-- 'Put' takes an optional description — 'Nothing' leaves existing
-- metadata untouched (and falls through to a default description only
-- when auto-creating a missing block); 'Just' updates/sets it.
--
-- 'Create' explicitly creates a new block with full metadata control.
-- 'Replace' does string-replace within an existing block's text.
--
-- 'GetShared' retrieves a block owned by another agent that has been
-- shared with the caller. Permission is checked by the handler against
-- the shared_blocks table.
data Memory a where
  Get       :: BlockHandle -> Memory Content
  Put       :: BlockHandle -> Content -> Maybe Text -> Memory ()
  Create    :: BlockHandle
            -> Text              -- description
            -> BlockType         -- block type
            -> SchemaKind        -- schema kind (handler fills nested defaults)
            -> Maybe Int         -- char_limit (Nothing = runtime default)
            -> Content           -- initial content
            -> Memory ()
  Append    :: BlockHandle -> Content -> Memory ()
  Replace   :: BlockHandle -> Text -> Text -> Memory ()  -- label, old, new
  Search    :: Query -> Memory [BlockHandle]
  Recall    :: BlockHandle -> Memory Content
  GetShared      :: Owner -> BlockHandle -> Memory Content
  WriteToPersona :: BlockHandle -> Content -> Memory ()
  -- | Toggle pinned status of a working block.
  Pin        :: BlockHandle -> Memory ()
  Unpin      :: BlockHandle -> Memory ()
  -- | Get the schema kind of a block as a string ("text", "map", "list", "log", "composite").
  GetSchema  :: BlockHandle -> Memory Text
  -- | Get a field from a Map-schema block. Returns JSON-encoded value.
  GetField   :: BlockHandle -> Text -> Memory (Maybe Text)
  -- | Set a field in a Map-schema block. Value is JSON-encoded.
  SetField   :: BlockHandle -> Text -> Text -> Memory ()
  -- | Update the description of an existing block.
  UpdateDesc :: BlockHandle -> Text -> Memory ()
  Delete :: BlockHandle -> Memory ()

-- | Fetch a block's rendered content by label.
get :: Member Memory effs => BlockHandle -> Eff effs Content
get h = send (Get h)

-- | Put content into a block. Auto-creates the block (Working, text
-- schema) if it doesn't exist. Leaves existing description metadata
-- untouched.
put :: Member Memory effs => BlockHandle -> Content -> Eff effs ()
put h c = send (Put h c Nothing)

-- | Like 'put', but also sets/updates the block's description.
putWithDesc :: Member Memory effs => BlockHandle -> Content -> Text -> Eff effs ()
putWithDesc h c d = send (Put h c (Just d))

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

-- | Fetch a shared block's content by owner agent id and label.
-- Errors if the block hasn't been shared with the caller.
getShared :: Member Memory effs => Owner -> BlockHandle -> Eff effs Content
getShared o h = send (GetShared o h)

-- | Pin a working block so it surfaces every turn.
pin :: Member Memory effs => BlockHandle -> Eff effs ()
pin h = send (Pin h)

-- | Unpin a working block.
unpin :: Member Memory effs => BlockHandle -> Eff effs ()
unpin h = send (Unpin h)

-- | Get the schema kind of a block.
getSchema :: Member Memory effs => BlockHandle -> Eff effs Text
getSchema h = send (GetSchema h)

-- | Get a field from a Map-schema block (returns JSON value or Nothing).
getField :: Member Memory effs => BlockHandle -> Text -> Eff effs (Maybe Text)
getField h f = send (GetField h f)

-- | Set a field in a Map-schema block (value is JSON-encoded).
setField :: Member Memory effs => BlockHandle -> Text -> Text -> Eff effs ()
setField h f v = send (SetField h f v)

-- | Update a block's description.
updateDesc :: Member Memory effs => BlockHandle -> Text -> Eff effs ()
updateDesc h d = send (UpdateDesc h d)

delete :: Member Memory effs => BlockHandle -> Eff effs ()
delete h = send (Delete h)
