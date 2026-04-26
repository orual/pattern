{-# LANGUAGE OverloadedStrings #-}
-- | Pattern.Http — typed wrappers around 'Pattern.Port.Call' for HTTP requests.
--
-- All functions go through the runtime-provided @http@ port. The response is
-- JSON-encoded @{status, headers, body}@ returned by HttpPort on the Rust side.
--
-- For simple use-cases, 'httpGet' / 'httpPost' / 'httpDelete' return the full
-- response JSON as 'Text'. Use 'Pattern.Aeson' lens accessors to inspect the
-- status code, headers, or body.
--
-- If you need binary content (images, archives, etc.), use
-- @Pattern.Shell.execute "curl ..."@ with an appropriate capability — HttpPort
-- is text-only by design.
--
-- This module deliberately avoids Data.Aeson — Pattern's vendored
-- 'Pattern.Aeson' is the canonical JSON surface, and the tidepool-extract
-- package set does not include the upstream @aeson@ library. The payload
-- builders construct JSON via inline Text concatenation with proper escaping
-- of the URL/body string fields.
module Pattern.Http where

import Control.Monad.Freer (Eff, Member)
import Pattern.Port (Port, call)
import Data.Text (Text)
import qualified Data.Text as T

-- | Perform an HTTP GET. Returns the full response JSON (status, headers, body).
httpGet :: Member Port effs => Text -> Eff effs Text
httpGet url = call "http" "get" (urlPayload url)

-- | Perform an HTTP POST with a text body. Returns the full response JSON.
httpPost :: Member Port effs => Text -> Text -> Eff effs Text
httpPost url body = call "http" "post" (urlBodyPayload url body)

-- | Perform an HTTP PUT with a text body. Returns the full response JSON.
httpPut :: Member Port effs => Text -> Text -> Eff effs Text
httpPut url body = call "http" "put" (urlBodyPayload url body)

-- | Perform an HTTP DELETE. Returns the full response JSON.
httpDelete :: Member Port effs => Text -> Eff effs Text
httpDelete url = call "http" "delete" (urlPayload url)

-- | Perform an HTTP HEAD request. Returns the full response JSON (empty body).
httpHead :: Member Port effs => Text -> Eff effs Text
httpHead url = call "http" "head" (urlPayload url)

-- | Configure the http port with a base URL applied to subsequent
-- relative URLs.
httpConfigure :: Member Port effs => Text -> Eff effs Text
httpConfigure baseUrl =
  call "http" "configure"
    (T.concat ["{\"base_url\":\"", escape baseUrl, "\"}"])

-- Internal helpers --------------------------------------------------------

urlPayload :: Text -> Text
urlPayload url = T.concat ["{\"url\":\"", escape url, "\"}"]

urlBodyPayload :: Text -> Text -> Text
urlBodyPayload url body =
  T.concat ["{\"url\":\"", escape url, "\",\"body\":\"", escape body, "\"}"]

-- | JSON-escape a string per RFC 8259 §7. Handles backslash,
-- double-quote, the named whitespace controls, AND every other C0
-- control character (U+0001..U+001F not already named) via @\\u00XX@.
-- Bodies that contain raw control chars (e.g. ANSI escape sequences in
-- captured agent log output) would otherwise produce a JSON payload
-- that serde_json on the Rust side rejects as @BadPayload@.
--
-- Bytes outside the C0 range are passed through verbatim — agents are
-- responsible for ensuring the input is valid UTF-8 Text. Non-ASCII
-- characters are valid in JSON strings without escaping.
escape :: Text -> Text
escape = T.concatMap esc
  where
    esc '\\' = "\\\\"
    esc '"'  = "\\\""
    esc '\n' = "\\n"
    esc '\r' = "\\r"
    esc '\t' = "\\t"
    esc '\b' = "\\b"
    esc '\f' = "\\f"
    esc c
      | c < '\x20' = T.pack ("\\u" ++ pad4Hex (fromEnum c))
      | otherwise  = T.singleton c

    -- Render an Int as a 4-digit lowercase hex string, padded with zeros.
    pad4Hex :: Int -> String
    pad4Hex n =
      let hex = showHex n
          pad = replicate (4 - length hex) '0'
      in pad ++ hex

    showHex :: Int -> String
    showHex 0 = "0"
    showHex n = go n ""
      where
        go 0 acc = acc
        go k acc =
          let (q, r) = k `divMod` 16
              ch    = "0123456789abcdef" !! r
          in go q (ch : acc)
