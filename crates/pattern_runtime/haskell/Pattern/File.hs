{-# LANGUAGE GADTs #-}
-- | Pattern.File — filesystem access (sandboxed).
--
-- Stubbed in Phase 3. Rust handler returns NotImplemented; real
-- implementation will route through a capability-scoped sandbox.
module Pattern.File where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

type Path    = Text
type Content = Text

-- | File effect algebra. Variant names are mirrored by
-- @Pattern.sdk::requests::file::FileReq@ (Rust).
data File a where
  Read  :: Path -> File Content
  Write :: Path -> Content -> File ()
  List  :: Path -> File [Path]

read_ :: Member File effs => Path -> Eff effs Content
read_ p = send (Read p)

write :: Member File effs => Path -> Content -> Eff effs ()
write p c = send (Write p c)

list :: Member File effs => Path -> Eff effs [Path]
list p = send (List p)
