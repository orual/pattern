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
