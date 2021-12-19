//use crate::core::thread_pool::ThreadPool;
use super::server::P2pServer;
use super::connection::Connection;
use super::error::P2pError;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::mpsc::{Sender, Receiver, channel};

enum ThreadMessage {
    Listen(Arc<Connection>), // listen to this connection
    Stop // stop a thread
}

enum Message {
    SendBytes(Vec<u8>), // bytes
    Exit, // Exit a connection
}

// MultiThreadServer use as much as peers connected
// 1 peer = 1 thread
// + 1 thread for incoming new connections
pub struct MultiThreadServer {
    peer_id: u64, // unique peer id
    tag: Option<String>, // node tag sent on handshake
    max_peers: usize, // max peers accepted by this server
    bind_address: String, // ip:port address to receive connections
    connections: Mutex<HashMap<u64, Arc<Connection>>>, // all connections accepted
    thread_channels: Mutex<HashMap<usize, Sender<Message>>>, // all unique channels for each thread
    channels: Mutex<HashMap<u64, usize>>, // all peers id linked to a thread (channel)
    sender: Mutex<Sender<ThreadMessage>>, // sender for all threads to send a new connection to handle for available thread
    receiver: Mutex<Receiver<ThreadMessage>> // receiver used in all threads to get new connection to handle
}

impl Drop for MultiThreadServer {
    fn drop(&mut self) {
        self.stop();
    }
}

impl P2pServer for MultiThreadServer {
    fn new(peer_id: u64, tag: Option<String>, max_peers: usize, bind_address: String) -> Self {
        if let Some(tag) = &tag {
            assert!(tag.len() > 0 && tag.len() <= 16);
        }

        // main channel used by ALL threads to receive new connections
        let (sender, receiver) = channel();
        MultiThreadServer {
            peer_id,
            tag,
            max_peers,
            bind_address,
            connections: Mutex::new(HashMap::new()),
            thread_channels: Mutex::new(HashMap::new()),
            channels: Mutex::new(HashMap::new()),
            sender: Mutex::new(sender),
            receiver: Mutex::new(receiver)
        }
    }

    fn start(&self) {
        use crossbeam::thread;
        thread::scope(|s| {
            s.spawn(|_| {
                self.listen_new_connections();
            });

            for id in 0..self.get_max_peers() {
                s.spawn(move |_| {
                    let mut buf: [u8; 512] = [0; 512];
                    let (sender, receiver) = channel();
                    match self.thread_channels.lock() {
                        Ok(mut channels) => {
                            channels.insert(id, sender);
                        },
                        Err(e) => panic!("Error while trying to lock thread_channels: {}", e)
                    };

                    'main: loop {
                        let connection = match self.receiver.lock() {
                            Ok(chan) => match chan.recv() {
                                Ok(msg) => {
                                    match msg {
                                        ThreadMessage::Stop => break 'main,
                                        ThreadMessage::Listen(connection) => connection
                                    }
                                },
                                Err(e) => panic!("Error while trying to get new connection: {}", e)
                            },
                            Err(e) => panic!("Error while trying to get new connection: {}", e) 
                        };

                        match self.channels.lock() {
                            Ok(mut channels) => {
                                channels.insert(connection.get_peer_id(), id);
                            },
                            Err(e) => panic!("Error while trying to get channels lock: {}", e)
                        }

                        'con: while !connection.is_closed() {
                            while let Ok(msg) = receiver.try_recv() {
                                match msg {
                                    Message::Exit => break 'con,
                                    Message::SendBytes(bytes) => {
                                        if let Err(e) = connection.send_bytes(&bytes) {
                                            println!("Error while trying to send bytes to {}: {}", connection, e);
                                            break;
                                        }
                                    }
                                }
                            }
                            self.listen_connection(&mut buf, &connection);
                        }
                    };
                });
            }
        }).unwrap();
    }

    fn stop(&self) {
        match self.get_connections_id() {
            Ok(peers) => {
                for peer in peers { // close all connections & remove them from threads so they can exit safely
                    if let Err(e) = self.remove_connection(&peer) {
                        println!("Error while removing connection: {}", e);
                    }
                }
            },
            Err(e) => panic!("Couldn't lock connections: {}", e)
        };

        match self.sender.lock() {
            Ok(sender) => {
                for _ in 0..self.max_peers { // send X messages so all threads can read it from main chan
                    if let Err(e) = sender.send(ThreadMessage::Stop) {
                        println!("Error while trying to close threads: {}", e);
                    }
                }
            },
            Err(e) => panic!("Couldn't lock thread_channels: {}", e)
        };
    }

    fn get_tag(&self) -> &Option<String> {
        &self.tag
    }

    fn get_max_peers(&self) -> usize {
        self.max_peers
    }

    fn get_peer_id(&self) -> u64 {
        self.peer_id
    }

    fn accept_new_connections(&self) -> bool {
        self.get_peer_count() < self.max_peers
    }

    fn get_peer_count(&self) -> usize {
        self.connections.lock().unwrap().len()
    }

    fn get_slots_available(&self) -> usize {
        self.max_peers - self.get_peer_count()
    }

    fn is_connected_to(&self, peer_id: &u64) -> Result<bool, P2pError> {
        match self.connections.lock() {
            Ok(connections) => {
                Ok(self.peer_id == *peer_id || connections.contains_key(peer_id))
            },
            Err(e) => Err(P2pError::OnLock(format!("is connected to {}: {}", peer_id, e)))
        }
    }

    fn is_connected_to_addr(&self, peer_addr: &SocketAddr) -> Result<bool, P2pError> {
        for connection in self.get_connections()? {
            if *connection.get_peer_address() == *peer_addr {
                return Ok(true)
            }
        }
        Ok(false)
    }

    fn is_multi_threaded(&self) -> bool {
        true
    }

    fn get_bind_address(&self) -> &String {
        &self.bind_address
    }
 
    fn get_connection(&self, peer_id: &u64) -> Result<Arc<Connection>, P2pError> {
        match self.connections.lock() {
            Ok(ref connections) => {
                match connections.get(peer_id) {
                    Some(connection) => Ok(connection.clone()), // TODO found a better way instead of clone
                    None => Err(P2pError::PeerNotFound(*peer_id))
                }
            }
            Err(e) => {
                Err(P2pError::OnLock(format!("trying to get {}: {}", peer_id, e)))
            }
        }
    }

    fn add_connection(&self, connection: Connection) -> Result<(), P2pError> {
        match self.connections.lock() {
            Ok(mut connections) => {
                let peer_id = connection.get_peer_id();
                let arc_connection = Arc::new(connection);
                match connections.insert(peer_id, arc_connection.clone()) {
                    Some(c) => {
                        panic!("Connection {} already exists!", c)
                        //Err(P2pError::PeerIdAlreadyUsed(peer_id)) // should not happen
                    },
                    None => match self.sender.lock() {
                        Ok(sender) => {
                            if let Err(e) = sender.send(ThreadMessage::Listen(arc_connection)) {
                                Err(P2pError::OnChannelMessage(peer_id, format!("{}", e)))
                            } else {
                                Ok(())
                            }
                        },
                        Err(e) => Err(P2pError::OnLock(format!("trying to get sender {}", e)))
                    }
                }
            }
            Err(e) => {
                Err(P2pError::OnLock(format!("trying to add {}: {}", connection, e)))
            }
        }
    }

    fn remove_connection(&self, peer_id: &u64) -> Result<(), P2pError> {
        match self.connections.lock().unwrap().remove(peer_id) {
            Some(connection) => {
                if !connection.is_closed() {
                    if let Err(e) = connection.close() {
                        return Err(P2pError::OnConnectionClose(format!("trying to remove {}: {}", peer_id, e)));
                    }
                }
                println!("{} disconnected", connection);
                match self.channels.lock() {
                    Ok(mut channels) => {
                        match channels.remove(peer_id) {
                            Some(thread_id) => match self.thread_channels.lock() {
                                Ok(channels) => match channels.get(&thread_id) {
                                    Some(sender) => {
                                        if let Err(e) = sender.send(Message::Exit) {
                                            Err(P2pError::OnChannelMessage(*peer_id, format!("{}", e)))
                                        } else {
                                            Ok(())
                                        }
                                    }
                                    None => panic!("No thread found for id {}", thread_id) // shouldn't happens
                                },
                                Err(e) => Err(P2pError::OnLock(format!("trying to get thread channels for thread {}: {}", thread_id, e)))
                            },
                            None => panic!("No channel found for a connection found!") // Shouldn't happen
                        }
                    }
                    Err(e) => {
                        Err(P2pError::OnLock(format!("trying to get channels for {}: {}", peer_id, e)))
                    }
                }
            },
            None => Err(P2pError::PeerNotFound(*peer_id)),
        }
    }

    fn get_connections(&self) -> Result<Vec<Arc<Connection>>, P2pError> {
        match self.connections.lock() {
            Ok(connections) => {
                Ok(connections.values().map(|arc| { arc.clone() }).collect()) // TODO Found a better way instead of cloning
            },
            Err(e) => Err(P2pError::OnLock(format!("trying to get connections: {}", e)))
        }
    }

    fn get_connections_id(&self) -> Result<Vec<u64>, P2pError> {
        match self.connections.lock() {
            Ok(connections) => {
                Ok(connections.values().map(|arc| { arc.get_peer_id() }).collect()) // TODO Found a better way instead of cloning
            },
            Err(e) => Err(P2pError::OnLock(format!("trying to get connections id: {}", e)))
        }
    }

    // notify the thread that own the target peer through channel
    fn send_to_peer(&self, peer_id: u64, bytes: Vec<u8>) -> Result<(), P2pError> {
        match self.channels.lock() {
            Ok(channels) => {
                match channels.get(&peer_id) {
                    Some(thread_id) => match self.thread_channels.lock() {
                        Ok(channels) => match channels.get(&thread_id) {
                            Some(sender) => {
                                if let Err(e) = sender.send(Message::SendBytes(bytes)) {
                                    Err(P2pError::OnChannelMessage(peer_id, format!("'SendBytes': {}", e)))
                                } else {
                                    Ok(())
                                }
                            },
                            None => panic!("No thread found for id {}", thread_id) // shouldn't happens
                        },
                        Err(e) => Err(P2pError::OnLock(format!("trying to get thread channels for thread {}: {}", thread_id, e)))
                    },
                    None => Err(P2pError::PeerNotFound(peer_id))
                }
            }
            Err(e) => {
                Err(P2pError::OnLock(format!("send_to_peer: {}", e))) 
            }
        }
    }
}