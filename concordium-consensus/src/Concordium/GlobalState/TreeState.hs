{-# LANGUAGE TemplateHaskell #-}
{-# LANGUAGE RecordWildCards #-}
module Concordium.GlobalState.TreeState where

import qualified Data.Sequence as Seq
import Lens.Micro.Platform
import Data.Time
import Data.Time.Clock.POSIX
import Data.List
import Data.Maybe
import qualified Data.Set as Set
import Control.Monad.Trans.Class
import Control.Monad.Trans.Maybe
import Control.Monad.Trans.RWS

import Concordium.GlobalState.Types
import Concordium.GlobalState.Block
import Concordium.GlobalState.BlockState
import Concordium.GlobalState.Finalization
import Concordium.GlobalState.Parameters
import Concordium.GlobalState.Transactions


data ConsensusStatistics = ConsensusStatistics {
    _blocksReceivedCount :: Int,
    _blocksVerifiedCount :: Int,
    _blockLastReceived :: Maybe UTCTime,
    _blockReceiveLatencyEMA :: Double,
    _blockReceiveLatencyEMVar :: Double,
    _blockReceivePeriodEMA :: Maybe Double,
    _blockReceivePeriodEMVar :: Maybe Double,
    _blockLastArrive :: Maybe UTCTime,
    _blockArriveLatencyEMA :: Double,
    _blockArriveLatencyEMVar :: Double,
    _blockArrivePeriodEMA :: Maybe Double,
    _blockArrivePeriodEMVar :: Maybe Double,
    _transactionsPerBlockEMA :: Double,
    _transactionsPerBlockEMVar :: Double,
    _finalizationCount :: Int,
    _lastFinalizedTime :: Maybe UTCTime,
    _finalizationPeriodEMA :: Maybe Double,
    _finalizationPeriodEMVar :: Maybe Double
}
makeLenses ''ConsensusStatistics

instance Show ConsensusStatistics where
    show ConsensusStatistics{..} = intercalate "," $ [show (fromMaybe 0 (realToFrac . utcTimeToPOSIXSeconds <$> (_blockLastArrive)) :: Double), show _blockArriveLatencyEMA, show _blockArriveLatencyEMVar, show _blockArrivePeriodEMA, show _blockArrivePeriodEMVar] ++
                                                [show (fromMaybe 0 (realToFrac . utcTimeToPOSIXSeconds <$> (_blockLastReceived)) :: Double), show _blockReceiveLatencyEMA, show _blockReceiveLatencyEMVar, show _blockReceivePeriodEMA, show _blockReceivePeriodEMVar]


initialConsensusStatistics :: ConsensusStatistics
initialConsensusStatistics = ConsensusStatistics {
    _blocksReceivedCount = 0,
    _blocksVerifiedCount = 0,
    _blockLastReceived = Nothing,
    _blockReceiveLatencyEMA = 0,
    _blockReceiveLatencyEMVar = 0,
    _blockReceivePeriodEMA = Nothing,
    _blockReceivePeriodEMVar = Nothing,
    _blockLastArrive = Nothing,
    _blockArriveLatencyEMA = 0,
    _blockArriveLatencyEMVar = 0,
    _blockArrivePeriodEMA = Nothing,
    _blockArrivePeriodEMVar = Nothing,
    _transactionsPerBlockEMA = 0,
    _transactionsPerBlockEMVar = 0,
    _finalizationCount = 0,
    _lastFinalizedTime = Nothing,
    _finalizationPeriodEMA = Nothing,
    _finalizationPeriodEMVar = Nothing
}


data BlockStatus =
    BlockAlive !BlockPointer
    | BlockDead
    | BlockFinalized !BlockPointer !FinalizationRecord
    deriving (Eq)
instance Show BlockStatus where
    show (BlockAlive _) = "Alive"
    show (BlockDead) = "Dead"
    show (BlockFinalized _ _) = "Finalized"

-- |Branches of a tree represented as a sequence, ordered by height above the last
-- finalized block, of lists of block pointers.  The blocks in the branches should
-- be exactly the live blocks.  If a block is in the branches, then either it is at
-- the lowest level and its parent is the last finalized block, or its parent is also
-- in the branches at the level below.
type Branches = Seq.Seq [BlockPointer]

-- |Monad that provides operations for working with the low-level tree state.
-- These operations are abstracted where possible to allow for a range of implementation
-- choices.
class Monad m => TreeStateMonad m where
    -- * Operations on the block table
    -- |Get the current status of a block.
    getBlockStatus :: BlockHash -> m (Maybe BlockStatus)
    -- |Make a live 'BlockPointer' from a 'PendingBlock'.
    -- The parent and last finalized pointers must be correct.
    makeLiveBlock ::
        PendingBlock        -- ^Block to make live
        -> BlockPointer     -- ^Parent block pointer
        -> BlockPointer     -- ^Last finalized block pointer
        -> BlockState       -- ^Block state
        -> UTCTime          -- ^Block arrival time
        -> m BlockPointer
    -- |Mark a block as dead.
    markDead :: BlockHash -> m ()
    -- |Mark a block as finalized (by a particular 'FinalizationRecord').
    --
    -- Precondition: The block must be alive.
    markFinalized :: BlockHash -> FinalizationRecord -> m ()
    -- * Queries on genesis block
    -- |Get the genesis 'BlockPointer'.
    getGenesisBlockPointer :: m BlockPointer
    -- |Get the 'GenesisData'.
    getGenesisData :: m GenesisData
    -- * Operations on the finalization list
    -- |Get the last finalized block.
    getLastFinalized :: m BlockPointer
    -- |Get the slot number of the last finalized block
    getLastFinalizedSlot :: m Slot
    getLastFinalizedSlot = blockSlot <$> getLastFinalized
    -- |Get the height of the last finalized block
    getLastFinalizedHeight :: m BlockHeight
    getLastFinalizedHeight = bpHeight <$> getLastFinalized
    -- |Get the next finalization index.
    getNextFinalizationIndex :: m FinalizationIndex
    -- |Add a block and finalization record to the finalization list.
    -- The block must be the one finalized by the record, and the finalization
    -- index must be the next finalization index.  These are not checked.
    addFinalization :: BlockPointer -> FinalizationRecord -> m ()
    -- * Operations on branches
    -- |Get the branches.
    getBranches :: m Branches
    -- |Set the branches.
    putBranches :: Branches -> m ()
    -- * Operations on blocks that are pending the arrival of other blocks
    --
    -- $pendingBlocks
    -- Pending blocks are conceptually stored in a min priority search queue,
    -- where multiple blocks may have the same key, which is their parent,
    -- and the priority is the block's slot number.
    -- When a block arrives (possibly dead), its pending children are removed
    -- from the queue and handled.  This uses 'takePendingChildren'.
    -- When a block is finalized, all pending blocks with a lower or equal slot
    -- number can be handled (they will become dead, since they can no longer
    -- join the tree).  This uses 'takeNextPendingUntil'.
    -- 
    -- |Return a list of the blocks that are pending the given parent block,
    -- removing them from the pending table.
    takePendingChildren :: BlockHash -> m [PendingBlock]
    -- |Add a pending block, that is pending on the arrival of its parent.
    addPendingBlock :: PendingBlock -> m ()
    -- |Return the next block that is pending its parent with slot number
    -- less than or equal to the given value, removing it from the pending
    -- table.  Returns 'Nothing' if there is no such pending block.
    takeNextPendingUntil :: Slot -> m (Maybe PendingBlock)
    -- * Operations on blocks that are pending the finalization of their
    -- last finalized block
    --
    -- $awaitingLastFinalized
    -- The blocks awaiting their last finalized block to become finalized are
    -- conceptually stored in a min priority queue, where the priority is the
    -- height of the block's indicated last finalized block.
    -- When a block is finalized, all of the blocks that are awaiting last
    -- finalized blocks of at most that height can be processed.
    --
    -- |Add a block that is awaiting finalization of its last finalized block.
    addAwaitingLastFinalized :: BlockHeight         -- ^Height of block's last finalized block
                                -> PendingBlock     -- ^Block that is pending
                                -> m ()
    -- |Take the next awaiting-last-finalized block where the height of the
    -- block that is awaiting finalization is less than or equal to the given
    -- value.
    takeAwaitingLastFinalizedUntil :: BlockHeight -> m (Maybe PendingBlock)
    -- * Operations on the finalization pool
    -- |Get the finalization pool at the given finalization index.
    getFinalizationPoolAtIndex :: FinalizationIndex -> m [FinalizationRecord]
    -- |Set the finalization pool at the given finalization index.
    putFinalizationPoolAtIndex :: FinalizationIndex -> [FinalizationRecord] -> m ()
    -- |Add a finalization record to the finalization pool.
    addFinalizationRecordToPool :: FinalizationRecord -> m ()
    -- * Operations on the pending transaction table
    --
    -- $pendingTransactions
    -- We maintain a 'PendingTransactionTable' for a particular block that is
    -- the focus block.  (Ideally, this should be the best block, however, it 
    -- shouldn't be a problem if it's not.)
    -- |Return the focus block.
    getFocusBlock :: m BlockPointer
    -- |Update the focus block.
    putFocusBlock :: BlockPointer -> m ()
    -- |Get the pending transactions after execution of the focus block.
    getPendingTransactions :: m PendingTransactionTable
    -- |Set the pending transactions after execution of the focus block.
    putPendingTransactions :: PendingTransactionTable -> m ()

    -- |Get non-finalized transactions for the given account starting at the given nonce (inclusive).
    -- These are returned as an ordered list of pairs of nonce and non-empty set of transactions
    -- with that nonce.
    getAccountNonFinalized :: AccountAddress -> Nonce -> m [(Nonce, Set.Set HashedTransaction)]

    -- * Operations on the transaction table
    -- |Add a transaction to the transaction table.
    -- Does nothing if the transaction's nonce preceeds the next available nonce
    -- for the account at the last finalized block, or if a transaction with the same
    -- hash is already in the table.
    -- Otherwise, adds the transaction to the table and the non-finalized transactions
    -- for its account.
    -- A return value of @True@ indicates that the transaction was added (and not already
    -- present).  A return value of @False@ indicates that the transaction was not added,
    -- either because it was already present or the nonce has already been finalized.
    addTransaction :: HashedTransaction -> m Bool
    addTransaction tr = addCommitTransaction tr 0
    -- |Finalize a list of transactions.  Per account, the transactions must be in
    -- continuous sequence by nonce, starting from the next available non-finalized
    -- nonce.
    finalizeTransactions :: [HashedTransaction] -> m ()
    -- |Mark a transaction as committed on a block with the given slot number.
    -- This will prevent it from being purged while the slot number exceeds
    -- that of the last finalized block.
    commitTransaction :: Slot -> HashedTransaction -> m ()
    -- |Add a transaction and mark it committed for the given slot number.
    addCommitTransaction :: HashedTransaction -> Slot -> m Bool
    -- |Purge a transaction from the transaction table if its last committed slot
    -- number does not exceed the slot number of the last finalized block.
    -- (A transaction that has been committed to a finalized block should not be purged.)
    -- Returns @True@ if the transaction is purged.
    purgeTransaction :: HashedTransaction -> m Bool
    -- * Operations on statistics
    -- |Get the current consensus statistics.
    getConsensusStatistics :: m ConsensusStatistics
    -- |Set the consensus statistics.
    putConsensusStatistics :: ConsensusStatistics -> m ()

instance (TreeStateMonad m) => TreeStateMonad (MaybeT m) where
    getBlockStatus  = lift . getBlockStatus
    makeLiveBlock b parent lastFin st time = lift $ makeLiveBlock b parent lastFin st time
    markDead = lift . markDead
    markFinalized bh fr = lift $ markFinalized bh fr
    getGenesisBlockPointer = lift getGenesisBlockPointer
    getGenesisData = lift getGenesisData
    getLastFinalized = lift getLastFinalized
    getLastFinalizedSlot = lift getLastFinalizedSlot
    getLastFinalizedHeight = lift getLastFinalizedHeight
    getNextFinalizationIndex = lift getNextFinalizationIndex
    addFinalization bp fr = lift $ addFinalization bp fr
    getBranches = lift getBranches
    putBranches = lift . putBranches
    takePendingChildren = lift . takePendingChildren
    addPendingBlock = lift . addPendingBlock
    takeNextPendingUntil = lift . takeNextPendingUntil
    addAwaitingLastFinalized bh pb = lift $ addAwaitingLastFinalized bh pb
    takeAwaitingLastFinalizedUntil = lift . takeAwaitingLastFinalizedUntil
    getFinalizationPoolAtIndex = lift . getFinalizationPoolAtIndex
    putFinalizationPoolAtIndex fi frs = lift $ putFinalizationPoolAtIndex fi frs
    addFinalizationRecordToPool = lift . addFinalizationRecordToPool
    getFocusBlock = lift getFocusBlock
    putFocusBlock = lift . putFocusBlock
    getPendingTransactions = lift getPendingTransactions
    putPendingTransactions = lift . putPendingTransactions
    getAccountNonFinalized acc = lift . getAccountNonFinalized acc
    addTransaction  = lift . addTransaction
    finalizeTransactions = lift . finalizeTransactions
    commitTransaction slot tr = lift $ commitTransaction slot tr
    addCommitTransaction tr slot = lift $ addCommitTransaction tr slot
    purgeTransaction = lift . purgeTransaction
    getConsensusStatistics = lift getConsensusStatistics
    putConsensusStatistics = lift . putConsensusStatistics

instance (TreeStateMonad m, Monoid w) => TreeStateMonad (RWST r w s m) where
    getBlockStatus  = lift . getBlockStatus
    makeLiveBlock b parent lastFin st time = lift $ makeLiveBlock b parent lastFin st time
    markDead = lift . markDead
    markFinalized bh fr = lift $ markFinalized bh fr
    getGenesisBlockPointer = lift getGenesisBlockPointer
    getGenesisData = lift getGenesisData
    getLastFinalized = lift getLastFinalized
    getLastFinalizedSlot = lift getLastFinalizedSlot
    getLastFinalizedHeight = lift getLastFinalizedHeight
    getNextFinalizationIndex = lift getNextFinalizationIndex
    addFinalization bp fr = lift $ addFinalization bp fr
    getBranches = lift getBranches
    putBranches = lift . putBranches
    takePendingChildren = lift . takePendingChildren
    addPendingBlock = lift . addPendingBlock
    takeNextPendingUntil = lift . takeNextPendingUntil
    addAwaitingLastFinalized bh pb = lift $ addAwaitingLastFinalized bh pb
    takeAwaitingLastFinalizedUntil = lift . takeAwaitingLastFinalizedUntil
    getFinalizationPoolAtIndex = lift . getFinalizationPoolAtIndex
    putFinalizationPoolAtIndex fi frs = lift $ putFinalizationPoolAtIndex fi frs
    addFinalizationRecordToPool = lift . addFinalizationRecordToPool
    getFocusBlock = lift getFocusBlock
    putFocusBlock = lift . putFocusBlock
    getPendingTransactions = lift getPendingTransactions
    putPendingTransactions = lift . putPendingTransactions
    getAccountNonFinalized acc = lift . getAccountNonFinalized acc
    addTransaction  = lift . addTransaction
    finalizeTransactions = lift . finalizeTransactions
    commitTransaction slot tr = lift $ commitTransaction slot tr
    addCommitTransaction tr slot = lift $ addCommitTransaction tr slot
    purgeTransaction = lift . purgeTransaction
    getConsensusStatistics = lift getConsensusStatistics
    putConsensusStatistics = lift . putConsensusStatistics
