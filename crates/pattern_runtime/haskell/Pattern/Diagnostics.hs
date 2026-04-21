{-# LANGUAGE GADTs #-}
-- | Pattern.Diagnostics — query session diagnostic events.
--
-- Agents can observe compile failures and other session-level diagnostics
-- via the 'diagnostics' helper. Events are accumulated during session
-- construction and are read-only thereafter.
module Pattern.Diagnostics where

import Control.Monad.Freer (Eff, Member)
import qualified Control.Monad.Freer as Freer
import Data.Text (Text)

-- | A single diagnostic event.
data DiagnosticEvent = DiagnosticEvent
  { severity :: Text
  , source   :: Text
  , message  :: Text
  , location :: Maybe Text
  }

-- | Effect algebra.
--
-- GetDiagnostics returns a JSON-encoded list of DiagnosticEvent records.
-- Decode with @Data.Aeson.decode@ or @Data.Aeson.eitherDecode@.
data Diagnostics a where
  GetDiagnostics :: Diagnostics Text

diagnostics :: Member Diagnostics effs => Eff effs Text
diagnostics = Freer.send GetDiagnostics
