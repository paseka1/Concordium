{-# LANGUAGE
    OverloadedStrings,
    ScopedTypeVariables,
    TypeFamilies,
    CPP,
    MonoLocalBinds #-}
module Concordium.Getters where

import Lens.Micro.Platform hiding ((.=))

import Concordium.Kontrol.BestBlock
import Concordium.Skov as Skov
import qualified Data.HashMap.Strict as HM

import Control.Monad.State.Class

import qualified Concordium.Scheduler.Types as AT
import Concordium.GlobalState.Types
import qualified Concordium.GlobalState.TreeState as TS
import Concordium.GlobalState.BlockPointer hiding (BlockPointer)
import Concordium.GlobalState.BlockMonads
import qualified Concordium.GlobalState.BlockState as BS
import qualified Concordium.GlobalState.Statistics as Stat
import qualified Concordium.GlobalState.Parameters as Parameters
import qualified Concordium.GlobalState.SeedState as SeedState
import Concordium.Types as T
import qualified Concordium.Wasm as Wasm
import Concordium.GlobalState.BakerInfo
import Concordium.GlobalState.Block hiding (PendingBlock)
import Concordium.Types.HashableTo
import Concordium.GlobalState.Instance
import Concordium.GlobalState.Finalization

import Concordium.Afgjort.Finalize(FinalizationStateLenses(..), FinalizationCurrentRound(..))
import Concordium.Afgjort.Finalize.Types
import Concordium.Kontrol (getFinalizationCommittee)

import Control.Concurrent.MVar
import Data.IORef
import Text.Read hiding (get, String)
import qualified Data.Map as Map
import Data.Aeson
import qualified Data.Text as T
import qualified Data.Set as S
import Data.String(fromString)
import Data.Word
import Data.Int
import Data.Vector (fromList)
import qualified Data.Vector as Vector
import Control.Monad
import Data.Foldable (foldrM)

class SkovQueryMonad m => SkovStateQueryable z m | z -> m where
    runStateQuery :: z -> m a -> IO a

instance (SkovConfiguration c, SkovQueryMonad (SkovT () c IO))
        => SkovStateQueryable (SkovContext c, IORef (SkovState c)) (SkovT () c IO) where
    runStateQuery (ctx, st) a = readIORef st >>= evalSkovT a () ctx

instance (SkovConfiguration c, SkovQueryMonad (SkovT () c IO))
        => SkovStateQueryable (SkovContext c, MVar (SkovState c)) (SkovT () c IO) where
    runStateQuery (ctx, st) a = readMVar st >>= evalSkovT a () ctx

hsh :: (HashableTo BlockHash a) => a -> String
hsh x = show (getHash x :: BlockHash)

getBestBlockState :: (BlockPointerMonad m, SkovQueryMonad m) => m (BlockState m)
getBestBlockState = queryBlockState =<< bestBlock

getLastFinalState :: SkovQueryMonad m => m (BlockState m)
getLastFinalState = queryBlockState =<< lastFinalizedBlock

getTransactionStatus :: SkovStateQueryable z m => AT.TransactionHash -> z -> IO Value
getTransactionStatus hash sfsRef = runStateQuery sfsRef $
  queryTransactionStatus hash >>= \case
    Nothing -> return Null
    Just AT.Received{} ->
      return $ object ["status" .= String "received"]
    Just AT.Finalized{..} ->
      withBlockStateJSON tsBlockHash $ \bs -> do
        outcome <- BS.getTransactionOutcome bs tsFinResult
        return $ object ["status" .= String "finalized",
                         "outcomes" .= object [fromString (show tsBlockHash) .= outcome]
                        ]
    Just AT.Committed{..} -> do
      outcomes <- forM (HM.toList tsResults) $ \(bh, idx) ->
        resolveBlock bh >>= \case
          Nothing -> return (T.pack (show bh) .= Null) -- should not happen
          Just bp -> do
            outcome <- flip BS.getTransactionOutcome idx =<< queryBlockState bp
            return (T.pack (show bh) .= outcome)
      return $ object ["status" .= String "committed",
                       "outcomes" .= object outcomes
                      ]

getTransactionStatusInBlock :: SkovStateQueryable z m => AT.TransactionHash -> BlockHash -> z -> IO Value
getTransactionStatusInBlock txHash blockHash sfsRef = runStateQuery sfsRef $
  queryTransactionStatus txHash >>= \case
    Nothing -> return Null
    Just AT.Received{} ->
      return $ object ["status" .= String "received"]
    Just AT.Finalized{..} ->
      if tsBlockHash == blockHash then
        withBlockStateJSON tsBlockHash $ \bs -> do
          outcome <- BS.getTransactionOutcome bs tsFinResult
          return $ object ["status" .= String "finalized",
                           "result" .= outcome
                          ]
      else
        return Null
    Just AT.Committed{..} ->
      case HM.lookup blockHash tsResults of
        Nothing -> return Null
        Just idx ->
          withBlockStateJSON blockHash $ \bs -> do
            outcome <- BS.getTransactionOutcome bs idx
            return $ object ["status" .= String "committed",
                             "result" .= outcome
                            ]

getAccountNonFinalizedTransactions :: SkovStateQueryable z m => AccountAddress -> z -> IO [TransactionHash]
getAccountNonFinalizedTransactions addr sfsRef = runStateQuery sfsRef $
    queryNonFinalizedTransactions addr

-- |Return the best guess as to what the next account nonce should be.
-- If all account transactions are finalized then this information is reliable.
-- Otherwise this is the best guess, assuming all other transactions will be
-- committed to blocks and eventually finalized.
-- The 'Bool' indicates whether all transactions are finalized.
getNextAccountNonce :: SkovStateQueryable z m => AccountAddress -> z -> IO Value
getNextAccountNonce addr sfsRef = runStateQuery sfsRef $ do
    (nonce, allFinal) <- (queryNextAccountNonce addr)
    return $ object ["nonce" .= nonce,
                     "allFinal" .= allFinal
                    ]

-- |Return a block with given hash and outcomes.
getBlockSummary :: (SkovStateQueryable z m) => BlockHash -> z -> IO Value
getBlockSummary hash sfsRef = runStateQuery sfsRef $
  resolveBlock hash >>= \case
    Nothing -> return Null
    Just bp -> do
      bs <- queryBlockState bp
      outcomes <- BS.getOutcomes bs
      specialOutcomes <- BS.getSpecialOutcomes bs
      let finData = blockFinalizationData <$> blockFields bp
      finDataJSON <-
            case finData of
              Just (BlockFinalizationData FinalizationRecord{..}) -> do
                  -- Get the finalization committee by examining the previous finalized block
                  finalizers <- blockAtFinIndex (finalizationIndex - 1) >>= \case
                      Nothing -> return Vector.empty -- This should not be possible
                      Just prevFin -> do
                          com <- getFinalizationCommittee prevFin
                          let signers = S.fromList (finalizationProofParties finalizationProof)
                          let fromPartyInfo i PartyInfo{..} = object [
                                "bakerId" .= partyBakerId,
                                "weight" .= (fromIntegral partyWeight :: Integer),
                                "signed" .= S.member (fromIntegral i) signers
                                ]
                          return (Vector.imap fromPartyInfo (parties com))
                  return $ object [
                    "finalizationBlockPointer" .= finalizationBlockPointer,
                    "finalizationIndex" .= finalizationIndex,
                    "finalizationDelay" .= finalizationDelay,
                    "finalizers" .= finalizers
                    ]
              _ -> return Null

      return $ object [
        "transactionSummaries" .= outcomes,
        "specialEvents" .= specialOutcomes,
        "finalizationData" .= finDataJSON
        ]

withBlockState :: SkovQueryMonad m => BlockHash -> (BlockState m -> m a) -> m (Maybe a)
withBlockState hash f =
  resolveBlock hash >>=
    \case Nothing -> return Nothing
          Just bp -> fmap Just . f =<< queryBlockState bp

withBlockStateJSON :: SkovQueryMonad m => BlockHash -> (BlockState m -> m Value) -> m Value
withBlockStateJSON hash f =
  resolveBlock hash >>=
    \case Nothing -> return Null
          Just bp -> f =<< queryBlockState bp

getAccountList :: SkovStateQueryable z m => BlockHash -> z -> IO Value
getAccountList hash sfsRef = runStateQuery sfsRef $
  withBlockStateJSON hash $ \st -> do
  alist <- BS.getAccountList st
  return . toJSON $ alist  -- show instance for account addresses is based on Base58 encoding

getInstances :: (SkovStateQueryable z m) => BlockHash -> z -> IO Value
getInstances hash sfsRef = runStateQuery sfsRef $
  withBlockStateJSON hash $ \st -> do
  ilist <- BS.getContractInstanceList st
  return $ toJSON (map iaddress ilist)

getAccountInfo :: (SkovStateQueryable z m) => BlockHash -> z -> AccountAddress -> IO Value
getAccountInfo hash sfsRef addr = runStateQuery sfsRef $
  withBlockStateJSON hash $ \st ->
  BS.getAccount st addr >>=
      \case Nothing -> return Null
            Just acc -> do
              Nonce nonce <- BS.getAccountNonce acc
              amount <- BS.getAccountAmount acc
              creds <- BS.getAccountCredentials acc
              delegate <- BS.getAccountStakeDelegate acc
              instances <- BS.getAccountInstances acc
              return $ object ["accountNonce" .= nonce
                                        ,"accountAmount" .= toInteger amount
                                        -- credentials, most recent first
                                        ,"accountCredentials" .= creds
                                        ,"accountDelegation" .= delegate
                                        ,"accountInstances" .= S.toList instances
                                        ]

getContractInfo :: (SkovStateQueryable z m) => BlockHash -> z -> AT.ContractAddress -> IO Value
getContractInfo hash sfsRef addr = runStateQuery sfsRef $
  withBlockStateJSON hash $ \st ->
  BS.getContractInstance st addr >>=
      \case Nothing -> return Null
            Just istance -> let params = instanceParameters istance
                            in return $ object ["model" .= instanceModel istance
                                               ,"owner" .= instanceOwner params
                                               ,"amount" .= instanceAmount istance]

getRewardStatus :: (SkovStateQueryable z m) => BlockHash -> z -> IO Value
getRewardStatus hash sfsRef = runStateQuery sfsRef $
  withBlockStateJSON hash $ \st -> do
  reward <- BS.getRewardStatus st
  return $ object [
    "totalAmount" .= (fromIntegral (reward ^. AT.totalGTU) :: Integer),
    "totalEncryptedAmount" .= (fromIntegral (reward ^. AT.totalEncryptedGTU) :: Integer),
    "centralBankAmount" .= (fromIntegral (reward ^. AT.centralBankGTU) :: Integer),
    "mintedAmountPerSlot" .= (fromIntegral (reward ^. AT.mintedGTUPerSlot) :: Integer)
    ]

getBlockBirkParameters :: (SkovStateQueryable z m) => BlockHash -> z -> IO Value
getBlockBirkParameters hash sfsRef = runStateQuery sfsRef $
  withBlockStateJSON hash $ \st -> do
  bps <- BS.getBlockBirkParameters st
  elDiff <- BS.getElectionDifficulty bps
  nonce <- BS.birkLeadershipElectionNonce bps
  lotteryBakers <- BS.getLotteryBakers bps
  fullBakerInfos <- BS.getFullBakerInfos lotteryBakers
  totalStake <- BS.getTotalBakerStake lotteryBakers
  return $ object [
    "electionDifficulty" .= elDiff,
    "electionNonce" .= nonce,
    "bakers" .= Array (fromList .
                       map (\(bid, FullBakerInfo{_bakerInfo = BakerInfo{..}, ..}) -> object ["bakerId" .= toInteger bid
                                                            ,"bakerAccount" .= show _bakerAccount
                                                            ,"bakerLotteryPower" .= ((fromIntegral _bakerStake :: Double) / fromIntegral totalStake)
                                                            ]) .
                       Map.toList $ fullBakerInfos)
    ]

getModuleList :: (SkovStateQueryable z m) => BlockHash -> z -> IO Value
getModuleList hash sfsRef = runStateQuery sfsRef $
  withBlockStateJSON hash $ \st -> do
  mlist <- BS.getModuleList st
  return . toJSON . map show $ mlist -- show instance of ModuleRef displays it in Base16


-- FIXME: This should not return an instrumented module, but rather the module as deployed.
getModuleSource :: (SkovStateQueryable z m) => BlockHash -> z -> ModuleRef -> IO (Maybe Wasm.WasmModule)
getModuleSource hash sfsRef mhash = runStateQuery sfsRef $
  resolveBlock hash >>=
    \case Nothing -> return Nothing
          Just bp -> do
            st <- queryBlockState bp
            BS.getModule st mhash >>= \case
              Nothing -> return Nothing
              Just modul -> return . Just $
                  let iModule = Wasm.miModule . BS.moduleInterface $ modul
                  in Wasm.WasmModule (Wasm.imWasmVersion iModule) (Wasm.imWasmSource iModule)

getConsensusStatus :: (SkovStateQueryable z m, TS.TreeStateMonad m) => z -> IO Value
getConsensusStatus sfsRef = runStateQuery sfsRef $ do
        bb <- bestBlock
        lfb <- lastFinalizedBlock
        genesis <- genesisBlock
        stats <- TS.getConsensusStatistics
        genData <- TS.getGenesisData
        let -- for now we'll use the genesis epoch length even though that is a bit less
            -- than optimal with respect to future changes.
            -- When all of these parameters are dynamic we need to revisit.
            slotDuration = Parameters.genesisSlotDuration genData
            epochDuration = fromIntegral (SeedState.epochLength (Parameters.genesisSeedState genData)) * slotDuration
        return $ object [
                "bestBlock" .= hsh bb,
                "genesisBlock" .= hsh genesis,
                -- time of the genesis block as UTC time (accurate to 1s)
                "genesisTime" .= timestampToUTCTime (Parameters.genesisTime genData),
                -- duration of a slot in milliseconds
                "slotDuration" .= durationMillis slotDuration,
                -- duration of an epoch in milliseconds
                "epochDuration" .= durationMillis epochDuration,
                "lastFinalizedBlock" .= hsh lfb,
                "bestBlockHeight" .= theBlockHeight (bpHeight bb),
                "lastFinalizedBlockHeight" .= theBlockHeight (bpHeight lfb),
                "blocksReceivedCount" .= (stats ^. Stat.blocksReceivedCount),
                "blockLastReceivedTime" .= (stats ^. Stat.blockLastReceived),
                "blockReceiveLatencyEMA" .= (stats ^. Stat.blockReceiveLatencyEMA),
                "blockReceiveLatencyEMSD" .= sqrt (stats ^. Stat.blockReceiveLatencyEMVar),
                "blockReceivePeriodEMA" .= (stats ^. Stat.blockReceivePeriodEMA),
                "blockReceivePeriodEMSD" .= (sqrt <$> (stats ^. Stat.blockReceivePeriodEMVar)),
                "blocksVerifiedCount" .= (stats ^. Stat.blocksVerifiedCount),
                "blockLastArrivedTime" .= (stats ^. Stat.blockLastArrive),
                "blockArriveLatencyEMA" .= (stats ^. Stat.blockArriveLatencyEMA),
                "blockArriveLatencyEMSD" .= sqrt (stats ^. Stat.blockArriveLatencyEMVar),
                "blockArrivePeriodEMA" .= (stats ^. Stat.blockArrivePeriodEMA),
                "blockArrivePeriodEMSD" .= (sqrt <$> (stats ^. Stat.blockArrivePeriodEMVar)),
                "transactionsPerBlockEMA" .= (stats ^. Stat.transactionsPerBlockEMA),
                "transactionsPerBlockEMSD" .= sqrt (stats ^. Stat.transactionsPerBlockEMVar),
                "finalizationCount" .= (stats ^. Stat.finalizationCount),
                "lastFinalizedTime" .= (stats ^. Stat.lastFinalizedTime),
                "finalizationPeriodEMA" .= (stats ^. Stat.finalizationPeriodEMA),
                "finalizationPeriodEMSD" .= (sqrt <$> (stats ^. Stat.finalizationPeriodEMVar))
            ]

getBlockInfo :: (SkovStateQueryable z m, BlockPointerMonad m, HashableTo BlockHash (BlockPointerType m)) => z -> String -> IO Value
getBlockInfo sfsRef blockHash = case readMaybe blockHash of
        Nothing -> return Null
        Just bh -> runStateQuery sfsRef $
                resolveBlock bh >>= \case
                    Nothing -> return Null
                    Just bp -> do
                        let slot = blockSlot bp
                        slotTime <- getSlotTime slot
                        bfin <- isFinalized bh
                        parent <- bpParent bp
                        lfin <- bpLastFinalized bp
                        return $ object [
                            "blockHash" .= hsh bp,
                            "blockParent" .= hsh parent,
                            "blockLastFinalized" .= hsh lfin,
                            "blockHeight" .= theBlockHeight (bpHeight bp),
                            "blockReceiveTime" .= bpReceiveTime bp,
                            "blockArriveTime" .= bpArriveTime bp,
                            "blockSlot" .= (fromIntegral slot :: Word64),
                            "blockSlotTime" .= slotTime,
                            "blockBaker" .= case blockFields bp of
                                            Nothing -> Null
                                            Just bf -> toJSON (toInteger (blockBaker bf)),
                            "finalized" .= bfin,
                            "transactionCount" .= bpTransactionCount bp,
                            "transactionEnergyCost" .= toInteger (bpTransactionsEnergyCost bp),
                            "transactionsSize" .= toInteger (bpTransactionsSize bp)
                            ]

getBlocksAtHeight :: (SkovStateQueryable z m, HashableTo BlockHash (BlockPointerType m))
    => z -> BlockHeight -> IO Value
getBlocksAtHeight sfsRef height = runStateQuery sfsRef $
    toJSONList . map hsh <$> Skov.getBlocksAtHeight height

getAncestors :: (SkovStateQueryable z m, BlockPointerMonad m, HashableTo BlockHash (BlockPointerType m))
             => z -> String -> BlockHeight -> IO Value
getAncestors sfsRef blockHash count = case readMaybe blockHash of
        Nothing -> return Null
        Just bh -> runStateQuery sfsRef $
                resolveBlock bh >>= \case
                    Nothing -> return Null
                    Just bp -> do
                      parents <- iterateForM bpParent (fromIntegral $ min count (1 + bpHeight bp)) bp
                      return $ toJSONList $ map hsh parents
   where
     iterateForM :: (Monad m) => (a -> m a) -> Int -> a -> m [a]
     iterateForM f steps initial = reverse <$> (go [] steps initial)
       where go acc n a | n <= 0 = return acc
                        | otherwise = do
                         a' <- f a
                         go (a:acc) (n-1) a'

getBranches :: forall z m. (SkovStateQueryable z m, TS.TreeStateMonad m)
            => z -> IO Value
getBranches sfsRef = runStateQuery sfsRef $ do
            brs <- branchesFromTop :: m [[BlockPointerType m]]
            brt <- foldM up Map.empty brs :: m (Map.Map (BlockPointerType m) [Value])
            lastFin <- lastFinalizedBlock :: m (BlockPointerType m)
            return $ object ["blockHash" .= hsh lastFin, "children" .= Map.findWithDefault [] lastFin brt]
    where
        up :: Map.Map (BlockPointerType m) [Value] -> [BlockPointerType m] -> m (Map.Map (BlockPointerType m) [Value])
        up childrenMap = foldrM (\(b :: BlockPointerType m) (ma :: Map.Map (BlockPointerType m) [Value]) -> do
                                    parent <- bpParent b :: m (BlockPointerType m)
                                    return $ (at parent . non [] %~ (object ["blockHash" .= hsh b, "children" .= (Map.findWithDefault [] b childrenMap :: [Value])] :)) ma) Map.empty

getBlockFinalization :: (SkovStateQueryable z m, TS.TreeStateMonad m)
                     => z -> BlockHash -> IO (Maybe FinalizationRecord)
getBlockFinalization sfsRef bh = runStateQuery sfsRef $ do
            bs <- TS.getBlockStatus bh
            case bs of
                Just (TS.BlockFinalized _ fr) -> return $ Just fr
                _ -> return Nothing

-- |Check whether a keypair is part of the baking committee by a key pair in the current best block.
-- Returns -1 if keypair is not added as a baker.
-- Returns -2 if keypair is added as a baker, but not part of the baking committee yet.
-- Returns >= 0 if keypair is part of the baking committee. In this case the return value
-- is the baker id as appearing in blocks.
-- NB: this function will not work correctly when there are more than 2^63-1 bakers.
bakerIdBestBlock :: (BlockPointerMonad m, SkovStateQueryable z m)
    => BakerSignVerifyKey
    -> z
    -> IO Int64
bakerIdBestBlock key sfsRef = runStateQuery sfsRef $ do
  bb <- bestBlock
  bps <- BS.getBlockBirkParameters =<< queryBlockState bb
  lotteryBakers <- BS.getLotteryBakers bps
  currentBakers <- BS.getCurrentBakers bps
  mlbid <- BS.getBakerFromKey lotteryBakers key
  mcbid <- BS.getBakerFromKey currentBakers key
  case mlbid of
    Just bid -> return (fromIntegral bid)
    Nothing ->
      case mcbid of
        Just _ -> return (-2)
        Nothing -> return (-1)

-- |Check whether the node is currently a member of the finalization committee.
checkIsCurrentFinalizer :: (SkovStateQueryable z m, MonadState s m, FinalizationStateLenses s t) => z -> IO Bool
checkIsCurrentFinalizer sfsRef = runStateQuery sfsRef $ do
   fs <- use finState
   case fs ^. finCurrentRound of
     PassiveCurrentRound _ -> return False
     ActiveCurrentRound _ -> return True

getAllIdentityProviders :: (SkovStateQueryable z m) => BlockHash -> z -> IO Value
getAllIdentityProviders hash sfsRef = runStateQuery sfsRef $
  withBlockStateJSON hash $ \st -> toJSON <$> BS.getAllIdentityProviders st

getAllAnonymityRevokers :: (SkovStateQueryable z m) => BlockHash -> z -> IO Value
getAllAnonymityRevokers hash sfsRef = runStateQuery sfsRef $
  withBlockStateJSON hash $ \st -> toJSON <$> BS.getAllAnonymityRevokers st
