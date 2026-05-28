-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal agent exercising `Pattern.File.read` against a custom bundle
-- with only the File handler. Used by
-- `tests/bundle_non_prelude5.rs::file_handler_dispatches_and_reports_no_file_manager`
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
