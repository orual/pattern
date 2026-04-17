{-# LANGUAGE GADTs #-}
-- | Pattern.Memory — persistent memory-block operations.
--
-- Stubbed in Phase 3. The GADT is declared so agent programs compile, but
-- the Rust handler is not wired until Phase 5 (memory adapter).
module Pattern.Memory where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Opaque handle referencing a memory block (Rust side uses SmolStr).
type BlockHandle = Text

-- | Content blob associated with a memory block.
type Content = Text

-- | Search query string for recall/search operations.
type Query = Text

-- | Memory effect algebra. Variant names are mirrored by
-- @Pattern.sdk::requests::memory::MemoryReq@ (Rust).
data Memory a where
  Read    :: BlockHandle -> Memory Content
  Write   :: BlockHandle -> Content -> Memory ()
  Append  :: BlockHandle -> Content -> Memory ()
  Search  :: Query -> Memory [BlockHandle]
  Recall  :: BlockHandle -> Memory Content
  Archive :: BlockHandle -> Memory ()

read_ :: Member Memory effs => BlockHandle -> Eff effs Content
read_ h = send (Read h)

write :: Member Memory effs => BlockHandle -> Content -> Eff effs ()
write h c = send (Write h c)

append :: Member Memory effs => BlockHandle -> Content -> Eff effs ()
append h c = send (Append h c)

search :: Member Memory effs => Query -> Eff effs [BlockHandle]
search q = send (Search q)

recall :: Member Memory effs => BlockHandle -> Eff effs Content
recall h = send (Recall h)

archive :: Member Memory effs => BlockHandle -> Eff effs ()
archive h = send (Archive h)
