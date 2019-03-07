use std::sync::atomic::{ AtomicU64, Ordering };
use std::sync::mpsc::{ Sender };
use std::sync::{ Arc, Mutex, RwLock };
use rustls::{ ServerSession, ClientSession, Session };

use common::{ P2PNodeId, P2PPeer, ConnectionType, get_current_stamp };
use connection::{ P2PNodeMode, P2PEvent };
use network::{ Buckets };
use prometheus_exporter::{ PrometheusServer };


pub type ConnServerSession = Option< Arc< RwLock< ServerSession > > >;
pub type ConnClientSession = Option< Arc< RwLock< ClientSession > > >;
pub type ConnSession = Option< Arc< RwLock<dyn Session > > >;

/// It is just a helper struct to facilitate sharing information with
/// message handlers, which are set up from _inside_ `Connection`.
/// In this way, all closures only need two arguments:
///     - This structure as a shared object, like `Rc< RefCell<...>>`
///     - The input message.
pub struct ConnectionPrivate {
    pub connection_type: ConnectionType,
    pub own_id: P2PNodeId,
    pub mode: P2PNodeMode,
    pub self_peer: P2PPeer,
    peer: Option<P2PPeer>,
    pub networks: Vec<u16>,
    pub own_networks: Arc<Mutex<Vec<u16>>>,
    pub buckets: Arc< RwLock< Buckets > >,

    // Session
    initiated_by_me: bool,
    tls_server_session: ConnServerSession,
    tls_client_session: ConnClientSession,

    // Stats
    last_seen: AtomicU64,
    pub failed_pkts: u32,
    pub prometheus_exporter: Option<Arc<Mutex<PrometheusServer>>>,
    pub event_log: Option<Sender<P2PEvent>>,

    // Time
    pub sent_handshake: u64,
    pub sent_ping: u64,
    pub last_latency_measured: u64,
}

impl ConnectionPrivate {
    pub fn new(
            connection_type: ConnectionType,
            mode: P2PNodeMode,
            own_id: P2PNodeId,
            self_peer: P2PPeer,
            own_networks: Arc< Mutex< Vec<u16>>>,
            buckets: Arc< RwLock< Buckets > >,
            initiated_by_me: bool,
            tls_server_session: Option< ServerSession>,
            tls_client_session: Option< ClientSession>,
            prometheus_exporter: Option<Arc<Mutex<PrometheusServer>>>,
            event_log: Option<Sender<P2PEvent>>,
            ) -> Self {

        let u64_max_value: u64 = u64::max_value();
        let srv_session = if let Some(s) = tls_server_session { Some(Arc::new( RwLock::new(s)))} else { None };
        let cli_session = if let Some(c) = tls_client_session { Some(Arc::new( RwLock::new(c)))} else { None };

        ConnectionPrivate {
            connection_type: connection_type,
            mode: mode,
            own_id: own_id,
            self_peer: self_peer,
            peer: None,
            networks: vec![],
            own_networks: own_networks,
            buckets: buckets,

            initiated_by_me: initiated_by_me,
            tls_server_session: srv_session,
            tls_client_session: cli_session,

            last_seen: AtomicU64::new( get_current_stamp()),
            failed_pkts: 0 as u32,
            prometheus_exporter: prometheus_exporter,
            event_log: event_log,

            sent_handshake: u64_max_value,
            sent_ping: u64_max_value,
            last_latency_measured: u64_max_value
        }
    }

    /// It returns the `Client Session` if connection has been initiated by me.
    /// Otherwise, it will return its `Server Session`.
    /// Both kind of session could be `None`.
    pub fn session(&self) -> ConnSession {
        if self.initiated_by_me {
            if let Some(ref cli_session) = self.tls_client_session {
                Some( Arc::clone(&cli_session) as Arc< RwLock< dyn Session>>)
            } else {
                None
            }
        } else {
            if let Some(ref srv_session) = self.tls_server_session {
                Some( Arc::clone(&srv_session) as Arc< RwLock< dyn Session>>)
            } else {
                None
            }
        }
    }

    pub fn update_last_seen(&mut self) {
        if self.mode != P2PNodeMode::BootstrapperMode
            && self.mode != P2PNodeMode::BootstrapperPrivateMode {
            self.last_seen.store( get_current_stamp(), Ordering::Relaxed);
        }
    }

    pub fn last_seen(&self) -> u64 {
        self.last_seen.load( Ordering::Relaxed)
    }

    pub fn add_networks(&mut self, networks: &Vec<u16>) {
        for ele in networks {
            if !self.networks.contains(ele) {
                self.networks.push(*ele);
            }
        }
    }

    pub fn remove_network(&mut self, network: &u16) {
        self.networks.retain(|x| x != network);
    }

    pub fn set_measured_ping_sent(&mut self) {
        self.sent_ping = get_current_stamp()
    }

    pub fn peer(&self) -> Option<P2PPeer> {
        self.peer.clone()
    }

    pub fn set_peer(&mut self, p: P2PPeer) {
        self.peer = Some(p);
    }
}
