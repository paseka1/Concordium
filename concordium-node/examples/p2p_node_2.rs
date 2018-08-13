#![feature(box_syntax, box_patterns)]
extern crate bytes;
extern crate mio;
extern crate p2p_client;
#[macro_use]
extern crate log;
extern crate env_logger;
use env_logger::Env;
use p2p_client::common::{NetworkMessage, NetworkPacket, NetworkRequest, P2PNodeId};
use p2p_client::configuration;
use p2p_client::p2p::*;
use std::sync::mpsc;
use std::sync::Arc;
use std::{thread, time};

fn main() {
    let conf = configuration::parse_config();

    let env = if conf.debug {
        Env::default().filter_or("MY_LOG_LEVEL", "debug")
    } else {
        Env::default().filter_or("MY_LOG_LEVEL", "info")
    };

    env_logger::init_from_env(env);
    info!("Starting up {} version {}!",
          p2p_client::APPNAME,
          p2p_client::VERSION);

    let listen_port = match conf.listen_port {
        Some(x) => x,
        _ => 8889,
    };

    info!("Debuging enabled {}", conf.debug);

    let (pkt_in, pkt_out) = mpsc::channel::<Arc<Box<NetworkMessage>>>();

    let _guard_pkt = thread::spawn(move || loop {
        if let Ok(ref msg) = pkt_out.recv() {
            match *msg.clone() {
                box NetworkMessage::NetworkPacket(NetworkPacket::DirectMessage(_, _, ref msg),
                                                  _,
                                                  _) => {
                    info!("DirectMessage with {:?} received", msg)
                }
                box NetworkMessage::NetworkPacket(NetworkPacket::BroadcastedMessage(_,
                                                                                    ref msg),
                                                  _,
                                                  _) => {
                    info!("BroadcastedMessage with {:?} received", msg)
                }
                box NetworkMessage::NetworkRequest(NetworkRequest::BanNode(_, ref x), _, _) => {
                    info!("Ban node request for {:?}", x)
                }
                box NetworkMessage::NetworkRequest(NetworkRequest::UnbanNode(_, ref x), _, _) => {
                    info!("Unban node requets for {:?}", x)
                }
                _ => {}
            }
        }
    });

    let mut node = if conf.debug {
        let (sender, receiver) = mpsc::channel();
        let _guard = thread::spawn(move || loop {
                                       if let Ok(msg) = receiver.recv() {
                                           match msg {
                                               P2PEvent::ConnectEvent(ip, port) => {
                                                   info!("Received connection from {}:{}", ip, port)
                                               }
                                               P2PEvent::DisconnectEvent(msg) => {
                                                   info!("Received disconnect for {}", msg)
                                               }
                                               P2PEvent::ReceivedMessageEvent(node_id) => {
                                                   info!("Received message from {:?}", node_id)
                                               }
                                               P2PEvent::SentMessageEvent(node_id) => {
                                                   info!("Sent message to {:?}", node_id)
                                               }
                                               P2PEvent::InitiatingConnection(ip, port) => {
                                                   info!("Initiating connection to {}:{}", ip, port)
                                               }
                                           }
                                       }
                                   });
        P2PNode::new(conf.id, listen_port, pkt_in, Some(sender))
    } else {
        P2PNode::new(conf.id, listen_port, pkt_in, None)
    };

    node.connect("127.0.0.1".parse().unwrap(), 8888);

    let _th = node.spawn();

    let _app = thread::spawn(move || loop {
                                 info!("Sending one packet");
                                 node.send_message(Some(P2PNodeId::from_string(&"c19cd000746763871fae95fcdd4508dfd8bf725f9767be68c3038df183527bb2".to_string()).unwrap()), &String::from("Hello world!").as_bytes().to_vec(), false);
                                 info!("Sleeping for 1 second");
                                 thread::sleep(time::Duration::from_secs(1));
                             });

    _app.join().unwrap();
}
