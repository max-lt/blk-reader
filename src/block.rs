use std::fs;
use std::io::Error;
use std::io::ErrorKind;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::io::Read as IoRead;
use std::io::Seek;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

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
use crate::xor::xor_in_place_at;
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
    /// Called after each insertion for every block of the buffered main
    /// branch, oldest first, as (index, branch_len, block): a live
    /// consumer rebuilds its unsealed-tip view from these calls
    buffer_cb: Option<Box<dyn Fn(usize, usize, &DecodedBlock) + 'call>>,
    /// Called with each block of a losing fork once its branch point is
    /// settled, after the winning block's own callback
    stale_cb: Option<Box<dyn Fn(DecodedBlock) + 'call>>,
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
    /// Chain root: blocks attach starting from the block whose prev hash
    /// equals this id (defaults to all-zeros, i.e. the genesis block).
    /// Set it to a known block hash to resume mid-chain with `start_at`.
    pub root: Option<BlockHash>,
    /// Start reading at this (blk file index, byte offset). Must point
    /// at a record boundary.
    pub start_at: Option<(u32, u64)>,
    /// Keep polling for new blk data at this interval instead of
    /// returning once the end of the files is reached. Partial records
    /// at the tail are then treated as data-not-yet-written rather than
    /// errors. Exit via `stop_flag`.
    pub follow: Option<Duration>,
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
            root: None,
            start_at: None,
            follow: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Parameters of the reader thread
struct ReaderConfig {
    dir: PathBuf,
    xor_key: XorKey,
    magic: Magic,
    max_blk_files: Option<usize>,
    start_at: Option<(u32, u64)>,
    follow: Option<Duration>,
}

impl<'a> BlockReader<'a> {
    pub fn new(options: BlockReaderOptions) -> BlockReader<'a> {
        let root = options.root.unwrap_or_else(BlockHash::all_zeros);

        BlockReader {
            height: 0,
            chain: Chain::new(root),
            block_cb: None,
            file_cb: None,
            buffer_cb: None,
            stale_cb: None,
            options,
        }
    }

    pub fn set_block_cb(&mut self, block_cb: Box<dyn Fn(DecodedBlock, u32) + 'a>) {
        self.block_cb = Some(block_cb);
    }

    pub fn set_file_cb(&mut self, file_cb: Box<dyn Fn(String, u32, u32) + 'a>) {
        self.file_cb = Some(file_cb);
    }

    pub fn set_buffer_cb(&mut self, buffer_cb: Box<dyn Fn(usize, usize, &DecodedBlock) + 'a>) {
        self.buffer_cb = Some(buffer_cb);
    }

    /// Called with each block of a losing fork once its branch point is
    /// settled (the winning side got buried deep enough to be sealed).
    /// Runs after the sealed block's own callback, so the fork point is
    /// already known to the consumer.
    pub fn set_stale_cb(&mut self, stale_cb: Box<dyn Fn(DecodedBlock) + 'a>) {
        self.stale_cb = Some(stale_cb);
    }

    /// Read the blk files of a directory through a three-stage pipeline:
    /// one reader thread (I/O + de-obfuscation + record splitting), a pool
    /// of decoder threads (transaction decoding + txid computation), and
    /// the calling thread as consumer. Out-of-order decoding is absorbed
    /// by the chain, which already handles out-of-order blk files, so the
    /// callbacks are invoked in block height order on the calling thread.
    pub fn read(&mut self, dir_path: &std::path::Path) -> Result<(), Error> {
        let xor_key = read_xor_key(dir_path)?;

        let reader_cfg = ReaderConfig {
            dir: dir_path.to_path_buf(),
            xor_key,
            magic: self.options.magic,
            max_blk_files: self.options.max_blk_files,
            start_at: self.options.start_at,
            follow: self.options.follow,
        };

        let workers = self.options.decode_workers.unwrap_or_else(|| {
            thread::available_parallelism()
                .map(|n| n.get().saturating_sub(2))
                .unwrap_or(4)
                .clamp(1, 12)
        });

        let stop_flag = Arc::clone(&self.options.stop_flag);

        thread::scope(|scope| {
            let (raw_tx, raw_rx) = mpsc::sync_channel::<RawBlock>(RAW_CHANNEL_CAP);
            let (out_tx, out_rx) = mpsc::sync_channel::<Output>(OUT_CHANNEL_CAP);
            let raw_rx = Arc::new(Mutex::new(raw_rx));

            {
                let out_tx = out_tx.clone();
                let stop_flag = Arc::clone(&stop_flag);
                scope.spawn(move || read_files(reader_cfg, &stop_flag, raw_tx, out_tx));
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

                        // Replay the buffered main branch to the live
                        // consumer (sealed blocks already left the chain)
                        if let Some(ref buffer_cb) = self.buffer_cb {
                            let mut len = 0;
                            self.chain.for_each_main(|_| len += 1);

                            let mut index = 0;
                            self.chain.for_each_main(|block| {
                                buffer_cb(index, len, block);
                                index += 1;
                            });
                        }

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

                    // Settling this block may have discarded a losing
                    // fork: report it after the winner
                    if let Some(ref stale_cb) = self.stale_cb {
                        for stale in self.chain.take_discarded() {
                            stale_cb(stale);
                        }
                    }

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

/// List the blk files of a directory as (index, path), sorted
fn scan_dir(dir: &Path, max_blk_files: Option<usize>) -> Result<Vec<(u32, String)>, Error> {
    let mut entries: Vec<(u32, String)> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|d| d.path())
        .filter(|p| p.is_file())
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?;
            let index = name.strip_prefix("blk")?.strip_suffix(".dat")?.parse().ok()?;
            Some((index, p.to_str()?.to_string()))
        })
        .collect();

    entries.sort();

    if let Some(max_blk_files) = max_blk_files {
        entries.truncate(max_blk_files);
    }

    Ok(entries)
}

/// Reader thread: read the blk files, split them into raw block records
/// and queue them for the decoder pool. In follow mode, keep polling the
/// directory and the tail of the last file for new data.
fn read_files(
    cfg: ReaderConfig,
    stop_flag: &AtomicBool,
    raw_tx: mpsc::SyncSender<RawBlock>,
    out_tx: mpsc::SyncSender<Output>,
) {
    // Position of the next byte to read (file index, offset)
    let mut pos: (u32, u64) = cfg.start_at.unwrap_or((0, 0));
    // Last file for which FileDone was sent
    let mut last_done: Option<u32> = None;

    loop {
        let entries = match scan_dir(&cfg.dir, cfg.max_blk_files) {
            Ok(entries) => entries,
            Err(e) => {
                let _ = out_tx.send(Output::Error(e));
                return;
            }
        };

        let last_index = entries.last().map(|(i, _)| *i).unwrap_or(0);

        for (index, file_path) in entries.iter() {
            if *index < pos.0 {
                continue;
            }

            let start_offset = if *index == pos.0 { pos.1 } else { 0 };

            match read_file(file_path, *index, start_offset, &cfg, stop_flag, &raw_tx) {
                Ok(end_offset) => {
                    pos = (*index, end_offset);

                    // In follow mode the last file can still grow: hold its
                    // FileDone until the next file appears (rollover)
                    let is_final = cfg.follow.is_none() || *index < last_index;
                    if is_final && last_done.map_or(true, |done| done < *index) {
                        last_done = Some(*index);
                        if out_tx.send(Output::FileDone(file_path.clone())).is_err() {
                            return;
                        }
                    }
                }
                Err(ReadStop::Stop) => return,
                Err(ReadStop::Error(e)) => {
                    let _ = out_tx.send(Output::Error(e));
                    return;
                }
            }

            if stop_flag.load(Ordering::Relaxed) {
                return;
            }
        }

        let Some(poll) = cfg.follow else {
            return;
        };

        thread::sleep(poll);

        if stop_flag.load(Ordering::Relaxed) {
            return;
        }
    }
}

/// Read a single blk file from `start_offset` and queue its raw block
/// records. Returns the offset of the first byte not consumed: the end
/// of the written data (zero tail), or the start of a partial record in
/// follow mode (data still being written by the node).
fn read_file(
    file_path: &str,
    blk_index: u32,
    start_offset: u64,
    cfg: &ReaderConfig,
    stop_flag: &AtomicBool,
    raw_tx: &mpsc::SyncSender<RawBlock>,
) -> Result<u64, ReadStop> {
    let mut file = fs::File::open(file_path)?;
    if start_offset > 0 {
        file.seek(std::io::SeekFrom::Start(start_offset))?;
    }

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    xor_in_place_at(&mut buf, cfg.xor_key, start_offset);

    let blk_path: Arc<str> = Arc::from(file_path);
    let magic_bytes = cfg.magic.to_bytes();
    let follow = cfg.follow.is_some();

    let mut offset: usize = 0; // relative to start_offset

    loop {
        let abs = start_offset + offset as u64;

        if offset + 8 > buf.len() {
            // Trailing bytes shorter than a record header: either the end
            // of the file, or a record being written (follow catches up on
            // the next poll)
            return Ok(abs);
        }

        if buf[offset..offset + 4] != magic_bytes {
            // Bitcoin Core preallocates blk files: raw zero bytes (XORed
            // with the key in `buf`) mark the end of the written data
            let zeros: [u8; 4] =
                std::array::from_fn(|i| cfg.xor_key[(abs as usize + i) % XOR_KEY_LEN]);
            if buf[offset..offset + 4] == zeros {
                return Ok(abs);
            }

            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Magic is not correct in {} offset={}", file_path, abs),
            )
            .into());
        }

        let size =
            u32::from_le_bytes(buf[offset + 4..offset + 8].try_into().unwrap()) as usize;

        // At least a header, at most the maximum serialized block size
        if !(80..=4_000_000).contains(&size) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Invalid block size {} in {} offset={}", size, file_path, abs),
            )
            .into());
        }

        if offset + 8 + size > buf.len() {
            // Partial record at the tail: in follow mode the node is still
            // writing it, retry from here on the next poll
            if follow {
                return Ok(abs);
            }

            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                format!("Truncated block in {} offset={}", file_path, abs),
            )
            .into());
        }

        let raw = RawBlock {
            blk_index,
            blk_path: Arc::clone(&blk_path),
            offset: abs,
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
