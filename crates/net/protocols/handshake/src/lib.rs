pub mod swarm {
    pub mod handshake {
        include!(concat!(env!("OUT_DIR"), "/swarm.handshake.rs"));
    }
}

use libp2p::StreamProtocol;
pub use swarm::handshake;

const HANDSHAKE_PROTOCOL: StreamProtocol = StreamProtocol::new("/swarm/handshake/11.0.0/handshake");



#[cfg(test)]
mod tests {
    use super::*;

    // #[test]
    // fn it_works() {
    //     let result = add(2, 2);
    //     assert_eq!(result, 4);
    // }
}
