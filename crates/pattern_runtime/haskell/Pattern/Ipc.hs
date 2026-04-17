{-# LANGUAGE GADTs #-}
-- | Pattern.Ipc — inter-process communication between constellation
-- members.
--
-- Stubbed in Phase 3. Rust handler returns NotImplemented.
module Pattern.Ipc where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

type Peer    = Text
type Payload = Text

-- | Ipc effect algebra. Variant names are mirrored by
-- @Pattern.sdk::requests::ipc::IpcReq@ (Rust).
data Ipc a where
  Send :: Peer -> Payload -> Ipc ()
  Recv :: Peer -> Ipc Payload

send_ :: Member Ipc effs => Peer -> Payload -> Eff effs ()
send_ p m = send (Send p m)

recv :: Member Ipc effs => Peer -> Eff effs Payload
recv p = send (Recv p)
