{-# LANGUAGE GADTs #-}
-- | Pattern.Sources — external data streams (firehose, process output,
-- future RSS / webhooks / etc.).
--
-- Stubbed in Phase 3. runtime handler returns NotImplemented; real
-- implementation will wrap the preserved `data_source/` abstractions.
module Pattern.Sources where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

type Name = Text
type Cb   = Text  -- Stub: real type carries callback closure; Phase 5 decides encoding.

-- | Effect algebra.
data Sources a where
  Stream    :: Name -> Sources Text
  Subscribe :: Name -> Cb -> Sources ()
  List      :: Sources [Name]

stream :: Member Sources effs => Name -> Eff effs Text
stream n = send (Stream n)

subscribe :: Member Sources effs => Name -> Cb -> Eff effs ()
subscribe n c = send (Subscribe n c)

list :: Member Sources effs => Eff effs [Name]
list = send List
