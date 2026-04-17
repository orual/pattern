-- | Pattern.Prelude — ergonomic re-export of the common agent-SDK subset.
--
-- Re-exports the five Prelude effects in the order expected by
-- `pattern_runtime::sdk::bundle::SdkBundle`:
-- `Memory, Message, Display, Time, Log`. These five form the head of
-- the full 11-handler bundle; agents declaring
-- `Eff '[Memory, Message, Display, Time, Log] a` line up with JIT tags
-- 0..4 correctly.
--
-- The remaining six effects (`Shell, File, Sources, Mcp, Ipc, Spawn`)
-- are available as individual modules. They are *not* re-exported from
-- Prelude because some of their constructors collide with Memory's
-- (`Pattern.File` and `Pattern.Memory` both export `Read` / `Write`),
-- which would make `import Pattern.Prelude` ambiguous at any use site.
-- Agents that need those effects should import them qualified, e.g.
-- `import qualified Pattern.File as F` + `F.read_ path`.
module Pattern.Prelude
  ( module Pattern.Memory
  , module Pattern.Message
  , module Pattern.Display
  , module Pattern.Time
  , module Pattern.Log
  ) where

import Pattern.Memory
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log
