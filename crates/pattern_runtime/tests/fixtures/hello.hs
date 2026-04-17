{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal hello-world agent for the end-to-end integration test.
--
-- Imports Pattern.Time and Pattern.Log without qualification. The runtime
-- inliner (`pattern_runtime::tidepool::inline::inline_sdk_modules`) flattens
-- these SDK modules into a single combined Haskell module before invoking
-- tidepool-extract. Because the modules are inlined rather than imported,
-- their definitions are in scope unqualified; qualified aliases (e.g.
-- `import qualified Pattern.Time as Time`) would not resolve after inlining.
module Hello (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Time
import Pattern.Log

agent :: Eff '[Time, Log] ()
agent = do
  _t <- now
  info "hello from haskell"
