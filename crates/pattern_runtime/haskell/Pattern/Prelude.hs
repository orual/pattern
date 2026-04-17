-- | Pattern.Prelude — ergonomic re-export of the full 11-effect SDK.
--
-- The SDK uses distinct constructor names across modules
-- (@Memory.Get@/@Put@, @File.Read@/@Write@/@ListDir@, @Rpc.Call@/@Recv@,
-- @Message.Send@, …), so @import Pattern.Prelude@ unqualified works even
-- when agents use several effects together. Qualified imports remain a
-- fine stylistic choice when you want explicit module attribution at the
-- call site (@Memory.Get \"label\"@ vs. @get \"label\"@).
module Pattern.Prelude
  ( module Pattern.Memory
  , module Pattern.Message
  , module Pattern.Display
  , module Pattern.Time
  , module Pattern.Log
  , module Pattern.Shell
  , module Pattern.File
  , module Pattern.Sources
  , module Pattern.Mcp
  , module Pattern.Rpc
  , module Pattern.Spawn
  ) where

import Pattern.Memory
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log
import Pattern.Shell
import Pattern.File
import Pattern.Sources
import Pattern.Mcp
import Pattern.Rpc
import Pattern.Spawn
