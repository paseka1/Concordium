{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

-- |A runner that takes a genesis data file and a block file and executes the chain.
-- This is currently configured to use the Paired global state with disk-based and
-- memory-based storage to ensure that the execution is consistent between these.
module Main where

import qualified Data.ByteString as BS
import System.Directory
import System.Environment
import System.FilePath
import System.IO

import Concordium.GlobalState
import Concordium.GlobalState.Parameters
import Concordium.Kontrol (currentTimestamp)
import Concordium.MultiVersion
import Concordium.Skov
import Concordium.TimerMonad

parseArgs :: [String] -> IO (BS.ByteString, FilePath)
parseArgs [gdPath, blocksPath] = do
    gd <- BS.readFile gdPath
    return (gd, blocksPath)
parseArgs _ = error "Expected exactly two arguments: genesis data file, and blocks to execute file"

main :: IO ()
main = do
    (genesisData, blocks) <- parseArgs =<< getArgs
    now <- currentTimestamp
    logFile <- openFile ("consensus-" ++ show now ++ ".log") WriteMode
    let logM src lvl msg = {- when (lvl == LLInfo) $ -} do
            hPutStrLn logFile $ show lvl ++ " - " ++ show src ++ ": " ++ msg
            hFlush logFile
    -- let logM src lvl msg = putStrLn $ show lvl ++ " - " ++ show src ++ ": " ++ msg
    let dataDir = "data" </> ("db" ++ show now)
    -- createDirectoryIfMissing True dataDir
    let config ::
            MultiVersionConfiguration
                DiskTreeDiskBlockConfig
                (NoFinalization ThreadTimer)
        config =
            MultiVersionConfiguration
                { mvcStateConfig = DiskStateConfig dataDir,
                  mvcFinalizationConfig = NoFinalization,
                  mvcRuntimeParameters = defaultRuntimeParameters{rpTransactionsPurgingDelay = 0, rpAccountsCacheSize = 10000}
                }
    let callbacks =
            Callbacks
                { broadcastBlock = \_ _ -> return (),
                  broadcastFinalizationMessage = \_ _ -> return (),
                  broadcastFinalizationRecord = \_ _ -> return (),
                  notifyCatchUpStatus = \_ _ -> return (),
                  notifyRegenesis = \_ -> return ()
                }

    mvr <- makeMultiVersionRunner config callbacks Nothing logM (Left genesisData)
    shutdownMultiVersionRunner mvr
    print "DONE"
