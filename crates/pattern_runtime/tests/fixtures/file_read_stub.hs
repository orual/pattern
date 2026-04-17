{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal agent exercising `Pattern.File.read_` against a custom bundle
-- with only the File handler. Used by
-- `tests/bundle_non_prelude5.rs::file_handler_stub_reports_not_implemented`
-- to verify the non-Prelude-5 handler dispatches correctly.
--
-- Pattern.File alone in scope — no collisions.
module FileReadStub (agent) where

import Control.Monad.Freer (Eff)
import Pattern.File

agent :: Eff '[File] ()
agent = do
  _ <- read_ "/tmp/pattern-file-stub"
  pure ()
