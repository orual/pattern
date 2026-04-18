{-# LANGUAGE BangPatterns, NoImplicitPrelude, OverloadedStrings #-}
-- | CSV/TSV parsing and table rendering.
--
-- Available in MCP via: @import Pattern.Table@
module Pattern.Table
  ( -- * Parsing
    parseCsv
  , parseTsv
  , parseDelimited
    -- * Rendering
  , renderTable
  , renderTableWith
    -- * Column operations
  , column
  , sortByColumn
  , filterByColumn
  ) where

import Prelude
  ( Int, Char, Bool(..), Maybe(..), String, Ordering(..)
  , Eq(..), Ord(..), Num(..), Show
  , Semigroup(..), Monoid(..)
  , ($), (.), otherwise, not, (&&), (||), negate, fst, snd
  , map, filter, foldl, foldl', foldr
  , null, error, fromIntegral
  , zip, length, replicate, reverse, concatMap
  )
import Data.Text (Text)
import qualified Data.Text as T
import Pattern.Prelude (enumFromTo, lines, splitOn, sortBy, comparing, strip)
import Pattern.Text (padRightWith)

-- ---------------------------------------------------------------------------
-- Parsing
-- ---------------------------------------------------------------------------

-- | Parse CSV text into rows of fields.
-- Handles simple CSV (no quoting). Splits on commas and newlines.
parseCsv :: Text -> [[Text]]
parseCsv = parseDelimited ','

-- | Parse TSV text into rows of fields.
parseTsv :: Text -> [[Text]]
parseTsv = parseDelimited '\t'

-- | Parse text delimited by the given character into rows of fields.
parseDelimited :: Char -> Text -> [[Text]]
parseDelimited delim t =
  let ls = filter (not . T.null . strip) (lines t)
  in  map (splitOn (T.singleton delim)) ls

-- ---------------------------------------------------------------------------
-- Rendering
-- ---------------------------------------------------------------------------

-- | Render a list of rows as an aligned table with pipe separators.
--
-- >>> renderTable [["Name","Age"],["Alice","30"],["Bob","25"]]
-- "| Name  | Age |"
-- "| Alice | 30  |"
-- "| Bob   | 25  |"
renderTable :: [[Text]] -> Text
renderTable = renderTableWith '|' ' '

-- | Render a table with custom separator and padding characters.
renderTableWith :: Char -> Char -> [[Text]] -> Text
renderTableWith sep pad rows =
  let widths = colWidths rows
      rendered = map (renderRow sep pad widths) rows
  in  T.unlines rendered

colWidths :: [[Text]] -> [Int]
colWidths [] = []
colWidths rows =
  let ncols = maxList 0 (map length rows)
      getCol i = map (safeIndex i) rows
      safeIndex i xs = case safeDrop i xs of
        []    -> T.empty
        (x:_) -> x
  in  map (\i -> maxList 0 (map T.length (getCol i))) (enumFromTo 0 (ncols - 1))

maxList :: Int -> [Int] -> Int
maxList d [] = d
maxList _ (x:xs) = foldl' (\a b -> if a >= b then a else b) x xs

renderRow :: Char -> Char -> [Int] -> [Text] -> Text
renderRow sep pad widths fields =
  let sepT = T.singleton sep
      padT = T.singleton pad
      cells = zipPad widths fields
      rendered = map (\(w, f) -> padT <> padRightWith w pad f <> padT) cells
  in  sepT <> T.intercalate sepT rendered <> sepT

zipPad :: [Int] -> [Text] -> [(Int, Text)]
zipPad [] _ = []
zipPad (w:ws) [] = (w, T.empty) : zipPad ws []
zipPad (w:ws) (f:fs) = (w, f) : zipPad ws fs

safeDrop :: Int -> [a] -> [a]
safeDrop 0 xs     = xs
safeDrop _ []     = []
safeDrop !n (_:xs) = safeDrop (n - 1) xs

-- ---------------------------------------------------------------------------
-- Column operations
-- ---------------------------------------------------------------------------

-- | Extract a column by index (0-based) from parsed rows.
column :: Int -> [[Text]] -> [Text]
column i = map (safeIdx i)
  where
    safeIdx n xs = case safeDrop n xs of
      []    -> T.empty
      (x:_) -> x

-- | Sort rows by a column index (0-based), using Text ordering.
-- First row (header) stays in place if present.
sortByColumn :: Int -> [[Text]] -> [[Text]]
sortByColumn _ [] = []
sortByColumn i (header:rows) = header : sortBy (comparing (safeIdx i)) rows
  where
    safeIdx n xs = case safeDrop n xs of
      []    -> T.empty
      (x:_) -> x

-- | Filter rows where the column value satisfies a predicate.
-- First row (header) is always kept.
filterByColumn :: Int -> (Text -> Bool) -> [[Text]] -> [[Text]]
filterByColumn _ _ [] = []
filterByColumn i p (header:rows) = header : filter (\r -> p (safeIdx i r)) rows
  where
    safeIdx n xs = case safeDrop n xs of
      []    -> T.empty
      (x:_) -> x

