use mio::*;
use mio::net::TcpListener;
use mio::net::TcpStream;
use std::net::SocketAddr;
use std::io::Error;
use std::io::Write;
use std::io::Read;
use std::io;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::time::Duration;
use get_if_addrs;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::IpAddr::{V4, V6};
use std::str::FromStr;
use utils;
use common::{NetworkMessage,NetworkRequest, NetworkPacket, NetworkResponse, P2PPeer, P2PNodeId};
use num_bigint::BigUint;
use num_traits::Num;
use num_bigint::ToBigUint;
use num_traits::pow;
use bytes::{Bytes, BytesMut, Buf, BufMut};
use bincode::{serialize, deserialize};
use time;
use rustls;
use webpki;
use std::sync::Arc;
use rustls::{Session, ServerConfig, NoClientAuth, Certificate, PrivateKey, ServerSession};
use rustls::internal::msgs::codec::Codec;

use env_logger;
#[macro_use]
use log;

const SERVER: Token = Token(0);
const BUCKET_SIZE: u8 = 20;
const KEY_SIZE: u16 = 256;

// REMOVE WHEN REPLACED!!!
pub fn get_self_peer() -> P2PPeer {
    P2PPeer::new(IpAddr::from_str("10.10.10.10").unwrap(), 9999)
}

struct Connection {
    handle: TcpStream,
    currently_read: u64,
    expected_size: u64,
    buffer: Option<BytesMut>,
    tls_session: ServerSession,
}

impl Connection {
    fn new(handle: TcpStream, tls_session: ServerSession) -> Self {
        Connection {
            handle: handle,
            currently_read: 0,
            expected_size: 0,
            buffer: None,
            tls_session
        }
    }

    pub fn append_buffer(&mut self, new_data: &[u8] ) {
        if let Some(ref mut buf) = self.buffer {
            buf.reserve(new_data.len());
            buf.put_slice(new_data);
            self.currently_read += new_data.len() as u64;
        }
    }

    pub fn clear_buffer(&mut self) {
        if let Some(ref mut buf) = self.buffer {
            buf.clear();
        }
        self.buffer = None;
    }

    pub fn setup_buffer(&mut self) {
        self.buffer = Some(BytesMut::with_capacity(1024));
    }
}

pub struct P2PMessage {
    pub addr: P2PNodeId,
    pub msg: Vec<u8>,
}

impl P2PMessage {
    pub fn new(addr: P2PNodeId, msg: Vec<u8>) -> Self {
        P2PMessage {
            addr,
            msg
        }
    }
}

pub struct TlsClient {
    socket: TcpStream,
    closing: bool,
    clean_closure: bool,
    tls_session: rustls::ClientSession,
}

impl io::Read for TlsClient {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.tls_session.read(bytes)
    }
}

impl io::Write for TlsClient {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.tls_session.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tls_session.flush()
    }
}

impl TlsClient {
    fn new(sock: TcpStream, hostname: webpki::DNSNameRef, cfg: Arc<rustls::ClientConfig>) -> TlsClient {
        TlsClient {
            socket: sock,
            closing: false,
            clean_closure: false,
            tls_session: rustls::ClientSession::new(&cfg, hostname),
        }
    }

    fn do_read(&mut self) {
        let rc = self.tls_session.read_tls(&mut self.socket);
        if rc.is_err() {
            println!("TLS read error: {:?}", rc);
            self.closing = true;
            return;
        }

        if rc.unwrap() == 0 {
            println!("EOF");
            self.closing = true;
            self.clean_closure = true;
            return;
        }

        let processed = self.tls_session.process_new_packets();
        if processed.is_err() {
            println!("TLS error: {:?}", processed.unwrap_err());
            self.closing = true;
            return;
        }

        let mut plaintext = Vec::new();
        let rc = self.tls_session.read_to_end(&mut plaintext);
        if !plaintext.is_empty() {
             io::stdout().write_all(&plaintext).unwrap();
        }

        if rc.is_err() {
            let err = rc.unwrap_err();
            println!("Plaintext read error: {:?}", err);
            self.clean_closure = err.kind() == io::ErrorKind::ConnectionAborted;
            self.closing = true;
            return;
        }
    }

    fn do_write(&mut self) {
        self.tls_session.write_tls(&mut self.socket).unwrap();
    }
}

struct TlsServer {
    server: TcpListener,
    connections: HashMap<Token, Connection>,
    next_id: usize,
    tls_config: Arc<ServerConfig>,
}

impl TlsServer {
    fn new(server: TcpListener, cfg: Arc<ServerConfig>) -> TlsServer {
        server,
        connections: HashMap::new(),
        next_id: 2,
        tls_config: cfg,
    }

    fn accept(&mut self, poll: &mut Poll) -> bool {
        match self.server.accept() {
            Ok((socket, addr)) => {
                debug!("Accepting new connection from {:?}", addr);

                let tls_session = ServerSession::new(&self.tls_config);
                let token = Token(self.next_id);
                self.next_id += 1;

                self.connections.insert(token, Connection::new(socket, token, tls_session));
                self.connections[&token].register(poll);

                true
            },
            Err(e) => {
                println!("encountered error while accepting connection; err={:?}", e);
                false
            }
        }
    }
}

pub struct P2PNode {
    listener: TcpListener,
    poll: Poll,
    token_counter: usize,
    peers: HashMap<Token, Connection>,
    out_rx: Receiver<P2PMessage>,
    in_tx: Sender<P2PMessage>,
    id: P2PNodeId,
    buckets: HashMap<u16, Vec<P2PPeer>>,
    map: HashMap<BigUint, Token>,
    send_queue: VecDeque<P2PMessage>,
}

impl P2PNode {
    pub fn new(supplied_id: Option<String>, port: u16, out_rx: Receiver<P2PMessage>, in_tx: Sender<P2PMessage>) -> Self {
        let addr = format!("0.0.0.0:{}", port).parse().unwrap();;

        info!("Creating new P2PNode");

        
        //Retrieve IP address octets, format to IP and SHA256 hash it
        let octets = P2PNode::get_ip().unwrap().octets();
        let ip_port = format!("{}.{}.{}.{}:{}", octets[0], octets[1], octets[2], octets[3], port);
        info!("Listening on {:?}", ip_port);

        let id = match supplied_id {
            Some(x) => {
                if x.chars().count() != 64 {
                    panic!("Incorrect ID specified.. Should be a sha256 value or 64 characters long!");
                }
                x
            },
            _ => {
                let instant = time::get_time();
                utils::to_hex_string(utils::sha256(&format!("{}.{}", instant.sec, instant.nsec)))
            }
        };

        let _id = P2PNodeId::from_string(id.clone());
        println!("Got ID: {}", _id.clone().to_string());

        let poll = match Poll::new() {
            Ok(x) => x,
            _ => panic!("Couldn't create poll")
        };

        let server = match TcpListener::bind(&addr) {
            Ok(x) => x,
            _ => panic!("Couldn't listen on port!")
        };

        match poll.register(&server, SERVER, Ready::readable(), PollOpt::edge()) {
            Ok(x) => (),
            _ => panic!("Couldn't register server with poll!")
        };

        let mut buckets = HashMap::new();
        for i in 0..KEY_SIZE {
            buckets.insert(i, Vec::new());
        }

        //Generate key pair and cert
        let (cert, private_key) = match utils::generate_certificate(id) {
            Ok(x) => {
                match x.x509.to_der() {
                    Ok(der) => {
                        match x.private_key.private_key_to_der() {
                            Ok(private_part) => {
                                (Certificate(der), PrivateKey(private_part))
                            },
                            Err(e) => {
                                panic!("Couldn't convert certificate to DER! {:?}", e);
                            }
                        }
                    },
                    Err(e) => {
                        panic!("Couldn't convert certificate to DER! {:?}", e);
                    }
                }
            },
            Err(e) => {
                panic!("Couldn't create certificate! {:?}", e);
            }
        };

        //TLS Server config
        let mut server_conf = ServerConfig::new(NoClientAuth::new());
        server_conf.set_single_cert(vec![cert], private_key);
        //server_conf.key_log = Arc::new(rustls::KeyLogFile::new());

        P2PNode {
            listener: server,
            poll,
            token_counter: 1,
            peers: HashMap::new(),
            out_rx,
            in_tx,
            id: _id,
            buckets,
            map: HashMap::new(),
            send_queue: VecDeque::new(),
        }

        
    }

    pub fn get_ip() -> Option<Ipv4Addr>{
        let mut ip : Ipv4Addr = Ipv4Addr::new(127,0,0,1);

        for adapter in get_if_addrs::get_if_addrs().unwrap() {
            match adapter.addr.ip() {
                V4(x) => {
                    if !x.is_loopback() && !x.is_link_local() && !x.is_multicast() && !x.is_broadcast() {
                        ip = x;
                    }
                    
                },
                V6(_) => {
                    //Ignore for now
                }
            };
            
        }

        if ip == Ipv4Addr::new(127,0,0,1) {
            None
        } else {
            Some(ip)
        }
    }

    pub fn distance(&self, to: &P2PNodeId) -> BigUint {
        self.id.get_id().clone() ^ to.get_id().clone()
    }

    pub fn insert_into_bucket(&mut self, node: P2PPeer) {
        let dist = self.distance(&node.id());
        for i in 0..KEY_SIZE {
            if dist >= pow(2_i8.to_biguint().unwrap(),i as usize) && dist < pow(2_i8.to_biguint().unwrap(),(i as usize)+1) {
                let bucket = self.buckets.get_mut(&i).unwrap();
                //Check size
                if bucket.len() >= BUCKET_SIZE as usize {
                    //Check if front element is active
                    bucket.remove(0);
                }
                bucket.push(node);
                break;
            }
        }
    }

    pub fn find_bucket_id(&mut self, id: P2PNodeId) -> Option<u16> {
        let dist = self.distance(&id);
        let mut ret : i32 = -1;
        for i in 0..KEY_SIZE {
            if dist >= pow(2_i8.to_biguint().unwrap(),i as usize) && dist < pow(2_i8.to_biguint().unwrap(),(i as usize)+1) {
                ret = i as i32;
            }
        }

        if ret == -1 {
            None
        } else {
            Some(ret as u16)
        }
    }

    pub fn closest_nodes(&self, id: P2PNodeId) -> Vec<P2PPeer> {
        let mut ret : Vec<P2PPeer> = Vec::with_capacity(KEY_SIZE as usize);
        let mut count = 0;
        for (_, bucket) in &self.buckets {
            //Fix later to do correctly
            if count < KEY_SIZE {
                for peer in bucket {
                    if count < KEY_SIZE {
                        ret.push(peer.clone());
                        count += 1;
                    } else {
                        break;
                    }
                }
            } else {
                break;
            }

        }

        ret
    }

    pub fn process_messages(&mut self) {
        //Try to send queued messages first
        loop {
            match self.send_queue.pop_front() {
                Some(x) => {
                    //We have messages to send
                    info!("Trying to send queued message to {:?}", x.addr);
                    match self.find_bucket_id(x.addr.clone()) {
                        Some(y) => {
                            //We found the appropriate bucket where the peer should be
                            let bucket = &self.buckets[&y];
                            let node = P2PPeer::from(x.addr, "127.0.0.1".parse().unwrap(), 8888);
                            if bucket.contains(&node) {
                                //Send directly to peer
                                match bucket.binary_search(&node) {
                                    Ok(idx) => {
                                        let peer = &bucket[idx];
                                        let mut socket = &self.peers[self.map.get(peer.id().get_id()).unwrap()].handle;
                                        socket.write(&x.msg).unwrap();
                                    },
                                    Err(_) => {
                                        panic!("Should never happen ..");
                                    }
                                }
                            } else {

                            }
                        },
                        None => {
                            //Send find_node to neighbors
                            //Let's just pick one at first
                            let node = P2PPeer::from(x.addr.clone(), "127.0.0.1".parse().unwrap(), 8888);
                            let nodes = &self.closest_nodes(node.id());
                            if nodes.len() > 0 {
                                let neighbor = &self.closest_nodes(node.id())[0];
                                let mut socket = &self.peers[self.map.get(neighbor.id().get_id()).unwrap()].handle;
                                socket.write(&NetworkRequest::FindNode(get_self_peer(), node.id()).serialize().as_bytes()).unwrap();
                            } else {
                                info!("Don't have any friends, so not sending message :(");
                            }

                            //Readd message
                            info!("Requeuing message for {:?}!", x.addr);
                            self.send_queue.push_back(x);
                        }
                    }
                },
                None => {
                    break;
                }
            }
        }
    }

    pub fn send_message(&mut self, id: P2PNodeId, msg: Vec<u8>) {
        info!("Queueing message!");
        self.send_queue.push_back(P2PMessage::new(id, msg));
    }

    pub fn connect(&mut self, peer: P2PPeer) -> Result<P2PNodeId, Error> {
        let stream = TcpStream::connect(&SocketAddr::new(peer.ip(), peer.port()));
        match stream {
            Ok(x) => {
                let token = Token(self.token_counter);
                let res = self.poll.register(&x, token, Ready::readable() | Ready::writable(), PollOpt::level());
                match res {
                    Ok(_) => {
                        //self.peers.insert(token, Connection::new(x));
                        self.insert_into_bucket(peer.clone());
                        println!("Inserting connection");
                        self.map.insert(peer.id().get_id().clone(), token);
                        self.token_counter += 1;
                        Ok(peer.id())
                    },
                    Err(x) => {
                        Err(x)
                    }
                }
            },
            Err(e) => {
                Err(e)
            }
        }
    }

    pub fn process(&mut self, events: &mut Events, channel: &mut Receiver<P2PPeer>) {
        loop {
            //Check if we have messages to receive
            match channel.try_recv() {
                Ok(x) => {
                    match self.connect(x) {
                        Ok(_) => {

                        },
                        Err(e) => {
                            println!("Error connecting: {}", e);
                        }
                    }
                },
                _ => {

                }
            }

            self.process_messages();

            self.poll.poll(events, Some(Duration::from_millis(500))).unwrap();

            for event in events.iter() {
                
                match event.token() {
                    SERVER => {
                        loop {
                            match self.listener.accept() {
                                Ok((mut socket, _)) => {
                                    let token = Token(self.token_counter);
                                    println!("Accepting connection from {}", socket.peer_addr().unwrap());
                                    self.poll.register(&socket, token, Ready::readable() | Ready::writable(), PollOpt::level()).unwrap();

                                    //let conn = Connection::new(socket);

                                    //self.peers.insert(token, conn);

                                    self.token_counter += 1;
                                }
                                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                                    break;
                                }
                                e => panic!("err={:?}", e),
                            }
                            
                        }
                    }
                    x => {
                        loop {
                            match self.peers.get_mut(&x) {
                                Some(conn) => {
                                    if conn.expected_size > 0 && conn.currently_read == conn.expected_size {
                                        println!("Completed file with {} size", conn.currently_read);
                                        conn.expected_size = 0;
                                        conn.currently_read = 0;
                                        conn.clear_buffer();
                                    } else if conn.expected_size > 0  {
                                        let remainder = conn.expected_size-conn.currently_read;
                                        if remainder < 512 {
                                            let mut buf = vec![0u8; remainder as usize];
                                            match conn.handle.read(&mut buf) {
                                                Ok(_) => {
                                                    conn.append_buffer(&buf);
                                                }
                                                _ => {}
                                            }
                                        } else {
                                            let mut buf = [0;512];
                                            match conn.handle.read(&mut buf) {
                                                Ok(_) => {
                                                    conn.append_buffer(&buf);
                                                }
                                                _ => {}
                                            }
                                        }

                                        if conn.currently_read == conn.expected_size  {
                                            println!("Completed file with {} size", conn.currently_read);
                                            conn.expected_size = 0;
                                            conn.currently_read = 0;
                                            conn.clear_buffer();
                                        }
                                    } else {
                                        let mut buf = [0;8];
                                        match conn.handle.peek(&mut buf) {
                                            Ok(n) => {
                                                if n == 8 {
                                                    conn.handle.read_exact(&mut buf).unwrap();
                                                    conn.expected_size = deserialize(&buf[..]).unwrap();
                                                    conn.setup_buffer();
                                                    println!("Starting new file, with expected size: {}", conn.expected_size);
                                                }
                                            }
                                            _ => println!("Error getting size..!")
                                        }
                                    }
                                },
                                None => {
                                    panic!("Couldn't find connection in peers list .. Should never have happened!");
                                }
                            }
                            let mut buf = [0; 256];
                            let y = x;


                            match self.peers.get_mut(&x).unwrap().handle.read(&mut buf) {
                                Ok(0) => {
                                    println!("Closing connection!");
                                    self.peers.remove(&x);
                                    break;
                                }
                                Ok(_) => {
                                    //self.insert_into_bucket(node)
                                    match NetworkMessage::deserialize(&String::from_utf8(buf.to_vec()).unwrap().trim_matches(char::from(0))) {
                                        NetworkMessage::NetworkRequest(x) => {
                                            match x {
                                                NetworkRequest::Ping(sender) => {
                                                    //Respond with pong
                                                    info!("Got request for ping");
                                                    self.peers.get_mut(&y).unwrap().handle.write(NetworkResponse::Pong(get_self_peer()).serialize().as_bytes()).unwrap();
                                                },
                                                NetworkRequest::FindNode(sender, x) => {
                                                    //Return list of nodes
                                                    info!("Got request for FindNode");
                                                    let nodes = self.closest_nodes(x);
                                                    self.peers.get_mut(&y).unwrap().handle.write(NetworkResponse::FindNode(get_self_peer(), nodes).serialize().as_bytes()).unwrap();
                                                },
                                                NetworkRequest::BanNode(sender, x) => {
                                                    info!("Got request for BanNode");
                                                    self.peers.get_mut(&y).unwrap().handle.write(NetworkResponse::BanNode(get_self_peer(), true).serialize().as_bytes()).unwrap();
                                                }
                                            }
                                        },
                                        NetworkMessage::NetworkResponse(x) => {
                                            match x {
                                                NetworkResponse::FindNode(sender, peers) => {
                                                    info!("Got response to FindNode");
                                                    //Process the received node list
                                                    for peer in peers.iter() {
                                                        self.insert_into_bucket(peer.clone());
                                                    }

                                                    self.insert_into_bucket(sender);
                                                },
                                                NetworkResponse::Pong(sender) => {
                                                    info!("Got response for ping");

                                                    //Note that node responded back
                                                    self.insert_into_bucket(sender);
                                                },
                                                NetworkResponse::BanNode(sender, ok) => {
                                                    info!("Got response to BanNode");
                                                }
                                            }
                                        },
                                        NetworkMessage::NetworkPacket(x) => {
                                            match x {
                                                NetworkPacket::DirectMessage(sender, msg) => {
                                                    info!("Got {} size direct message", msg.len());
                                                },
                                                NetworkPacket::BroadcastedMessage(sender,msg) => {
                                                    info!("Got {} size broadcasted packet", msg.len());
                                                }
                                            }
                                        },
                                        NetworkMessage::UnknownMessage => {
                                            info!("Unknown message received!");
                                            info!("Contents were: {:?}", String::from_utf8(buf.to_vec()).unwrap());
                                        },
                                        NetworkMessage::InvalidMessage => {
                                            info!("Invalid message received!");
                                            info!("Contents were: {:?}", String::from_utf8(buf.to_vec()).unwrap());
                                        }                                        
                                    }
                                    
                                },
                                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                                    break;
                                }
                                e => (),
                            }
                        }
                    }
                }
            }
        }
    }
}