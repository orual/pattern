-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE BangPatterns, NoImplicitPrelude, OverloadedStrings #-}
-- | Advanced text utilities for code generation and formatting.
--
-- All functions operate on Data.Text. Available in MCP via:
-- @import Pattern.Text@
module Pattern.Text
  ( -- * Case conversion
    camelToSnake
  , snakeToCamel
  , capitalize
  , titleCase
    -- * Formatting
  , padLeft
  , padRight
  , padLeftWith
  , padRightWith
  , center
  , centerWith
  , indent
  , dedent
  , wrap
    -- * Transformations
  , slugify
  , truncateText
  ) where

import Prelude
  ( Int, Char, Bool(..), Maybe(..), String
  , Eq(..), Ord(..), Num(..), Integral(..)
  , Semigroup(..), Monoid(..)
  , ($), (.), otherwise, not, (&&), (||), negate, fst
  , map, filter, foldl, foldr, foldl'
  , null, error, fromIntegral, reverse, concatMap
  )
import Data.Text (Text)
import qualified Data.Text as T
import Pattern.Prelude
  ( words, lines, splitOn
  , isUpper, isLower, isAlphaNum, toLowerChar, toUpperChar
  )

-- ---------------------------------------------------------------------------
-- Case conversion
-- ---------------------------------------------------------------------------

-- | Convert camelCase or PascalCase to snake_case.
--
-- >>> camelToSnake "helloWorld"
-- "hello_world"
-- >>> camelToSnake "HTTPServer"
-- "h_t_t_p_server"
camelToSnake :: Text -> Text
camelToSnake = T.pack . go . T.unpack
  where
    go [] = []
    go (c:cs)
      | isUpper c = '_' : toLowerChar c : go cs
      | otherwise = c : go cs

-- | Convert snake_case to camelCase.
--
-- >>> snakeToCamel "hello_world"
-- "helloWorld"
snakeToCamel :: Text -> Text
snakeToCamel t = case splitOn "_" t of
  []     -> T.empty
  (w:ws) -> T.concat (w : map capitalize ws)

-- | Capitalize the first character of a Text.
--
-- >>> capitalize "hello"
-- "Hello"
capitalize :: Text -> Text
capitalize t = case T.uncons t of
  Nothing      -> T.empty
  Just (c, cs) -> T.cons (toUpperChar c) cs

-- | Capitalize each word in a Text.
--
-- >>> titleCase "hello world"
-- "Hello World"
titleCase :: Text -> Text
titleCase = T.unwords . map capitalize . words

-- ---------------------------------------------------------------------------
-- Formatting
-- ---------------------------------------------------------------------------

-- | Pad text on the left with spaces to a given width.
padLeft :: Int -> Text -> Text
padLeft w = padLeftWith w ' '

-- | Pad text on the right with spaces to a given width.
padRight :: Int -> Text -> Text
padRight w = padRightWith w ' '

-- | Pad text on the left to a given width with a custom character.
padLeftWith :: Int -> Char -> Text -> Text
padLeftWith w pad t
  | T.length t >= w = t
  | otherwise = T.replicate (w - T.length t) (T.singleton pad) <> t

-- | Pad text on the right to a given width with a custom character.
padRightWith :: Int -> Char -> Text -> Text
padRightWith w pad t
  | T.length t >= w = t
  | otherwise = t <> T.replicate (w - T.length t) (T.singleton pad)

-- | Center text in a field of given width, padding with spaces.
center :: Int -> Text -> Text
center w = centerWith w ' '

-- | Center text in a field of given width, padding with a custom character.
centerWith :: Int -> Char -> Text -> Text
centerWith w pad t
  | T.length t >= w = t
  | otherwise =
      let total = w - T.length t
          lpad  = total `div` 2
          rpad  = total - lpad
      in  T.replicate lpad (T.singleton pad) <> t <> T.replicate rpad (T.singleton pad)

-- | Indent every line of text by n spaces.
indent :: Int -> Text -> Text
indent n t = T.unlines (map (prefix <>) (lines t))
  where prefix = T.replicate n " "

-- | Remove common leading whitespace from all non-empty lines.
dedent :: Text -> Text
dedent t =
  let ls = lines t
      nonEmpty = filter (not . T.null . T.stripStart) ls
      minIndent = case nonEmpty of
        []     -> 0
        (x:xs) -> foldl' (\acc l -> min' acc (countLeading l)) (countLeading x) xs
  in  T.unlines (map (T.drop minIndent) ls)
  where
    countLeading = T.length . T.takeWhile (== ' ')
    min' a b = if a <= b then a else b

-- | Wrap text to a given line width at word boundaries.
wrap :: Int -> Text -> Text
wrap w = T.unlines . concatMap (wrapLine w) . lines
  where
    wrapLine :: Int -> Text -> [Text]
    wrapLine width line
      | T.length line <= width = [line]
      | otherwise = go width (words line) [] 0
    go _ [] acc _ = [T.unwords (reverse acc)]
    go width (word:ws) acc lineLen
      | null acc  = go width ws [word] (T.length word)
      | lineLen + 1 + T.length word > width =
          T.unwords (reverse acc) : go width (word:ws) [] 0
      | otherwise = go width ws (word:acc) (lineLen + 1 + T.length word)

-- ---------------------------------------------------------------------------
-- Transformations
-- ---------------------------------------------------------------------------

-- | Convert text to a URL-friendly slug (lowercase, hyphens for spaces/punctuation).
--
-- >>> slugify "Hello, World!"
-- "hello-world"
slugify :: Text -> Text
slugify = collapseHyphens . T.map toLowerOrHyphen . T.strip
  where
    toLowerOrHyphen c
      | isAlphaNum c = toLowerChar c
      | otherwise    = '-'
    collapseHyphens = T.intercalate "-" . filter (not . T.null) . splitOn "-"

-- | Truncate text to n characters, appending "..." if truncated.
truncateText :: Int -> Text -> Text
truncateText n t
  | T.length t <= n = t
  | n <= 3          = T.take n t
  | otherwise       = T.take (n - 3) t <> "..."
