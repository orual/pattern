-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal hello-world agent for the end-to-end integration test.
--
-- Imports `Pattern.Time` and `Pattern.Log` directly. Compilation is native
-- multi-module: `tidepool-extract` resolves these imports against the SDK
-- directory passed on the include path (see `compile_program` /
-- `compile_and_run`).
module Hello (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Time
import Pattern.Log

agent :: Eff '[Time, Log] ()
agent = do
  _t <- now
  info "hello from haskell"
