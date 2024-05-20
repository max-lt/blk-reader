use bitcoin::p2p::Magic;
use bitcoin::network::Network;

pub const MAGIC: Magic = Magic::BITCOIN;
pub const NETWORK: Network = Network::Bitcoin;
pub const MAX_ORPHAN_BLOCKS: usize = 10_000;
