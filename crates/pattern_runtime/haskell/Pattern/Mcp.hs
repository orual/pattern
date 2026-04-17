{-# LANGUAGE GADTs #-}
-- | Pattern.Mcp — Model-Context-Protocol tool calls.
--
-- Stubbed in Phase 3. Rust handler returns NotImplemented. Real
-- implementation lives in the post-foundation plugin-system plan.
module Pattern.Mcp where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

type Server = Text
type Method = Text

-- | Mcp effect algebra. Variant names are mirrored by
-- @Pattern.sdk::requests::mcp::McpReq@ (Rust).
data Mcp a where
  Call :: Server -> Method -> Mcp ()

call :: Member Mcp effs => Server -> Method -> Eff effs ()
call s m = send (Call s m)
