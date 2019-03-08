#![feature(box_syntax, box_patterns)]
extern crate bytes;
extern crate mio;
extern crate p2p_client;
#[macro_use]
extern crate log;
#[cfg(not(target_os = "windows"))]
extern crate grpciounix as grpcio;
#[cfg(target_os = "windows")]
extern crate grpciowin as grpcio;
extern crate consensus_sys;

#[cfg(test)]
mod tests {
    use grpcio::{ChannelBuilder, EnvBuilder};
    use p2p_client::network::{NetworkMessage, NetworkPacket, NetworkRequest};
    use p2p_client::connection::{ P2PEvent, P2PNodeMode };
    use p2p_client::p2p::p2p_node::{ P2PNode };
    use p2p_client::proto::*;
    use p2p_client::rpc::RpcServerImpl;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread;
    use consensus_sys::consensus::*;
    use std::sync::atomic::{ AtomicUsize, Ordering};

    static PORT_OFFSET: AtomicUsize = AtomicUsize::new(0);

    /// It returns next port available and it ensures that next `slot_size` ports will be 
    /// available too.
    ///
    /// # Arguments
    /// * `slot_size` - Size of blocked ports. It 
    ///
    /// # Example
    /// ```
    /// let port_range_1 = next_port_offset( 10);   // It will return 0, you can use from 0..9
    /// let port_range_2 = next_port_offset( 20);   // It will return 10, you can use from 10..19
    /// let port_range_3 = next_port_offset( 100);  // It will return 30, you can use from 20..129
    /// let port_range_4 = next_port_offset( 130);
    /// ```
    fn next_port_offset( slot_size: usize) -> u16 {
        PORT_OFFSET.fetch_add( slot_size, Ordering::SeqCst) as u16
    }

    #[test]
    #[ignore]
    pub fn test_consensus_tests() {
        ConsensusContainer::start_haskell();
        test_grpc_consensus();
        ConsensusContainer::stop_haskell();
    }

    pub fn test_grpc_consensus() {

        let port_node = next_port_offset( 2);

        let (pkt_in, pkt_out) = mpsc::channel::<Arc<Box<NetworkMessage>>>();

        let (genesis_data, private_data) =
            match ConsensusContainer::generate_data(0, 1) {
                Ok((genesis, private_data)) => (genesis.clone(), private_data.clone()),
                _ => panic!("Couldn't read haskell data"),
            };
        let mut consensus_container = ConsensusContainer::new(genesis_data);
        &consensus_container.start_baker(0, private_data.get(&(0 as i64)).unwrap().to_vec());

        let (sender, receiver) = mpsc::channel();
        let _guard =
            thread::spawn(move || {
                              loop {
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
                                          P2PEvent::JoinedNetwork(peer, network_id) => {
                                              info!("Peer {} joined network {}",
                                                    peer.id().to_string(),
                                                    network_id);
                                          }
                                          P2PEvent::LeftNetwork(peer, network_id) => {
                                              info!("Peer {} left network {}",
                                                    peer.id().to_string(),
                                                    network_id);
                                          }
                                      }
                                  }
                              }
                          });
        let node = P2PNode::new(None,
                                Some("127.0.0.1".to_string()),
                                18888+port_node,
                                None,
                                None,
                                pkt_in,
                                Some(sender),
                                P2PNodeMode::NormalPrivateMode,
                                None,
                                vec![],
                                100);

        let mut _node_self_clone = node.clone();

        let _guard_pkt = thread::spawn(move || {
                                           loop {
                                               if let Ok(ref outer_msg) = pkt_out.recv() {
                                                   match *outer_msg.clone() {
                                                   box NetworkMessage::NetworkPacket(NetworkPacket::DirectMessage(_, ref msgid, _, ref nid, ref msg), _, _) => info!("DirectMessage/{}/{} with {:?} received", nid, msgid, msg),
                                                   box NetworkMessage::NetworkPacket(NetworkPacket::BroadcastedMessage(_,ref msgid, ref nid, ref msg), _, _) => {
                                                       info!("BroadcastedMessage/{}/{} with {:?} received", nid, msgid, msg);
                                                       _node_self_clone.send_message(None, *nid, Some(msgid.clone()), &msg, true).map_err(|e| panic!(e)).ok();
                                                   }
                                                   box NetworkMessage::NetworkRequest(NetworkRequest::BanNode(_, ref x), _, _) => info!("Ban node request for {:?}", x),
                                                   box NetworkMessage::NetworkRequest(NetworkRequest::UnbanNode(_, ref x), _, _) => info!("Unban node requets for {:?}", x),
                                                   _ => {}
                                               }
                                               }
                                           }
                                       });

        let mut rpc_serv = RpcServerImpl::new(node.clone(),
                                                None,
                                              Some(consensus_container.clone()),
                                              "127.0.0.1".to_string(),
                                              11000+port_node,
                                              "rpcadmin".to_string());
        rpc_serv.start_server().expect("rpc");

        let env = Arc::new(EnvBuilder::new().build());
        let ch = ChannelBuilder::new(env).connect(&format!("127.0.0.1:{}", 11000+port_node));

        let client = P2PClient::new(ch);

        let mut req_meta_builder = ::grpcio::MetadataBuilder::new();
        req_meta_builder.add_str("Authentication", "rpcadmin")
                        .unwrap();
        let meta_data = req_meta_builder.build();

        let call_options = ::grpcio::CallOption::default().headers(meta_data.clone());
        match client.get_best_block_info_opt(&Empty::new(), call_options) {
            Ok(ref res) => {
                assert!(res.best_block_info.contains("globalState"));
            },
            _ => panic!("Didn't  get right result from GetBestBlockInfo"),
        }

        /*
        TODO - disabled for now until Acorn is ready in master

        const TEST_TRANSACTION:&str = &"{\"txAddr\":\"31\",\"txSender\":\"53656e6465723a203131\",\"txMessage\":\"Increment\",\"txNonce\":\"de8bb42d9c1ea10399a996d1875fc1a0b8583d21febc4e32f63d0e7766554dc1\"}";

        let call_options = ::grpcio::CallOption::default().headers(meta_data.clone());
        let mut message = PoCSendTransactionMessage::new();
        message.set_network_id(100);
        message.set_message_content(TEST_TRANSACTION.to_string());
        match client.po_c_send_transaction_opt(&message, call_options) {
            Ok(ref res) => {
                assert_eq!(res.value, true);
            },
            _ => panic!("Didn't get respones back from sending transaction"),
        }
        */
        consensus_container.stop_baker(0);
    }
}
