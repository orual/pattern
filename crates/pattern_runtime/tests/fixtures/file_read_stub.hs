{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal agent exercising `Pattern.File.read` against a custom bundle
-- with only the File handler. Used by
-- `tests/bundle_non_prelude5.rs::file_handler_stub_reports_not_implemented`
-- to verify the non-Prelude-5 handler dispatches correctly.
--
-- Qualified import to avoid ambiguity with base Prelude.read (this file
-- has no NoImplicitPrelude pragma).
module FileReadStub (agent) where

import Control.Monad.Freer (Eff)
import qualified Pattern.File as F

agent :: Eff '[F.File] ()
agent = do
  _ <- F.read "/tmp/pattern-file-stub"
  pure ()
