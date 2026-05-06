{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE OverloadedStrings #-}
module Pattern.Web
  ( Web(..)
  , search
  , fetch
  , fetchReadable
  , fetchRaw
  , fetchContinue
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Web interaction effect: search the web and fetch page content.
data Web a where
  -- | Search the web. Returns JSON array of {title, url, snippet}.
  WebSearch :: Text -> Maybe Int -> Web Text
  -- | Fetch a URL. Format: Nothing/Just "readable" for extracted text,
  --   Just "raw" for HTML.
  WebFetch :: Text -> Maybe Text -> Web Text
  -- | Continue reading a fetched page from a character offset.
  WebFetchContinue :: Text -> Int -> Maybe Int -> Web Text

-- | Search the web for a query. Returns JSON array of search results.
search :: Member Web effs => Text -> Eff effs Text
search q = send (WebSearch q Nothing)

-- | Fetch a URL and extract readable text content.
fetch :: Member Web effs => Text -> Eff effs Text
fetch url = send (WebFetch url Nothing)

-- | Fetch a URL and extract readable text (explicit).
fetchReadable :: Member Web effs => Text -> Eff effs Text
fetchReadable url = send (WebFetch url (Just "readable"))

-- | Fetch a URL and return raw HTML.
fetchRaw :: Member Web effs => Text -> Eff effs Text
fetchRaw url = send (WebFetch url (Just "raw"))

-- | Continue reading from a previous fetch at a character offset.
fetchContinue :: Member Web effs => Text -> Int -> Eff effs Text
fetchContinue url offset = send (WebFetchContinue url offset Nothing)
