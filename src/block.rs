use std::fs;
use std::io::Error;
use std::io::ErrorKind;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;

use bitcoin::block::Header;
use bitcoin::consensus::encode;
use bitcoin::hashes::Hash;
use bitcoin::p2p::Magic;
use bitcoin::Block;
use bitcoin::BlockHash;
use bitcoin::Transaction;
use bitcoin::Txid;

use crate::chain::Chain;
use crate::chain::GetBlockIds;
use crate::xor::read_xor_key;
use crate::xor::xor_in_place;
use crate::xor::XorKey;
use crate::xor::XOR_KEY_LEN;

/// Raw blocks buffered between the reader thread and the decoder pool
const RAW_CHANNEL_CAP: usize = 64;

/// Decoded blocks buffered between the decoder pool and the consumer
const OUT_CHANNEL_CAP: usize = 64;

/// A fully decoded block, with the txid of every transaction
/// precomputed by the decoder pool
#[derive(Debug, Clone)]
pub struct DecodedBlock {
    pub blk_index: u32,
    pub blk_path: Arc<str>,
    pub offset: u64,
    pub block_hash: BlockHash,
    pub header: Header,
    pub txdata: Vec<Transaction>,
    /// Txid of `txdata[i]`
    pub txids: Vec<Txid>,
}

impl DecodedBlock {
    pub fn into_block(self) -> Block {
        Block {
            header: self.header,
            txdata: self.txdata,
        }
    }
}

impl GetBlockIds<BlockHash> for DecodedBlock {
    fn get_block_id(&self) -> BlockHash {
        self.block_hash
    }

    fn get_block_prev_id(&self) -> BlockHash {
        self.header.prev_blockhash
    }
}

/// Raw block bytes as stored in a blk file, before decoding
struct RawBlock {
    blk_index: u32,
    blk_path: Arc<str>,
    offset: u64,
    bytes: Vec<u8>,
}

/// Messages sent to the consumer thread
enum Output {
    Block(DecodedBlock),
    /// The reader is done queueing this file. Blocks of this file may
    /// still be in flight in the decoder pool when this is received.
    FileDone(String),
    Error(Error),
}

/// Why the reader thread stopped in the middle of a file
enum ReadStop {
    /// The consumer hung up or the stop flag was raised
    Stop,
    Error(Error),
}

impl From<Error> for ReadStop {
    fn from(e: Error) -> Self {
        ReadStop::Error(e)
    }
}

pub struct BlockReader<'call> {
    height: u32,
    chain: Chain<BlockHash, DecodedBlock>,
    block_cb: Option<Box<dyn Fn(DecodedBlock, u32) + 'call>>,
    file_cb: Option<Box<dyn Fn(String, u32, u32) + 'call>>,
    options: BlockReaderOptions,
}

pub struct BlockReaderOptions {
    pub max_blocks: Option<u32>,
    pub max_orphans: Option<usize>,
    pub max_blk_files: Option<usize>,
    /// Network magic bytes (defaults to mainnet)
    pub magic: Magic,
    /// Number of decoder threads (defaults to the available parallelism
    /// minus the reader and consumer threads, capped at 12)
    pub decode_workers: Option<usize>,
    pub stop_flag: Arc<AtomicBool>,
}

impl Default for BlockReaderOptions {
    fn default() -> Self {
        BlockReaderOptions {
            max_blocks: None,
            max_orphans: Some(10_000),
            max_blk_files: None,
            magic: Magic::BITCOIN,
            decode_workers: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl<'a> BlockReader<'a> {
    pub fn new(options: BlockReaderOptions) -> BlockReader<'a> {
        BlockReader {
            height: 0,
            chain: Chain::new(BlockHash::all_zeros()),
            block_cb: None,
            file_cb: None,
            options,
        }
    }

    pub fn set_block_cb(&mut self, block_cb: Box<dyn Fn(DecodedBlock, u32) + 'a>) {
        self.block_cb = Some(block_cb);
    }

    pub fn set_file_cb(&mut self, file_cb: Box<dyn Fn(String, u32, u32) + 'a>) {
        self.file_cb = Some(file_cb);
    }

    /// Read the directory and return a list of files
    fn read_dir(&self, dir_path: &std::path::Path) -> Result<Vec<String>, Error> {
        let mut entries: Vec<String> = fs::read_dir(dir_path)?
            .filter_map(Result::ok)
            .map(|d| d.path())
            .filter(|d| d.is_file() && d.extension().is_some())
            .filter_map(|d| d.to_str().map(str::to_string))
            .filter(|s| s.contains("/blk") && s.ends_with(".dat"))
            .collect();

        entries.sort();

        match self.options.max_blk_files {
            Some(max_blk_files) => entries.truncate(max_blk_files),
            None => (),
        }

        return Ok(entries);
    }

    /// Read the blk files of a directory through a three-stage pipeline:
    /// one reader thread (I/O + de-obfuscation + record splitting), a pool
    /// of decoder threads (transaction decoding + txid computation), and
    /// the calling thread as consumer. Out-of-order decoding is absorbed
    /// by the chain, which already handles out-of-order blk files, so the
    /// callbacks are invoked in block height order on the calling thread.
    pub fn read(&mut self, dir_path: &std::path::Path) -> Result<(), Error> {
        let xor_key = read_xor_key(dir_path)?;
        let entries = BlockReader::read_dir(&self, dir_path)?;

        let workers = self.options.decode_workers.unwrap_or_else(|| {
            thread::available_parallelism()
                .map(|n| n.get().saturating_sub(2))
                .unwrap_or(4)
                .clamp(1, 12)
        });

        let magic = self.options.magic;
        let stop_flag = Arc::clone(&self.options.stop_flag);

        thread::scope(|scope| {
            let (raw_tx, raw_rx) = mpsc::sync_channel::<RawBlock>(RAW_CHANNEL_CAP);
            let (out_tx, out_rx) = mpsc::sync_channel::<Output>(OUT_CHANNEL_CAP);
            let raw_rx = Arc::new(Mutex::new(raw_rx));

            {
                let out_tx = out_tx.clone();
                let stop_flag = Arc::clone(&stop_flag);
                scope.spawn(move || read_files(entries, xor_key, magic, &stop_flag, raw_tx, out_tx));
            }

            for _ in 0..workers {
                let raw_rx = Arc::clone(&raw_rx);
                let out_tx = out_tx.clone();
                scope.spawn(move || decode_worker(raw_rx, out_tx));
            }

            // The consumer only reads from the workers and the reader
            drop(out_tx);

            let mut last_time: u32 = 0;

            // Dropping out_rx (on break, return or loop end) disconnects the
            // channels, which shuts down the reader and the workers
            for output in out_rx.iter() {
                match output {
                    Output::Error(e) => return Err(e),
                    Output::FileDone(file_path) => {
                        if let Some(ref file_cb) = self.file_cb {
                            file_cb(file_path, self.height, last_time);
                        }
                    }
                    Output::Block(block) => {
                        last_time = block.header.time;
                        self.insert(block);

                        // Stop signal received
                        if stop_flag.load(Ordering::Relaxed) {
                            println!("Stop signal received");
                            break;
                        }

                        // We reached the limit of blocks, stop here
                        if self.max_height_reached() {
                            println!(
                                "Reached limit of blocks. Next block is {} {}",
                                self.height,
                                self.chain.next_id()
                            );
                            break;
                        }

                        // We reached the limit of orphan blocks, stop here
                        if self.max_orphans_reached() {
                            println!("Reached limit of orphan blocks {}", self.orphans());
                            break;
                        }
                    }
                }
            }

            Ok(())
        })
    }

    /// Insert a block into the index
    fn insert(&mut self, block: DecodedBlock) {
        self.chain.insert(block);

        while self.chain.longest_chain_depth() >= 10 {
            match self.chain.pop_head() {
                Some(block) => {
                    self.push_block(block);
                    if self.max_height_reached() {
                        return;
                    }
                }
                None => return,
            }
        }
    }

    fn push_block(&mut self, block: DecodedBlock) {
        let height = self.height;

        self.height += 1;

        // Call the callback function
        if let Some(ref block_cb) = self.block_cb {
            block_cb(block, height);
        }
    }

    /// Return the number of orphans blocks
    pub fn orphans(&self) -> usize {
        self.chain.orphans()
    }

    /// Return the height of the last block
    pub fn height(&self) -> u32 {
        self.height
    }

    fn max_height_reached(&self) -> bool {
        match self.options.max_blocks {
            Some(max_blocks) => self.height >= max_blocks,
            None => false,
        }
    }

    fn max_orphans_reached(&self) -> bool {
        match self.options.max_orphans {
            Some(max_orphans) => self.orphans() >= max_orphans,
            None => false,
        }
    }
}

/// Reader thread: read each blk file, split it into raw block records
/// and queue them for the decoder pool
fn read_files(
    entries: Vec<String>,
    xor_key: XorKey,
    magic: Magic,
    stop_flag: &AtomicBool,
    raw_tx: mpsc::SyncSender<RawBlock>,
    out_tx: mpsc::SyncSender<Output>,
) {
    for file_path in entries {
        match read_file(&file_path, xor_key, magic, stop_flag, &raw_tx) {
            Ok(()) => {
                if out_tx.send(Output::FileDone(file_path)).is_err() {
                    return;
                }
            }
            Err(ReadStop::Stop) => return,
            Err(ReadStop::Error(e)) => {
                let _ = out_tx.send(Output::Error(e));
                return;
            }
        }
    }
}

/// Read a single blk file and queue its raw block records
fn read_file(
    file_path: &str,
    xor_key: XorKey,
    magic: Magic,
    stop_flag: &AtomicBool,
    raw_tx: &mpsc::SyncSender<RawBlock>,
) -> Result<(), ReadStop> {
    let blk_index = std::path::Path::new(file_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("blk"))
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Invalid blk file name: {}", file_path),
            )
        })?;

    let mut buf = fs::read(file_path)?;
    xor_in_place(&mut buf, xor_key);

    let blk_path: Arc<str> = Arc::from(file_path);
    let magic_bytes = magic.to_bytes();

    let mut offset: usize = 0;

    while offset + 8 <= buf.len() {
        if buf[offset..offset + 4] != magic_bytes {
            // Bitcoin Core preallocates blk files: raw zero bytes (XORed
            // with the key in `buf`) mark the end of the written data
            let zeros: [u8; 4] = std::array::from_fn(|i| xor_key[(offset + i) % XOR_KEY_LEN]);
            if buf[offset..offset + 4] == zeros {
                return Ok(());
            }

            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Magic is not correct in {} offset={}", file_path, offset),
            )
            .into());
        }

        let size = u32::from_le_bytes(buf[offset + 4..offset + 8].try_into().unwrap()) as usize;

        // At least a header, at most the maximum serialized block size
        if !(80..=4_000_000).contains(&size) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Invalid block size {} in {} offset={}", size, file_path, offset),
            )
            .into());
        }

        if offset + 8 + size > buf.len() {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                format!("Truncated block in {} offset={}", file_path, offset),
            )
            .into());
        }

        let raw = RawBlock {
            blk_index,
            blk_path: Arc::clone(&blk_path),
            offset: offset as u64,
            bytes: buf[offset + 8..offset + 8 + size].to_vec(),
        };

        if raw_tx.send(raw).is_err() {
            return Err(ReadStop::Stop);
        }

        offset += 8 + size;

        if stop_flag.load(Ordering::Relaxed) {
            return Err(ReadStop::Stop);
        }
    }

    Ok(())
}

/// Decoder thread: decode raw blocks and compute their txids
fn decode_worker(raw_rx: Arc<Mutex<mpsc::Receiver<RawBlock>>>, out_tx: mpsc::SyncSender<Output>) {
    loop {
        // Holding the lock while waiting is fine: it makes the idle
        // workers queue on the mutex instead of the channel
        let raw = match raw_rx.lock().unwrap().recv() {
            Ok(raw) => raw,
            Err(_) => return, // reader is done
        };

        let output = match decode(raw) {
            Ok(block) => Output::Block(block),
            Err(e) => Output::Error(e),
        };

        if out_tx.send(output).is_err() {
            return; // consumer is done
        }
    }
}

fn decode(raw: RawBlock) -> Result<DecodedBlock, Error> {
    let block: Block = encode::deserialize(&raw.bytes).map_err(|e| {
        Error::new(
            ErrorKind::InvalidData,
            format!(
                "Failed to decode block in {} offset={}: {}",
                raw.blk_path, raw.offset, e
            ),
        )
    })?;

    let txids = block.txdata.iter().map(|tx| tx.compute_txid()).collect();

    Ok(DecodedBlock {
        blk_index: raw.blk_index,
        blk_path: raw.blk_path,
        offset: raw.offset,
        block_hash: block.header.block_hash(),
        header: block.header,
        txdata: block.txdata,
        txids,
    })
}
