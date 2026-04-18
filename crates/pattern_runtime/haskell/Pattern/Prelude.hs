{-# LANGUAGE BangPatterns, NoImplicitPrelude, FlexibleInstances #-}
-- | Pattern.Prelude — a curated base-prelude substitute (Text-returning
-- @show@, Text-safe list/text helpers, JSON construction, Map/Set
-- re-exports). Ported from tidepool-mcp's @Pattern.Prelude@; now
-- fully first-party under the Pattern namespace so agents don't
-- need to know about the tidepool internals.
--
-- Does NOT re-export the 13-effect SDK modules. Agent programs using
-- the full SDK import them directly (qualified or unqualified, per
-- the hybrid scheme documented in CLAUDE.md). Code-tool programs
-- have the GADT declarations and helpers inlined by the preamble
-- builder and do not need explicit SDK imports.
--
-- Used with @{-# LANGUAGE NoImplicitPrelude #-}@: base's @Prelude@
-- is NOT implicitly in scope, so every symbol an agent uses comes
-- either from @Pattern.Prelude@ or an explicit import.
module Pattern.Prelude
  ( -- * Types (re-exported from base)
    Int, Integer, Word, Char, Bool(..), Double, Float
  , String, Ordering(..), Maybe(..), Either(..)
    -- * Text type (re-exported from Data.Text)
  , Text
  , Pack(..), unpack
  , toUpper, toLower
  , strip
  , splitOn
  , replace
  , isSuffixOf, isInfixOf
    -- * Text versions of words/lines
  , words, lines, unwords, unlines
    -- * Typeclasses (re-exported from base)
  , Eq(..), Ord(..), Num(..), Integral(..), Real, Fractional(..), Floating(..), Show
  , Semigroup(..), Monoid(..)
  , fromIntegral, realToFrac, truncate, ceiling, floor, round
  , Functor(..), Applicative(..), Monad(..)
  , (<$>)
    -- * show (Text-returning shadow)
  , show, showT
  , showDouble
    -- * Basic functions (re-exported from base)
  , id, const, flip, (.), ($), ($!)
  , not, (&&), (||), otherwise, seq
  , fst, snd, curry, uncurry
  , error, undefined
    -- * List operations
  , map, filter, foldl, foldl', foldr
  , null
  , take, drop, zip, zipWith, unzip
  , lookup, elem, notElem
  , any, all, and, or
  , sum, product, minimum, maximum
  , concat, iterate, repeat, cycle
  , scanl, scanr
    -- * Self-contained list operations
  , reverse
  , splitAt
  , span
  , break
  , init
  , nub
  , nubBy
  , sort
  , sortBy
  , concatMap, concatMapM
  , append
  , (++)
  , dropWhile
  , length
  , replicate
  , isPrefixOf
  , intersperse
    -- * Text intercalate (shadows list version)
  , intercalate
  , joinText
  , tReverse
    -- * Text takeWhile/dropWhile (shadows T.takeWhile/T.dropWhile to avoid PAP bug)
  , takeWhileT
  , dropWhileT
    -- * Polymorphic typeclasses (work on both Text and [a])
  , Len(..), Null(..), Slice(..)
    -- * Additional list combinators
  , find
  , partition
  , groupBy
  , takeWhile
  , tails
  , unfoldr
  , mapAccumL
  , transpose
  , genericLength
  , zipWith3
  , zipWith4
    -- * Function combinators
  , on
  , comparing
    -- * Monadic combinators
  , mapM, mapM_, sequence, sequence_
  , when, unless, void, join, guard
  , forM, forM_
  , (=<<), (>=>), (<=<)
  , foldM, foldM_
  , filterM, replicateM, zipWithM
    -- * Maybe/Either utilities
  , maybe, fromMaybe, isJust, isNothing, catMaybes, mapMaybe
  , either
    -- * Partial functions (use with care)
  , head
  , tail
  , last
    -- * Numeric utilities
  , even, odd
    -- * Text-to-number parsing
  , parseIntM, parseInt, parseDoubleM, parseDouble
    -- * Char predicates & conversions
  , ord, chr, fromEnum
  , isDigit, isAlpha, isAlphaNum, isSpace, isUpper, isLower
  , digitToInt, toLowerChar, toUpperChar
    -- * Indexed list operations (safe alternatives to [0..])
  , zipWithIndex, imap, enumFromTo
    -- * Monomorphic numeric helpers
  , abs', signum', min', max'
    -- * Additional list combinators (P2)
  , elemIndex, findIndex
  , zip3, unzip3
    -- * Map/Set types
  , Map, Set
    -- * JSON (Pattern.Aeson — vendored, construction-only)
  , Value(..), Key, object, (.=), toJSON
  , ToJSON
    -- * JSON lenses (Pattern.Aeson.Lens + Control.Lens)
  , key, nth, _String, _Number, _Bool, _Array, _Object, _Int, _Double
  , members, values, _Null
  , preview, toListOf, (^?), (^..), (&), (.~), (%~), to, _Just, traverse
    -- * JSON Value helpers
  , (?.), lookupKey, asText, asInt, asDouble, asBool, asArray, asObject
    -- * Map operations (qualified via Map prefix)
  , Map.fromList, Map.toList, Map.insert, Map.delete
  , Map.member, Map.size, Map.keys, Map.elems
  , Map.union, Map.intersection, Map.difference
  , Map.foldlWithKey', Map.foldrWithKey
  , Map.mapKeys, Map.mapWithKey, Map.filterWithKey
  , Map.singleton, Map.empty
  , Map.findWithDefault, Map.adjust
  , Map.unionWith, Map.intersectionWith
    -- * Set type (use qualified Set.xxx via preamble's `import qualified Data.Set as Set`)
    -- * Map helpers (local impls — unqualified, unlike Map.* re-exports above)
  , insertWith
  ) where

-- Original tidepool-prelude imports follow:

import Prelude
  ( Int, Integer, Word, Char, Bool(..), Double, Float
  , String, Ordering(..), Maybe(..), Either(..)
  , Eq(..), Ord(..), Num(..), Integral(..), Real, Fractional(..), Floating(..), Show
  , Semigroup(..), Monoid(..)
  , fromIntegral, realToFrac, truncate, ceiling, floor, even, odd
  , Functor(..), Applicative(..), Monad(..)
  , (<$>)
  , id, const, flip, (.), ($), ($!)
  , not, (&&), (||), otherwise, seq
  , fst, snd, curry, uncurry
  , error, undefined
  , maybe, either
  , map, foldl, foldr
  , take, drop, zip, zipWith, unzip
  , lookup, elem, notElem
  , any, all, and, or
  , sum, product, minimum, maximum
  , concat, iterate, repeat, cycle
  , scanl, scanr
  , negate, quot, rem
  , compare
  , fromEnum
  , mapM, mapM_, sequence, sequence_
  )
import qualified Prelude as P (show, drop, length, null, dropWhile)
import Data.Text (Text)
import qualified Data.Text as T
import Data.Char (ord, chr)
import Data.Maybe (fromMaybe, isJust, isNothing, catMaybes, mapMaybe)
import Data.List (foldl', find, partition, groupBy, takeWhile, tails, unfoldr, mapAccumL, transpose, genericLength, sort, sortBy)
import Data.Map.Strict (Map)
import Data.Set (Set)
import Control.Monad
  ( when, unless, void, join, guard
  , forM, forM_
  , (=<<), (>=>), (<=<)
  , foldM, foldM_
  )
import Pattern.Aeson (Value(..), Key, object, (.=), toJSON, ToJSON, fromText)
import Pattern.Aeson.Lens (key, nth, _String, _Number, _Bool, _Array, _Object, _Int, _Double, members, values, _Null)
import Control.Lens (preview, toListOf, (^?), (^..), (&), (.~), (%~), to, _Just, traverse)
import qualified Data.Map.Strict as Map

-- Permanent binding-level interception in Translate.hs.
-- GHC's floatToDigits/Integer pipeline is fundamentally incompatible with
-- the JIT, so showDouble is always intercepted and emitted as ShowDoubleAddr.
-- The body is a fallback that should never run.
-- The Double arg must be used to prevent GHC worker-wrapper from dropping it.
{-# NOINLINE showDouble #-}
showDouble :: Double -> String
showDouble d = case d of !_ -> error "showDouble: should be intercepted by Translate"

-- | Text-returning show: @show x@ gives @Text@ instead of @String@.
show :: Show a => a -> Text
show = T.pack . P.show

-- | Alias for 'show' (for discoverability, since our @show@ returns @Text@).
showT :: Show a => a -> Text
showT = show

-- | Polymorphic pack: identity on Text, T.pack on String.
-- Single-method typeclass, no error branches — JIT-safe.
class Pack a where
  pack :: a -> Text

instance Pack String where
  pack = T.pack
  {-# INLINE pack #-}

instance Pack Text where
  pack = id
  {-# INLINE pack #-}

unpack :: Text -> String
unpack = T.unpack

toUpper :: Text -> Text
toUpper = T.toUpper

toLower :: Text -> Text
toLower = T.toLower

strip :: Text -> Text
strip = T.strip

-- Pure reimplementation for Prelude export (avoids text-package dependency chain).
splitOn :: Text -> Text -> [Text]
splitOn sep t
  | T.null sep = map (\c -> T.pack [c]) (T.unpack t)
  | otherwise  = go (T.unpack t) (T.unpack sep)
  where
    go [] _     = [T.pack ""]
    go s  sepCs = case matchAt [] s sepCs of
      Nothing          -> [T.pack s]
      Just (pre, rest) -> T.pack pre : go rest sepCs
    matchAt _   [] _ = Nothing
    matchAt acc s@(c:cs) sepCs
      | startsWith s sepCs = Just (reverse acc, P.drop (P.length sepCs) s)
      | otherwise          = matchAt (c:acc) cs sepCs
    startsWith _ []     = True
    startsWith [] _     = False
    startsWith (c:cs) (p:ps) = c == p && startsWith cs ps

replace :: Text -> Text -> Text -> Text
replace = T.replace

isSuffixOf :: Text -> Text -> Bool
isSuffixOf = T.isSuffixOf

isInfixOf :: Text -> Text -> Bool
isInfixOf = T.isInfixOf

-- Pure reimplementation for Prelude export (avoids text-package dependency chain).
words :: Text -> [Text]
words t = go (T.unpack t)
  where
    go [] = []
    go s  = let s'       = P.dropWhile isSpace s
                (w, rest) = breakOnSpace s'
            in if P.null w then [] else T.pack w : go rest
    breakOnSpace [] = ([], [])
    breakOnSpace (c:cs)
      | isSpace c = ([], c:cs)
      | otherwise = let (w, r) = breakOnSpace cs in (c:w, r)

-- Pure reimplementation for Prelude export (avoids text-package dependency chain).
lines :: Text -> [Text]
lines t = go (T.unpack t)
  where
    go [] = []
    go s  = let (l, rest) = breakOnNL s
            in T.pack l : case rest of
                 []        -> []
                 (_:rest') -> go rest'
    breakOnNL [] = ([], [])
    breakOnNL (c:cs)
      | c == '\n' = ([], c:cs)
      | otherwise = let (l, r) = breakOnNL cs in (c:l, r)

unwords :: [Text] -> Text
unwords = T.unwords

unlines :: [Text] -> Text
unlines = T.unlines

-- | Append two lists.
append :: [a] -> [a] -> [a]
append []     ys = ys
append (x:xs) ys = x : append xs ys
{-# INLINE append #-}

(++) :: [a] -> [a] -> [a]
(++) = append
{-# INLINE (++) #-}
infixr 5 ++

-- | Check if a list is empty.
null :: [a] -> Bool
null [] = True
null _  = False
{-# INLINE null #-}

-- | Reverse a list.
reverse :: [a] -> [a]
reverse = go []
  where
    go :: [a] -> [a] -> [a]
    go acc []     = acc
    go acc (x:xs) = go (x:acc) xs
{-# INLINE reverse #-}

-- | Split a list at position n.
splitAt :: Int -> [a] -> ([a], [a])
splitAt n xs = go n xs
  where
    go :: Int -> [a] -> ([a], [a])
    go 0 ys      = ([], ys)
    go _ []      = ([], [])
    go !m (y:ys) = let (as, bs) = go (m - 1) ys in (y:as, bs)
{-# INLINE splitAt #-}

-- | Take the longest prefix satisfying a predicate.
span :: (a -> Bool) -> [a] -> ([a], [a])
span _ []     = ([], [])
span p xs@(x:xs')
  | p x       = let (ys, zs) = span p xs' in (x:ys, zs)
  | otherwise  = ([], xs)
{-# INLINE span #-}

-- | Take the longest prefix NOT satisfying a predicate.
break :: (a -> Bool) -> [a] -> ([a], [a])
break _ []     = ([], [])
break p xs@(x:xs')
  | p x       = ([], xs)
  | otherwise  = let (ys, zs) = break p xs' in (x:ys, zs)
{-# INLINE break #-}

-- | Drop the longest prefix satisfying a predicate.
dropWhile :: (a -> Bool) -> [a] -> [a]
dropWhile _ []     = []
dropWhile p (x:xs)
  | p x       = dropWhile p xs
  | otherwise  = x : xs
{-# INLINE dropWhile #-}

-- | All elements except the last. Returns [] for empty input.
init :: [a] -> [a]
init []     = []
init [_]    = []
init (x:xs) = x : init xs
{-# INLINE init #-}


-- | Map a function over a list and concatenate results.
concatMap :: (a -> [b]) -> [a] -> [b]
concatMap _ []     = []
concatMap f (x:xs) = f x `append` concatMap f xs
{-# INLINE concatMap #-}

-- | Monadic concatMap: map an effectful function over a list and concatenate results.
-- @concatMapM f xs = fmap concat (mapM f xs)@
concatMapM :: Monad m => (a -> m [b]) -> [a] -> m [b]
concatMapM f xs = fmap concat (mapM f xs)
{-# INLINE concatMapM #-}

-- | Monadic filter: keep elements for which the effectful predicate returns True.
-- @filterM (\\f -> isInfixOf "unsafe" \<$\> fsRead f) files@
filterM :: Monad m => (a -> m Bool) -> [a] -> m [a]
filterM _ []     = pure []
filterM p (x:xs) = do
  keep <- p x
  rest <- filterM p xs
  pure (if keep then x : rest else rest)

-- | Repeat an effect N times, collecting results.
-- @replicateM 3 (ask "next?")@
replicateM :: Monad m => Int -> m a -> m [a]
replicateM n act = go n
  where
    go i | i <= 0    = pure []
         | otherwise = do { x <- act; xs <- go (i - 1); pure (x : xs) }

-- | Zip two lists with an effectful function.
-- @zipWithM (\\a b -> llmJson (a \<\> b) schema) prompts contexts@
zipWithM :: Monad m => (a -> b -> m c) -> [a] -> [b] -> m [c]
zipWithM f (a:as) (b:bs) = do { c <- f a b; cs <- zipWithM f as bs; pure (c : cs) }
zipWithM _ _      _      = pure []

-- | Length of a list.
length :: [a] -> Int
length = go 0
  where
    go :: Int -> [a] -> Int
    go !acc []     = acc
    go !acc (_:xs) = go (acc + 1) xs
{-# INLINE length #-}

-- | Build a list of n copies of a value.
replicate :: Int -> a -> [a]
replicate n x = go n
  where
    go 0 = []
    go !m = x : go (m - 1)
{-# INLINE replicate #-}

-- | Join a list of Texts with a separator. Shadows list intercalate.
-- For list intercalate, use @import qualified Data.List as L@ then @L.intercalate@.
intercalate :: Text -> [Text] -> Text
intercalate = T.intercalate
{-# INLINE intercalate #-}

-- | Alias for 'intercalate' (for discoverability).
joinText :: Text -> [Text] -> Text
joinText = T.intercalate
{-# INLINE joinText #-}

-- | Reverse a Text.
tReverse :: Text -> Text
tReverse = T.reverse
{-# INLINE tReverse #-}

-- | Text takeWhile: take the longest prefix of characters satisfying a predicate.
-- Pure reimplementation — avoids fat-interface PAP bug with @T.takeWhile@.
-- Use this instead of @T.takeWhile@ in point-free / higher-order contexts.
takeWhileT :: (Char -> Bool) -> Text -> Text
takeWhileT p t = T.pack (go (T.unpack t))
  where
    go [] = []
    go (c:cs)
      | p c       = c : go cs
      | otherwise = []
{-# INLINE takeWhileT #-}

-- | Text dropWhile: drop the longest prefix of characters satisfying a predicate.
-- Pure reimplementation — avoids fat-interface PAP bug with @T.dropWhile@.
-- Use this instead of @T.dropWhile@ in point-free / higher-order contexts.
dropWhileT :: (Char -> Bool) -> Text -> Text
dropWhileT p t = T.pack (go (T.unpack t))
  where
    go [] = []
    go s@(c:cs)
      | p c       = go cs
      | otherwise = s
{-# INLINE dropWhileT #-}

-- ---------------------------------------------------------------------------
-- Polymorphic typeclasses (work on both Text and [a])
-- ---------------------------------------------------------------------------

-- | Length of a container. Works on both Text and lists.
class Len a where
  len :: a -> Int

instance Len Text where
  len = T.length
  {-# INLINE len #-}

instance Len [a] where
  len [] = 0
  len (_:xs) = 1 + len xs
  {-# INLINE len #-}

-- | Emptiness check. Works on both Text and lists.
class Null a where
  isNull :: a -> Bool

instance Null Text where
  isNull = T.null
  {-# INLINE isNull #-}

instance Null [a] where
  isNull [] = True
  isNull _  = False
  {-# INLINE isNull #-}

-- | Take/drop prefix. Works on both Text and lists.
-- Named @stake@/@sdrop@ to avoid shadowing list @take@/@drop@.
class Slice a where
  stake :: Int -> a -> a
  sdrop :: Int -> a -> a

instance Slice Text where
  stake = T.take
  sdrop = T.drop
  {-# INLINE stake #-}
  {-# INLINE sdrop #-}

instance Slice [a] where
  stake 0 _  = []
  stake _ [] = []
  stake n (x:xs) = x : stake (n-1) xs
  sdrop 0 xs = xs
  sdrop _ [] = []
  sdrop n (_:xs) = sdrop (n-1) xs
  {-# INLINE stake #-}
  {-# INLINE sdrop #-}

-- | Is the first Text a prefix of the second?
isPrefixOf :: Text -> Text -> Bool
isPrefixOf = T.isPrefixOf
{-# INLINE isPrefixOf #-}

-- | Insert an element between every pair of elements.
intersperse :: a -> [a] -> [a]
intersperse _   []     = []
intersperse _   [x]    = [x]
intersperse sep (x:xs) = x : sep : intersperse sep xs
{-# INLINE intersperse #-}

-- | Extract the first element. Partial: errors on empty list.
head :: [a] -> a
head (x:_) = x
head []    = error "head: empty list"
{-# INLINE head #-}

-- | Extract all elements after the head. Partial: errors on empty list.
tail :: [a] -> [a]
tail (_:xs) = xs
tail []     = error "tail: empty list"
{-# INLINE tail #-}

-- | Extract the last element. Partial: errors on empty list.
last :: [a] -> a
last [x]    = x
last (_:xs) = last xs
last []     = error "last: empty list"
{-# INLINE last #-}

-- #155: Monomorphic even/odd shadows removed — GHC specialization
-- (re-enabled) eliminates Integral dictionary passing at compile time.

-- Monomorphic round :: Double -> Int.
-- GHC specializes round @Double @Int but the specialized version calls
-- rintDouble (FFI to C's rint()), which we don't support. This shadow
-- avoids the FFI call entirely.
round :: Double -> Int
round d =
  let n = truncate d :: Int
      f = d - fromIntegral n  -- fractional part
      af = if f < 0.0 then negate f else f
  in if af < 0.5 then n
     else if af > 0.5 then (if f > 0.0 then n + 1 else n - 1)
     else if even n then n  -- banker's rounding: round to even on .5
          else (if f > 0.0 then n + 1 else n - 1)
{-# INLINE round #-}

-- | Zip three lists with a function.
zipWith3 :: (a -> b -> c -> d) -> [a] -> [b] -> [c] -> [d]
zipWith3 f (a:as) (b:bs) (c:cs) = f a b c : zipWith3 f as bs cs
zipWith3 _ _ _ _ = []
{-# INLINE zipWith3 #-}

-- | Zip four lists with a function.
zipWith4 :: (a -> b -> c -> d -> e) -> [a] -> [b] -> [c] -> [d] -> [e]
zipWith4 f (a:as) (b:bs) (c:cs) (d:ds) = f a b c d : zipWith4 f as bs cs ds
zipWith4 _ _ _ _ _ = []
{-# INLINE zipWith4 #-}

-- | Apply a binary function with arguments from a projection.
on :: (b -> b -> c) -> (a -> b) -> a -> a -> c
on f g x y = f (g x) (g y)
{-# INLINE on #-}

-- | Build a comparison from a projection.
comparing :: Ord b => (a -> b) -> a -> a -> Ordering
comparing f x y = compare (f x) (f y)
{-# INLINE comparing #-}

-- ---------------------------------------------------------------------------
-- Text-to-number parsing (avoids Read typeclass which crashes the JIT)
-- ---------------------------------------------------------------------------

-- | Parse an integer from Text, returning Nothing on failure.
parseIntM :: Text -> Maybe Int
parseIntM t = case T.uncons t of
  Nothing -> Nothing
  Just ('-', rest) -> negate <$> parseNat rest
  Just ('+', rest) -> parseNat rest
  Just _           -> parseNat t
  where
    parseNat :: Text -> Maybe Int
    parseNat s
      | T.null s          = Nothing
      | T.all isDigitC s  = Just (T.foldl' (\acc c -> acc * 10 + (ord c - ord '0')) 0 s)
      | otherwise         = Nothing
    isDigitC :: Char -> Bool
    isDigitC c = c >= '0' && c <= '9'

-- | Parse an integer from Text, calling error on failure.
parseInt :: Text -> Int
parseInt t = fromMaybe (error ("parseInt: not a number: " <> T.unpack t)) (parseIntM t)

-- | Parse a Double from Text, returning Nothing on failure.
-- Handles optional sign, integer part, optional decimal part.
parseDoubleM :: Text -> Maybe Double
parseDoubleM t = case T.uncons t of
  Nothing -> Nothing
  Just ('-', rest) -> negate <$> parsePos rest
  Just ('+', rest) -> parsePos rest
  Just _           -> parsePos t
  where
    parsePos :: Text -> Maybe Double
    parsePos s = case T.break (== '.') s of
      (intPart, rest)
        | T.null intPart -> Nothing
        | not (T.all isDigitC intPart) -> Nothing
        | T.null rest ->
            Just (fromIntegral (parseDigits intPart))
        | otherwise -> case T.uncons rest of
            Just ('.', fracPart)
              | T.null fracPart -> Just (fromIntegral (parseDigits intPart))
              | T.all isDigitC fracPart ->
                  let whole = fromIntegral (parseDigits intPart) :: Double
                      frac  = fromIntegral (parseDigits fracPart) :: Double
                      denom = fromIntegral (pow10 (T.length fracPart)) :: Double
                  in  Just (whole + frac / denom)
              | otherwise -> Nothing
            _ -> Nothing
    parseDigits :: Text -> Int
    parseDigits = T.foldl' (\acc c -> acc * 10 + (ord c - ord '0')) 0
    pow10 :: Int -> Int
    pow10 0 = 1
    pow10 !n = 10 * pow10 (n - 1)
    isDigitC :: Char -> Bool
    isDigitC c = c >= '0' && c <= '9'

-- | Parse a Double from Text, calling error on failure.
parseDouble :: Text -> Double
parseDouble t = fromMaybe (error ("parseDouble: not a number: " <> T.unpack t)) (parseDoubleM t)

-- ---------------------------------------------------------------------------
-- JSON Value helpers
-- ---------------------------------------------------------------------------

-- | Safe key lookup: @v ?. "name"@ returns @Just val@ or @Nothing@.
(?.) :: Value -> Text -> Maybe Value
Object o ?. k = Map.lookup (fromText k) o
_        ?. _ = Nothing
infixl 9 ?.
{-# INLINE (?.) #-}

-- | Lookup a key in a Value, returning Nothing if not an Object or key missing.
lookupKey :: Text -> Value -> Maybe Value
lookupKey k (Object o) = Map.lookup (fromText k) o
lookupKey _ _          = Nothing
{-# INLINE lookupKey #-}

-- | Extract Text from a String Value, or Nothing.
asText :: Value -> Maybe Text
asText (String t) = Just t
asText _          = Nothing
{-# INLINE asText #-}

-- | Extract Int from a Number Value (truncates), or Nothing.
asInt :: Value -> Maybe Int
asInt (Number d) = Just (truncate d)
asInt _          = Nothing
{-# INLINE asInt #-}

-- | Extract Double from a Number Value, or Nothing.
asDouble :: Value -> Maybe Double
asDouble (Number d) = Just d
asDouble _          = Nothing
{-# INLINE asDouble #-}

-- | Extract Bool from a Bool Value, or Nothing.
asBool :: Value -> Maybe Bool
asBool (Bool b) = Just b
asBool _        = Nothing
{-# INLINE asBool #-}

-- | Extract the array from an Array Value, or Nothing.
asArray :: Value -> Maybe [Value]
asArray (Array a) = Just a
asArray _         = Nothing
{-# INLINE asArray #-}

-- | Extract the object from an Object Value, or Nothing.
asObject :: Value -> Maybe (Map.Map Key Value)
asObject (Object o) = Just o
asObject _          = Nothing
{-# INLINE asObject #-}

-- ---------------------------------------------------------------------------
-- Char predicates (monomorphic, range-based — avoids Data.Char dictionaries)
-- ---------------------------------------------------------------------------

-- | Is the character a decimal digit (0-9)?
isDigit :: Char -> Bool
isDigit c = c >= '0' && c <= '9'
{-# INLINE isDigit #-}

-- | Is the character an ASCII letter?
isAlpha :: Char -> Bool
isAlpha c = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
{-# INLINE isAlpha #-}

-- | Is the character an ASCII letter or digit?
isAlphaNum :: Char -> Bool
isAlphaNum c = isAlpha c || isDigit c
{-# INLINE isAlphaNum #-}

-- | Is the character ASCII whitespace?
isSpace :: Char -> Bool
isSpace c = c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v'
{-# INLINE isSpace #-}

-- | Is the character an ASCII uppercase letter?
isUpper :: Char -> Bool
isUpper c = c >= 'A' && c <= 'Z'
{-# INLINE isUpper #-}

-- | Is the character an ASCII lowercase letter?
isLower :: Char -> Bool
isLower c = c >= 'a' && c <= 'z'
{-# INLINE isLower #-}

-- | Convert a digit character to its numeric value.
-- Returns -1 for non-digit characters (avoids pulling in error dictionaries).
digitToInt :: Char -> Int
digitToInt c
  | c >= '0' && c <= '9' = ord c - ord '0'
  | c >= 'a' && c <= 'f' = ord c - ord 'a' + 10
  | c >= 'A' && c <= 'F' = ord c - ord 'A' + 10
  | otherwise             = -1
{-# INLINE digitToInt #-}

-- | Convert an ASCII character to lowercase.
toLowerChar :: Char -> Char
toLowerChar c
  | c >= 'A' && c <= 'Z' = chr (ord c + 32)
  | otherwise             = c
{-# INLINE toLowerChar #-}

-- | Convert an ASCII character to uppercase.
toUpperChar :: Char -> Char
toUpperChar c
  | c >= 'a' && c <= 'z' = chr (ord c - 32)
  | otherwise             = c
{-# INLINE toUpperChar #-}

-- ---------------------------------------------------------------------------
-- Monomorphic numeric helpers (avoids Num/Ord dictionary issues)
-- ---------------------------------------------------------------------------

-- | Monomorphic absolute value for Int.
abs' :: Int -> Int
abs' n = if n < 0 then negate n else n
{-# INLINE abs' #-}

-- | Monomorphic signum for Int.
signum' :: Int -> Int
signum' n
  | n < 0     = -1
  | n == 0    = 0
  | otherwise = 1
{-# INLINE signum' #-}

-- | Monomorphic min for Int.
min' :: Int -> Int -> Int
min' a b = if a <= b then a else b
{-# INLINE min' #-}

-- | Monomorphic max for Int.
max' :: Int -> Int -> Int
max' a b = if a >= b then a else b
{-# INLINE max' #-}

-- ---------------------------------------------------------------------------
-- Indexed list operations (safe alternatives to [0..])
-- The JIT evaluates data constructor fields eagerly, so infinite lists
-- crash with SIGSEGV.  These helpers avoid infinite lists entirely.
-- ---------------------------------------------------------------------------

-- | Pair each element with its 0-based index.
-- @zipWithIndex ["a","b","c"] == [(0,"a"),(1,"b"),(2,"c")]@
zipWithIndex :: [a] -> [(Int, a)]
zipWithIndex = go 0
  where
    go _ []     = []
    go !i (x:xs) = (i, x) : go (i + 1) xs
{-# INLINE zipWithIndex #-}

-- | Map with 0-based index.
-- @imap (\i x -> (i, x)) ["a","b"] == [(0,"a"),(1,"b")]@
imap :: (Int -> a -> b) -> [a] -> [b]
imap f = go 0
  where
    go _ []     = []
    go !i (x:xs) = f i x : go (i + 1) xs
{-# INLINE imap #-}

-- | Monomorphic enumFromTo for Int. Finite range, no infinite lists.
-- @enumFromTo 0 4 == [0,1,2,3,4]@
enumFromTo :: Int -> Int -> [Int]
enumFromTo lo hi
  | lo > hi   = []
  | otherwise = lo : enumFromTo (lo + 1) hi
{-# INLINE enumFromTo #-}

-- ---------------------------------------------------------------------------
-- Additional list combinators (P2)
-- ---------------------------------------------------------------------------

-- | Index of the first element equal to the target.
elemIndex :: Eq a => a -> [a] -> Maybe Int
elemIndex x = go 0
  where
    go _ []     = Nothing
    go !i (y:ys)
      | x == y    = Just i
      | otherwise = go (i + 1) ys
{-# INLINABLE elemIndex #-}

-- | Index of the first element satisfying the predicate.
findIndex :: (a -> Bool) -> [a] -> Maybe Int
findIndex p = go 0
  where
    go _ []     = Nothing
    go !i (x:xs)
      | p x       = Just i
      | otherwise = go (i + 1) xs
{-# INLINE findIndex #-}

-- | Zip three lists.
zip3 :: [a] -> [b] -> [c] -> [(a, b, c)]
zip3 (a:as) (b:bs) (c:cs) = (a, b, c) : zip3 as bs cs
zip3 _ _ _ = []
{-# INLINE zip3 #-}

-- | Unzip a list of triples.
unzip3 :: [(a, b, c)] -> ([a], [b], [c])
unzip3 [] = ([], [], [])
unzip3 ((a,b,c):rest) = let (as, bs, cs) = unzip3 rest in (a:as, b:bs, c:cs)
{-# INLINE unzip3 #-}

-- | Tail-recursive filter (accumulator-based, avoids (:) in non-tail position).
filter :: (a -> Bool) -> [a] -> [a]
filter p = go []
  where
    go acc []     = reverse acc
    go acc (x:xs)
      | p x       = go (x : acc) xs
      | otherwise  = go acc xs
{-# INLINE filter #-}

-- | Tail-recursive nubBy (foldl'-style accumulator + linear scan).
nubBy :: (a -> a -> Bool) -> [a] -> [a]
nubBy _ [] = []
nubBy eq xs = go [] xs
  where
    go acc []     = reverse acc
    go acc (x:rest)
      | elemBy x acc = go acc rest
      | otherwise    = go (x : acc) rest
    elemBy _ []     = False
    elemBy x (y:ys)
      | eq x y      = True
      | otherwise    = elemBy x ys

-- | Tail-recursive nub (uses nubBy).
nub :: (Eq a) => [a] -> [a]
nub = nubBy (==)

-- ---------------------------------------------------------------------------
-- Map insertWith (local impl — avoids GHC's internal unfolding)
-- ---------------------------------------------------------------------------

-- | @insertWith f key new m@ — if @key@ exists with value @old@, store @f new old@;
-- otherwise insert @new@. Monomorphic on Text keys to avoid pulling in GHC's
-- internal Data.Map.Strict.insertWith which causes timeout under the JIT
-- (complex balance/rotation unfoldings + Ord dictionary re-evaluation).
-- Uses Map.lookup + Map.insert which are known working.
insertWith :: (a -> a -> a) -> Text -> a -> Map Text a -> Map Text a
insertWith f k v m = case Map.lookup k m of
  Just old -> let !combined = f v old in Map.insert k combined m
  Nothing  -> Map.insert k v m
{-# INLINE insertWith #-}

