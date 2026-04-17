{-# LANGUAGE GADTs #-}
-- | Pattern.File — filesystem access (sandboxed).
--
-- Stubbed in Phase 3. The runtime currently returns NotImplemented; real
-- implementation will route through a capability-scoped sandbox.
--
-- 'List' is named 'ListDir' to avoid colliding with 'Pattern.Sources.List'
-- (the canonical "list all sources" op).
module Pattern.File where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

type Path    = Text
type Content = Text

-- | File effect algebra.
data File a where
  Read    :: Path -> File Content
  Write   :: Path -> Content -> File ()
  ListDir :: Path -> File [Path]

read_ :: Member File effs => Path -> Eff effs Content
read_ p = send (Read p)

write :: Member File effs => Path -> Content -> Eff effs ()
write p c = send (Write p c)

-- | List entries of a directory.
listDir :: Member File effs => Path -> Eff effs [Path]
listDir p = send (ListDir p)
