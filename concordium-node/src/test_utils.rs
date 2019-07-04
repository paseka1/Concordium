use crate::{
    common::PeerType,
    configuration::Config,
    network::{NetworkMessage, NetworkPacketType, NetworkRequest, NetworkResponse},
    p2p::p2p_node::P2PNode,
    stats_export_service::{StatsExportService, StatsServiceMode},
};
use concordium_common::UCursor;
use failure::Fallible;
use std::{
    cell::RefCell,
    net::TcpListener,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::Receiver,
        Arc, Once, RwLock, ONCE_INIT,
    },
    time,
};
use structopt::StructOpt;

static INIT: Once = ONCE_INIT;
static PORT_OFFSET: AtomicUsize = AtomicUsize::new(0);
static PORT_START_NODE: u16 = 8888;

pub const TESTCONFIG: &[&str] = &["no-bootstrap"];

/// It returns next available port
pub fn next_available_port() -> u16 {
    let mut available_port = None;

    while available_port.is_none() {
        let port = PORT_OFFSET.fetch_add(1, Ordering::SeqCst) as u16 + PORT_START_NODE;
        available_port = TcpListener::bind(("127.0.0.1", port)).map(|_| port).ok();
        assert!(port < std::u16::MAX);
    }

    available_port.unwrap()
}

use chrono::{offset::Utc, DateTime};
use std::io::Write;

/// It initializes the global logger with a `env_logger`, but just once.
pub fn setup_logger() {
    // INIT.call_once(|| env_logger::init());

    // @note It adds thread ID to each message.
    INIT.call_once(|| {
        let mut builder = env_logger::Builder::from_default_env();
        builder
            .format(|buf, record| {
                let curr_thread = std::thread::current();
                let now: DateTime<Utc> = std::time::SystemTime::now().into();
                writeln!(
                    buf,
                    "[{} {} {} {:?}] {}",
                    now.format("%c"),
                    record.level(),
                    record.target(),
                    curr_thread.id(),
                    record.args()
                )
            })
            .init();
    });
}

#[cfg(debug_assertions)]
pub fn max_recv_timeout() -> std::time::Duration {
    time::Duration::from_secs(5 * 60) // 5 minutes
}

#[cfg(not(debug_assertions))]
pub fn max_recv_timeout() -> std::time::Duration {
    time::Duration::from_secs(60) // 1 minutes
}

/// It makes a list of nodes using `make_node_and_sync`.
///
/// # Arguments
/// * `port` - Initial port. Each node will use the port `port` + `i` where `i`
///   is `[0,
/// count)`.
/// * `count` - Number of nodes to be generated.
/// * `networks` - Networks added to new nodes.
///
/// # Return
/// As `make_node_and_sync`, this returns a tuple but it contains list
/// of objects instead of just one.
pub fn make_nodes_from_port(
    count: usize,
    networks: Vec<u16>,
    blind_trusted_broadcast: bool,
) -> Fallible<Vec<(RefCell<P2PNode>, Receiver<NetworkMessage>)>> {
    let mut nodes_and_receivers = Vec::with_capacity(count);

    for _i in 0..count {
        let (node, receiver) = make_node_and_sync(
            next_available_port(),
            networks.clone(),
            blind_trusted_broadcast,
            PeerType::Node,
        )?;

        nodes_and_receivers.push((RefCell::new(node), receiver));
    }

    Ok(nodes_and_receivers)
}

/// It creates a pair of `P2PNode` and a `Receiver` which can be used to
/// wait for specific messages.
/// Using this approach protocol tests will be easier and cleaner.
pub fn make_node_and_sync(
    port: u16,
    networks: Vec<u16>,
    blind_trusted_broadcast: bool,
    node_type: PeerType,
) -> Fallible<(P2PNode, Receiver<NetworkMessage>)> {
    let (net_tx, _) = std::sync::mpsc::channel();
    let (msg_wait_tx, msg_wait_rx) = std::sync::mpsc::channel();

    let mut config = Config::from_iter(TESTCONFIG.to_vec()).add_options(
        Some("127.0.0.1".to_owned()),
        port,
        networks,
        100,
    );
    config.connection.no_trust_broadcasts = blind_trusted_broadcast;

    let export_service = Arc::new(RwLock::new(
        StatsExportService::new(StatsServiceMode::NodeMode).unwrap(),
    ));
    let mut node = P2PNode::new(None, &config, net_tx, None, node_type, Some(export_service));
    let node_id = port;

    node.add_notification(make_atomic_callback!(move |m: &NetworkMessage| {
        log_any_message_handler(node_id, m)
    }))
    .add_notification(make_atomic_callback!(move |m: &NetworkMessage| {
        // It is safe to ignore error.
        let _ = msg_wait_tx.send(m.clone());
        Ok(())
    }));

    node.spawn();
    Ok((node, msg_wait_rx))
}

/// It connects `source` and `target` nodes, and it waits until
/// `receiver` receive a `handshake` response packet.
/// Other messages are ignored.
pub fn connect_and_wait_handshake(
    source: &mut P2PNode,
    target: &P2PNode,
    receiver: &Receiver<NetworkMessage>,
) -> Fallible<()> {
    source.connect(PeerType::Node, target.internal_addr, None)?;

    // Wait for Handshake response on source node
    loop {
        if let NetworkMessage::NetworkResponse(NetworkResponse::Handshake(..), ..) =
            receiver.recv()?
        {
            break;
        }
    }

    Ok(())
}

pub fn wait_broadcast_message(waiter: &Receiver<NetworkMessage>) -> Fallible<UCursor> {
    let payload;
    loop {
        let msg = waiter.recv()?;
        if let NetworkMessage::NetworkPacket(ref pac, ..) = msg {
            if let NetworkPacketType::BroadcastedMessage = pac.packet_type {
                payload = pac.message.clone();
                break;
            }
        }
    }

    Ok(payload)
}

pub fn wait_direct_message(waiter: &Receiver<NetworkMessage>) -> Fallible<UCursor> {
    let payload;
    loop {
        let msg = waiter.recv()?;
        if let NetworkMessage::NetworkPacket(ref pac, ..) = msg {
            if let NetworkPacketType::DirectMessage(..) = pac.packet_type {
                payload = pac.message.clone();
                break;
            }
        }
    }

    Ok(payload)
}

pub fn wait_direct_message_timeout(
    waiter: &Receiver<NetworkMessage>,
    timeout: std::time::Duration,
) -> Option<UCursor> {
    let mut payload = None;

    while let Ok(msg) = waiter.recv_timeout(timeout) {
        if let NetworkMessage::NetworkPacket(ref pac, ..) = msg {
            if let NetworkPacketType::DirectMessage(..) = pac.packet_type {
                payload = Some(pac.message.clone());
                break;
            }
        }
    }

    payload
}

pub fn consume_pending_messages(waiter: &Receiver<NetworkMessage>) {
    let max_wait_time = time::Duration::from_millis(250);
    loop {
        if waiter.recv_timeout(max_wait_time).is_err() {
            break;
        }
    }
}

/// Helper handler to log as `info` the secuence of packets received by
/// node.
///
/// It requires two steps in order to print messages when tests are running:
///  - Enable logger, using function `setup_logger()`.
///  - Enable `RUST_LOG` using:
///  ```ignore
///  $> export RUST_LOG "p2p_client::test_utils=trace"
///  ```
///
/// # Example
/// ```
/// # use concordium_common::make_atomic_callback;
/// # use p2p_client::{
/// #     common::PeerType,
/// #     connection::network_handler::message_processor::MessageManager,
/// #     network::NetworkMessage,
/// #     test_utils::{log_any_message_handler, make_node_and_sync},
/// # };
/// # use std::sync::{Arc, RwLock};
/// let (mut node, waiter) = make_node_and_sync(5555, vec![100], true, PeerType::Node).unwrap();
/// let id = node.id();
///
/// node.message_processor()
///     .write()
///     .unwrap()
///     .add_notification(make_atomic_callback!(move |m: &NetworkMessage| {
///         log_any_message_handler(id, m);
///         Ok(())
///     }));
/// ```
pub fn log_any_message_handler<T>(id: T, message: &NetworkMessage) -> Fallible<()>
where
    T: std::fmt::Display, {
    let msg_type: String = match message {
        NetworkMessage::NetworkRequest(ref request, ..) => match request {
            NetworkRequest::Ping(ref peer, ..) => format!("Request::Ping({})", peer.id()),
            NetworkRequest::FindNode(ref peer, ..) => format!("Request::FindNode({})", peer.id()),
            NetworkRequest::BanNode(ref peer, ..) => format!("Request::BanNode({})", peer.id()),
            NetworkRequest::Handshake(ref peer, ..) => format!("Request::Handshake({})", peer.id()),
            NetworkRequest::GetPeers(ref peer, ..) => format!("Request::GetPeers({})", peer.id()),
            NetworkRequest::UnbanNode(ref peer, ..) => format!("Request::UnbanNode({})", peer.id()),
            NetworkRequest::JoinNetwork(ref peer, ..) => {
                format!("Request::JoinNetwork({})", peer.id())
            }
            NetworkRequest::LeaveNetwork(ref peer, ..) => {
                format!("Request::LeaveNetwork({})", peer.id())
            }
            NetworkRequest::Retransmit(ref peer, ..) => {
                format!("Request::Retransmit({})", peer.id())
            }
        },
        NetworkMessage::NetworkResponse(ref response, ..) => match response {
            NetworkResponse::Pong(..) => "Response::Pong".to_owned(),
            NetworkResponse::FindNode(..) => "Response::FindNode".to_owned(),
            NetworkResponse::PeerList(..) => "Response::PeerList".to_owned(),
            NetworkResponse::Handshake(..) => "Response::Handshake".to_owned(),
        },
        NetworkMessage::NetworkPacket(ref packet, ..) => match packet.packet_type {
            NetworkPacketType::BroadcastedMessage => {
                format!("Packet::Broadcast(size={})", packet.message.len())
            }
            NetworkPacketType::DirectMessage(src_node_id, ..) => format!(
                "Packet::Direct(from={},size={})",
                src_node_id,
                packet.message.len()
            ),
        },
        NetworkMessage::UnknownMessage => "Unknown".to_owned(),
        NetworkMessage::InvalidMessage => "Invalid".to_owned(),
    };
    info!("Message at {}: {}", id, msg_type);

    Ok(())
}
