#![feature(plugin, use_extern_macros, proc_macro_path_invoc)]
#![plugin(tarpc_plugins)]
extern crate p2p_client;
#[macro_use]
extern crate tarpc;
#[macro_use]
extern crate log;
extern crate env_logger;
extern crate bytes;
extern crate mio;
use p2p_client::configuration;
use p2p_client::common::P2PNodeId;
use tarpc::sync::{client, server};
use tarpc::sync::client::ClientExt;
use tarpc::util::{FirstSocketAddr, Never};
use std::sync::mpsc;
use std::thread;
use p2p_client::p2p::*;
use p2p_client::common::{NetworkRequest,NetworkPacket,NetworkMessage};
use mio::Events;
use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

service! {
    rpc peer_connect(ip: String, port: u16) -> bool;
    rpc send_message(id: Option<String>, msg: String, broadcast: bool) -> bool;
}

#[derive(Clone)]
struct HelloServer {
    node: RefCell<P2PNode>,
}

impl HelloServer {
    pub fn new() -> Self {
        let conf = configuration::parse_config();

        let listen_port = match conf.listen_port {
            Some(x) => x,
            _ => 8888,
        };

        info!("Debuging enabled {}", conf.debug);

        let (pkt_in,pkt_out) = mpsc::channel();

        let _guard_pkt = thread::spawn(move|| {
            loop {
                if let Ok(msg) = pkt_out.recv() {
                    match msg {
                        NetworkMessage::NetworkPacket(NetworkPacket::DirectMessage(_,_, msg),_,_) => info!( "DirectMessage with text {} received", msg),
                        NetworkMessage::NetworkPacket(NetworkPacket::BroadcastedMessage(_,msg),_,_) => info!("BroadcastedMessage with text {} received", msg),
                        NetworkMessage::NetworkRequest(NetworkRequest::BanNode(_, x),_,_)  => info!("Ban node request for {:x}", x.get_id()),
                        NetworkMessage::NetworkRequest(NetworkRequest::UnbanNode(_, x), _, _) => info!("Unban node requets for {:x}", x.get_id()), 
                        _ => {}
                    }
                }
            }
        });

        let mut node = if conf.debug {
            let (sender, receiver) = mpsc::channel();
            let _guard = thread::spawn(move|| {
                loop {
                    if let Ok(msg) = receiver.recv() {
                        match msg {
                            P2PEvent::ConnectEvent(ip, port) => info!("Received connection from {}:{}", ip, port),
                            P2PEvent::DisconnectEvent(msg) => info!("Received disconnect for {}", msg),
                            P2PEvent::ReceivedMessageEvent(node_id) => info!("Received message from {:?}", node_id),
                            P2PEvent::SentMessageEvent(node_id) => info!("Sent message to {:?}", node_id),
                            P2PEvent::InitiatingConnection(ip,port) => info!("Initiating connection to {}:{}", ip, port),
                        }
                    }
                }
            });
            P2PNode::new(conf.id, listen_port, pkt_in, Some(sender))
        } else {
            P2PNode::new(conf.id, listen_port, pkt_in, None)
        };

        HelloServer {
            node: RefCell::new(node),
        }
    }
}

impl SyncService for HelloServer {
    fn peer_connect(&self, ip: String, port: u16) -> Result<bool, Never> {
        info!("Connecting to IP: {} and port: {}!", ip, port);
        self.node.borrow_mut().connect(ip.parse().unwrap(), port);
        Ok(true)
    }

    fn send_message(&self, id: Option<String>, msg: String, broadcast: bool) -> Result<bool, Never> {
        info!("Sending message to ID: {:?} with contents: {}. Broadcast? {}", id, msg, broadcast);
        let id = match id {
            Some(x) => Some(P2PNodeId::from_string(x)),
            None => None,
        };

        self.node.borrow_mut().send_message(id, msg, broadcast);
        Ok(true)
    }
}

fn main() {
    let conf = configuration::parse_config();
    info!("I'm your new coin! Network enabled: {}, should connect to {}", conf.network, conf.remote_ip.unwrap_or(String::from("nothing")));
    //println!("calling lib in c {} version found is {}", ffi::is_present(), ffi::version());

    env_logger::init();
    info!("Starting up!");

    
    let mut events = Events::with_capacity(1024);

    let (tx, rx) = mpsc::channel();
    let serv = HelloServer::new();
    let th1 = thread::spawn(move || {
        let th2 = serv.node.borrow_mut().spawn();
        let mut handle = match serv.listen("localhost:0", server::Options::default()) {
            Ok(x) => x,
            Err(e) => panic!("Couldn't start RPC service!"),
        };

        match tx.send(handle.addr()) {
            Ok(x) => {},
            Err(e) => info!("Couldn't send handle addr, {:?}", e),
        };

        handle.run();
        th2.join();
    });



    let client = SyncClient::connect(rx.recv().unwrap(), client::Options::default()).unwrap();
    thread::sleep(Duration::from_secs(5));


    th1.join().unwrap();
}