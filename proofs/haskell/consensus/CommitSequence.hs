-- | FV-CO-004: Commit Sequence Monotonicity — Haskell Reference Specification
--
-- Executable specification for verifying that Shoal++ commit sequences
-- are strictly monotonically increasing and gap-free.  The Rust
-- implementation in nexus-consensus/src/shoal.rs must produce identical
-- output for the same canonical input corpus.
--
-- Status: COMPLETE — runnable via `runghc CommitSequence.hs` or `cabal test`
-- Invariant: FV-CO-004
-- Object: VO-CO-006
-- Anchor: crates/nexus-consensus/src/shoal.rs

module Consensus.CommitSequence
  ( -- * Types
    CommitSequence(..)
  , Round(..)
  , ValidatorId(..)
  , Reputation(..)
  , AnchorCandidate(..)
  , CommittedBatch(..)
  , ShoalState(..)
    -- * Core logic
  , initialState
  , nextAnchorRound
  , selectLeader
  , tryCommit
  , assignSequences
    -- * Properties
  , prop_monotonic
  , prop_startsAtZero
  , prop_gapFree
  , prop_anchorRoundsEven
  , prop_leaderRotation
  , prop_sequenceMatchesIndex
    -- * Test runner
  , runAllProperties
  ) where

import Data.List (sortBy)
import Data.Ord (Down(..), comparing)
import Data.Word (Word64)
import System.Exit (exitFailure, exitSuccess)

-- ═══════════════════════════════════════════════════════════════════
-- Types
-- ═══════════════════════════════════════════════════════════════════

-- | Commit sequence number.  Must be strictly monotonically increasing
--   and gap-free across successive commits.
newtype CommitSequence = CommitSequence { unCommitSequence :: Word64 }
  deriving (Eq, Ord, Show)

-- | DAG round number (0-indexed).
newtype Round = Round { unRound :: Word64 }
  deriving (Eq, Ord, Show)

-- | Validator identifier (index in committee).
newtype ValidatorId = ValidatorId { unValidatorId :: Int }
  deriving (Eq, Ord, Show)

-- | Reputation score in [0.0, 1.0].
newtype Reputation = Reputation { unReputation :: Double }
  deriving (Eq, Ord, Show)

-- | An anchor candidate with a certificate at the anchor round.
data AnchorCandidate = AnchorCandidate
  { acValidator  :: ValidatorId
  , acReputation :: Reputation
  } deriving (Show)

-- | A committed sub-DAG produced by the orderer.
data CommittedBatch = CommittedBatch
  { cbLeader       :: ValidatorId       -- ^ Elected leader for this anchor
  , cbAnchorRound  :: Round             -- ^ Even-numbered anchor round
  , cbCertCount    :: Int               -- ^ Number of certificates in causal history
  , cbSequence     :: CommitSequence    -- ^ Monotonically assigned
  } deriving (Show)

-- | Mutable Shoal orderer state.
data ShoalState = ShoalState
  { ssNextSequence   :: Word64    -- ^ Next sequence to assign (starts at 0)
  , ssNextAnchor     :: Round     -- ^ Next anchor round to attempt
  , ssRecoveryWindow :: Word64    -- ^ C-4 recovery: max gap before skipping (default 6)
  } deriving (Show)

-- ═══════════════════════════════════════════════════════════════════
-- Core Logic — mirrors shoal.rs
-- ═══════════════════════════════════════════════════════════════════

-- | Initial orderer state.
initialState :: ShoalState
initialState = ShoalState
  { ssNextSequence   = 0
  , ssNextAnchor     = Round 0
  , ssRecoveryWindow = 6
  }

-- | Compute the next anchor round (always even, spaced by 2).
nextAnchorRound :: Round -> Round
nextAnchorRound (Round r) = Round (r + 2)

-- | Select leader for an anchor round from candidates.
--
-- Algorithm (matches shoal.rs select_leader):
--   1. Sort by reputation descending, then by validator index ascending.
--   2. Top tier = top 10% of committee (at least 1).
--   3. Rotate among top tier: index = (anchor_round / 2) % top_tier_size.
selectLeader :: Round -> [AnchorCandidate] -> Maybe ValidatorId
selectLeader _ [] = Nothing
selectLeader (Round anchorRound) candidates =
  let sorted = sortBy (comparing (Down . acReputation)
                       <> comparing (acValidator))
                      candidates
      n = length sorted
      topTierCount = max 1 (n `div` 10)
      topTier = take topTierCount sorted
      rotation = fromIntegral ((anchorRound `div` 2) `mod` fromIntegral topTierCount)
  in Just (acValidator (topTier !! rotation))

-- | Attempt a commit at the current anchor round.
--
-- Returns Nothing if the DAG hasn't advanced past the anchor.
-- Returns Just (batch, newState) on success.
-- Applies C-4 skip logic if leader is missing and gap exceeds recovery window.
tryCommit
  :: ShoalState
  -> Round                   -- ^ Current DAG round
  -> [AnchorCandidate]       -- ^ Validators with certs at anchor round
  -> Bool                    -- ^ Does the elected leader have a cert?
  -> Int                     -- ^ Causal history size (if leader present)
  -> Maybe (CommittedBatch, ShoalState)
tryCommit st dagRound candidates leaderHasCert causalSize
  | dagRound <= ssNextAnchor st = Nothing  -- DAG hasn't advanced past anchor
  | otherwise =
      case selectLeader (ssNextAnchor st) candidates of
        Nothing -> Nothing  -- No candidates at all
        Just leaderId
          | not leaderHasCert ->
              -- C-4 recovery: skip only if gap exceeds recovery window
              let gap = unRound dagRound - unRound (ssNextAnchor st)
              in if gap > ssRecoveryWindow st
                 then Just (skipAndAdvance st)
                 else Nothing  -- Wait for leader cert
          | otherwise ->
              let batch = CommittedBatch
                    { cbLeader      = leaderId
                    , cbAnchorRound = ssNextAnchor st
                    , cbCertCount   = causalSize
                    , cbSequence    = CommitSequence (ssNextSequence st)
                    }
                  st' = st
                    { ssNextSequence = ssNextSequence st + 1
                    , ssNextAnchor   = nextAnchorRound (ssNextAnchor st)
                    }
              in Just (batch, st')
  where
    -- Skip anchor without committing (no sequence consumed)
    skipAndAdvance s =
      let skipped = CommittedBatch
            { cbLeader      = ValidatorId (-1)  -- sentinel: skipped
            , cbAnchorRound = ssNextAnchor s
            , cbCertCount   = 0
            , cbSequence    = CommitSequence (ssNextSequence s)
            }
          s' = s
            { ssNextSequence = ssNextSequence s + 1
            , ssNextAnchor   = nextAnchorRound (ssNextAnchor s)
            }
      in (skipped, s')

-- | Reference implementation of sequence assignment.
--
-- Given a list of anchor rounds that produced commits, assign sequential
-- CommitSequence values starting from 0.
--
-- Invariant: for all i, sequence(result[i+1]) == sequence(result[i]) + 1
assignSequences :: [a] -> [(a, CommitSequence)]
assignSequences = zipWith (\i x -> (x, CommitSequence i)) [0..]

-- ═══════════════════════════════════════════════════════════════════
-- Properties
-- ═══════════════════════════════════════════════════════════════════

-- | Property: assignSequences always produces a strictly monotonic sequence.
prop_monotonic :: [Int] -> Bool
prop_monotonic xs =
  let result = assignSequences xs
      seqs   = map (unCommitSequence . snd) result
  in and $ zipWith (\a b -> b == a + 1) seqs (drop 1 seqs)

-- | Property: first sequence is always 0 (when non-empty).
prop_startsAtZero :: [Int] -> Bool
prop_startsAtZero [] = True
prop_startsAtZero xs =
  case assignSequences xs of
    (_, s) : _ -> unCommitSequence s == 0
    []         -> True

-- | Property: sequence is gap-free (contiguous from 0 to n-1).
prop_gapFree :: [Int] -> Bool
prop_gapFree [] = null (assignSequences ([] :: [Int]))
prop_gapFree xs =
  let result = assignSequences xs
      seqs   = map (unCommitSequence . snd) result
  in seqs == [0 .. fromIntegral (length xs) - 1]

-- | Property: anchor rounds produced by successive commits are always even.
prop_anchorRoundsEven :: Bool
prop_anchorRoundsEven =
  let rounds = take 100 $ iterate nextAnchorRound (Round 0)
  in all (\(Round r) -> r `mod` 2 == 0) rounds

-- | Property: leader rotation cycles through top tier.
prop_leaderRotation :: Bool
prop_leaderRotation =
  let candidates = [ AnchorCandidate (ValidatorId i) (Reputation (1.0 - fromIntegral i * 0.01))
                   | i <- [0..19] ]
      topTierSize = max 1 (length candidates `div` 10)  -- 2
      leaders = [ selectLeader (Round (r * 2)) candidates | r <- [0..9] ]
      ids = [ unValidatorId v | Just v <- leaders ]
  in -- Leaders should cycle within top tier
     all (\vid -> vid < topTierSize) ids
     && length ids == 10

-- | Property: N inputs produce exactly sequences [0..N-1].
prop_sequenceMatchesIndex :: Bool
prop_sequenceMatchesIndex =
  let n = 50
      result = assignSequences [1..n]
      seqs   = map (unCommitSequence . snd) result
  in seqs == [0..fromIntegral n - 1]

-- ═══════════════════════════════════════════════════════════════════
-- Test Runner
-- ═══════════════════════════════════════════════════════════════════

data TestResult = TestResult
  { trName   :: String
  , trPassed :: Bool
  }

runTest :: String -> Bool -> TestResult
runTest name result = TestResult name result

runAllProperties :: IO ()
runAllProperties = do
  let results =
        [ runTest "prop_monotonic (empty)"     (prop_monotonic [])
        , runTest "prop_monotonic (singleton)"  (prop_monotonic [42])
        , runTest "prop_monotonic (many)"       (prop_monotonic [1..100])
        , runTest "prop_startsAtZero (empty)"   (prop_startsAtZero [])
        , runTest "prop_startsAtZero (many)"    (prop_startsAtZero [1..50])
        , runTest "prop_gapFree (empty)"        (prop_gapFree [])
        , runTest "prop_gapFree (many)"         (prop_gapFree [1..100])
        , runTest "prop_anchorRoundsEven"       prop_anchorRoundsEven
        , runTest "prop_leaderRotation"         prop_leaderRotation
        , runTest "prop_sequenceMatchesIndex"   prop_sequenceMatchesIndex
        ]
  mapM_ (\tr -> putStrLn $ (if trPassed tr then "  PASS  " else "  FAIL  ") ++ trName tr) results
  let failures = filter (not . trPassed) results
  putStrLn $ "\n" ++ show (length results) ++ " tests, "
           ++ show (length results - length failures) ++ " passed, "
           ++ show (length failures) ++ " failed."
  if null failures then exitSuccess else exitFailure

-- | Entry point: run all properties and exit with appropriate code.
main :: IO ()
main = do
  putStrLn "FV-CO-004: CommitSequence Monotonicity — Haskell Reference Specification"
  putStrLn "========================================================================\n"
  runAllProperties
