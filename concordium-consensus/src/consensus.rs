use byteorder::{ByteOrder, LittleEndian};

use concordium_common::{
    into_err, PacketType, RelayOrStopEnvelope, RelayOrStopReceiver, RelayOrStopSender,
    RelayOrStopSenderHelper, RelayOrStopSyncSender,
};
use failure::{bail, Fallible};

#[cfg(test)]
use std::time::Duration;
use std::{
    collections::HashMap,
    fmt, mem, str,
    sync::{mpsc, Arc, Mutex, RwLock},
    thread, time,
};

use crate::{fails::BakerNotRunning, ffi::*};
use concordium_global_state::{block::*, common::*, finalization::*, tree::SkovReq};

pub type PeerId = u64;
pub type PrivateData = HashMap<i64, Vec<u8>>;

pub struct ConsensusMessage {
    pub variant:  PacketType,
    pub producer: Option<PeerId>,
    pub payload:  Arc<[u8]>,
}

impl ConsensusMessage {
    pub fn new(variant: PacketType, producer: Option<PeerId>, payload: Box<[u8]>) -> Self {
        Self {
            variant,
            producer,
            payload: Arc::from(payload),
        }
    }
}

impl fmt::Debug for ConsensusMessage {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        macro_rules! print_deserialized {
            ($object:ty) => {{
                if let Ok(object) = <$object>::deserialize(&self.payload) {
                    format!("{:?}", object)
                } else {
                    format!("corrupted bytes")
                }
            }};
        }

        let content = match self.variant {
            PacketType::Block => print_deserialized!(Block),
            PacketType::FinalizationRecord => print_deserialized!(FinalizationRecord),
            PacketType::FinalizationMessage => print_deserialized!(FinalizationMessage),
            PacketType::CatchupBlockByHash => {
                let hash = HashBytes::new(&self.payload[..SHA256 as usize]);
                let delta = LittleEndian::read_u64(
                    &self.payload[SHA256 as usize..][..mem::size_of::<Delta>()],
                );
                format!("catch-up request for block {:?}, delta {}", hash, delta)
            }
            PacketType::CatchupFinalizationRecordByHash => {
                let hash = HashBytes::new(&self.payload[..SHA256 as usize]);
                format!(
                    "catch-up request for the finalization record for block {:?}",
                    hash
                )
            }
            PacketType::CatchupFinalizationRecordByIndex => {
                let idx = LittleEndian::read_u64(
                    &self.payload[..mem::size_of::<FinalizationIndex>() as usize],
                );
                format!(
                    "catch-up request for the finalization record at index {}",
                    idx
                )
            }
            p => format!("{}", p),
        };

        write!(f, "{}", content)
    }
}

pub struct ConsensusOutQueue {
    receiver_request: Arc<Mutex<RelayOrStopReceiver<ConsensusMessage>>>,
    sender_request:   RelayOrStopSyncSender<ConsensusMessage>,
}

const SYNC_CHANNEL_BOUND: usize = 64;

impl Default for ConsensusOutQueue {
    fn default() -> Self {
        let (sender_request, receiver_request) =
            mpsc::sync_channel::<RelayOrStopEnvelope<ConsensusMessage>>(SYNC_CHANNEL_BOUND);
        ConsensusOutQueue {
            receiver_request: Arc::new(Mutex::new(receiver_request)),
            sender_request,
        }
    }
}

impl ConsensusOutQueue {
    pub fn send_message(&self, message: ConsensusMessage) -> Fallible<()> {
        into_err!(self.sender_request.send_msg(message))
    }

    pub fn recv_message(
        &self,
        skov_sender: &RelayOrStopSender<SkovReq>,
    ) -> Fallible<RelayOrStopEnvelope<ConsensusMessage>> {
        let message = into_err!(safe_lock!(self.receiver_request).recv());

        if let Ok(RelayOrStopEnvelope::Relay(ref msg)) = message {
            match msg.variant {
                PacketType::Block
                | PacketType::FinalizationRecord
                | PacketType::CatchupBlockByHash
                | PacketType::CatchupFinalizationRecordByHash
                | PacketType::CatchupFinalizationRecordByIndex => {
                    let request = SkovReq::new(None, msg.variant, Arc::clone(&msg.payload));
                    skov_sender.send(RelayOrStopEnvelope::Relay(request))?
                },
                _ => {} // not used in Skov,
            }
        }

        message
    }

    pub fn clear(&self) {
        if let Ok(ref mut q) = self.receiver_request.try_lock() {
            debug!(
                "Drained the Consensus request queue for {} element(s)",
                q.try_iter().count()
            );
        }
    }

    pub fn stop(&self) -> Fallible<()> {
        into_err!(self.sender_request.send_stop())?;
        Ok(())
    }
}

#[cfg(test)]
impl ConsensusOutQueue {
    pub fn try_recv_message(self) -> Fallible<RelayOrStopEnvelope<ConsensusMessage>> {
        into_err!(safe_lock!(self.receiver_request).try_recv())
    }

    pub fn recv_timeout_message(
        self,
        timeout: Duration,
    ) -> Fallible<RelayOrStopEnvelope<ConsensusMessage>> {
        into_err!(safe_lock!(self.receiver_request).recv_timeout(timeout))
    }
}

macro_rules! baker_running_wrapper {
    ($self:ident, $call:expr) => {{
        safe_read!($self.baker)
            .as_ref()
            .map(|baker| $call(baker))
            .ok_or_else(|| failure::Error::from(BakerNotRunning))
    }};
}

lazy_static! {
    pub static ref CALLBACK_QUEUE: ConsensusOutQueue = { ConsensusOutQueue::default() };
    pub static ref GENERATED_PRIVATE_DATA: RwLock<PrivateData> = { RwLock::new(HashMap::new()) };
    pub static ref GENERATED_GENESIS_DATA: RwLock<Option<Vec<u8>>> = { RwLock::new(None) };
}

#[derive(Clone, Default)]
pub struct ConsensusContainer {
    pub baker: Arc<RwLock<Option<ConsensusBaker>>>,
}

impl ConsensusContainer {
    pub fn start_baker(&mut self, baker_id: u64, genesis_data: Vec<u8>, private_data: Vec<u8>) {
        safe_write!(self.baker).replace(ConsensusBaker::new(baker_id, genesis_data, private_data));
    }

    pub fn stop_baker(&mut self) {
        if let Ok(mut baker) = self.baker.write() {
            if CALLBACK_QUEUE.stop().is_err() {
                error!("Some queues couldn't send a stop signal");
            };

            if let Some(baker) = baker.as_ref() {
                baker.stop()
            }
            baker.take();

            if baker.is_none() {
                CALLBACK_QUEUE.clear();
            }
        } else {
            error!("The baker can't be stopped!");
        }
    }

    pub fn send_block(&self, peer_id: PeerId, block: &[u8]) -> ConsensusFfiResponse {
        if let Some(baker) = &*safe_read!(self.baker) {
            baker.send_block(peer_id, block)
        } else {
            ConsensusFfiResponse::BakerNotFound
        }
    }

    pub fn send_finalization(&self, peer_id: PeerId, msg: &[u8]) -> ConsensusFfiResponse {
        if let Some(baker) = &*safe_read!(self.baker) {
            baker.send_finalization(peer_id, msg)
        } else {
            ConsensusFfiResponse::BakerNotFound
        }
    }

    pub fn send_finalization_record(&self, peer_id: PeerId, rec: &[u8]) -> ConsensusFfiResponse {
        if let Some(baker) = &*safe_read!(self.baker) {
            baker.send_finalization_record(peer_id, rec)
        } else {
            ConsensusFfiResponse::BakerNotFound
        }
    }

    pub fn send_transaction(&self, tx: &[u8]) -> ConsensusFfiResponse {
        if let Some(baker) = &*safe_read!(self.baker) {
            baker.send_transaction(tx.to_vec())
        } else {
            ConsensusFfiResponse::BakerNotFound
        }
    }

    pub fn generate_data(genesis_time: u64, num_bakers: u64) -> Fallible<(Vec<u8>, PrivateData)> {
        if let Ok(ref mut lock) = GENERATED_GENESIS_DATA.write() {
            **lock = None;
        }

        if let Ok(ref mut lock) = GENERATED_PRIVATE_DATA.write() {
            lock.clear();
        }

        unsafe {
            makeGenesisData(
                genesis_time,
                num_bakers,
                on_genesis_generated,
                on_private_data_generated,
            );
        }

        for _ in 0..num_bakers {
            if !safe_read!(GENERATED_GENESIS_DATA).is_some()
                || safe_read!(GENERATED_PRIVATE_DATA).len() < num_bakers as usize
            {
                thread::sleep(time::Duration::from_millis(200));
            }
        }

        let genesis_data = match GENERATED_GENESIS_DATA.write() {
            Ok(ref mut genesis) if genesis.is_some() => genesis.take().unwrap(),
            _ => bail!("Didn't get genesis data from Haskell"),
        };

        if let Ok(priv_data) = GENERATED_PRIVATE_DATA.read() {
            if priv_data.len() < num_bakers as usize {
                bail!("Didn't get private data from Haskell");
            } else {
                return Ok((genesis_data, priv_data.clone()));
            }
        } else {
            bail!("Didn't get private data from Haskell");
        }
    }

    pub fn get_consensus_status(&self) -> Fallible<String> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker.get_consensus_status())
    }

    pub fn get_block_info(&self, block_hash: &str) -> Fallible<String> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_block_info(block_hash))
    }

    pub fn get_ancestors(&self, block_hash: &str, amount: u64) -> Fallible<String> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_ancestors(block_hash, amount))
    }

    pub fn get_branches(&self) -> Fallible<String> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker.get_branches())
    }

    pub fn get_account_list(&self, block_hash: &str) -> Fallible<String> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_account_list(block_hash))
    }

    pub fn get_instances(&self, block_hash: &str) -> Fallible<String> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_instances(block_hash))
    }

    pub fn get_account_info(&self, block_hash: &str, account_address: &str) -> Fallible<String> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_account_info(block_hash, account_address))
    }

    pub fn get_instance_info(&self, block_hash: &str, contract_address: &str) -> Fallible<String> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_instance_info(block_hash, contract_address))
    }

    pub fn get_reward_status(&self, block_hash: &str) -> Fallible<String> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_reward_status(block_hash))
    }

    pub fn get_birk_parameters(&self, block_hash: &str) -> Fallible<String> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_birk_parameters(block_hash))
    }

    pub fn get_module_list(&self, block_hash: &str) -> Fallible<String> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_module_list(block_hash))
    }

    pub fn get_module_source(&self, block_hash: &str, module_ref: &str) -> Fallible<Vec<u8>> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_module_source(block_hash, module_ref))
    }

    pub fn get_block(&self, block_hash: &[u8]) -> Fallible<Vec<u8>> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker.get_block(block_hash))
    }

    pub fn get_block_by_delta(&self, block_hash: &[u8], delta: Delta) -> Fallible<Vec<u8>> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_block_by_delta(block_hash, delta))
    }

    pub fn get_block_finalization(&self, block_hash: &[u8]) -> Fallible<Vec<u8>> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_block_finalization(block_hash))
    }

    pub fn get_indexed_finalization(&self, index: u64) -> Fallible<Vec<u8>> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_indexed_finalization(index))
    }

    pub fn get_finalization_point(&self) -> Fallible<Vec<u8>> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_finalization_point())
    }

    pub fn get_finalization_messages(
        &self,
        request: &[u8],
        peer_id: PeerId,
    ) -> Fallible<ConsensusFfiResponse> {
        baker_running_wrapper!(self, |baker: &ConsensusBaker| baker
            .get_finalization_messages(request, peer_id))
    }
}

pub fn catchup_enqueue(request: ConsensusMessage) {
    let request_info = format!("{:?}", request.payload);

    match CALLBACK_QUEUE.send_message(request) {
        Ok(_) => debug!("Queueing a catch-up request: {}", request_info),
        _ => error!("Couldn't queue a catch-up request ({})", request_info),
    }
}