-- | Pattern.Prelude — ergonomic re-export of the common agent-SDK subset.
--
-- Agent programs typically need Time + Log + Memory + Message + Display.
-- Rarer effects (Shell / File / Sources / Mcp / Ipc / Spawn) are available
-- via their individual modules.
module Pattern.Prelude
  ( module Pattern.Time
  , module Pattern.Log
  , module Pattern.Memory
  , module Pattern.Message
  , module Pattern.Display
  ) where

import Pattern.Time
import Pattern.Log
import Pattern.Memory
import Pattern.Message
import Pattern.Display
