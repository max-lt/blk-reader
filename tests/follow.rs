use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use bitcoin::absolute::LockTime;
use bitcoin::block::Header;
use bitcoin::hashes::Hash;
use bitcoin::transaction::Version;
use bitcoin::Amount;
use bitcoin::Block;
use bitcoin::BlockHash;
use bitcoin::CompactTarget;
use bitcoin::OutPoint;
use bitcoin::ScriptBuf;
use bitcoin::Sequence;
use bitcoin::Transaction;
use bitcoin::TxIn;
use bitcoin::TxMerkleNode;
use bitcoin::TxOut;
use bitcoin::Witness;

use blk_reader::BlockReader;
use blk_reader::BlockReaderOptions;

/// The reader validates neither PoW nor merkle roots, so a synthetic
/// chain only needs consistent prev hashes
fn make_block(prev: BlockHash, n: u32) -> Block {
    let tx = Transaction {
        version: Version::ONE,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::from(n.to_le_bytes().to_vec()),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(50),
            script_pubkey: ScriptBuf::new(),
        }],
    };

    Block {
        header: Header {
            version: bitcoin::block::Version::ONE,
            prev_blockhash: prev,
            merkle_root: TxMerkleNode::all_zeros(),
            time: n,
            bits: CompactTarget::from_consensus(0x1d00ffff),
            nonce: n,
        },
        txdata: vec![tx],
    }
}

fn make_chain(count: u32) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut prev = BlockHash::all_zeros();
    for n in 0..count {
        let block = make_block(prev, n);
        prev = block.block_hash();
        blocks.push(block);
    }
    blocks
}

fn record(block: &Block) -> Vec<u8> {
    let bytes = bitcoin::consensus::encode::serialize(block);
    let mut rec = Vec::with_capacity(8 + bytes.len());
    rec.extend_from_slice(&bitcoin::p2p::Magic::BITCOIN.to_bytes());
    rec.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    rec.extend_from_slice(&bytes);
    rec
}

fn write_atomic(path: &std::path::Path, data: &[u8]) {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timeout waiting for {}", what);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("blkreader-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn follow_growth_and_rollover() {
    let dir = temp_dir("follow");
    let blocks = make_chain(30);
    let recs: Vec<Vec<u8>> = blocks.iter().map(record).collect();
    let cat = |range: std::ops::Range<usize>| -> Vec<u8> {
        recs[range].iter().flatten().copied().collect()
    };

    // First 10 blocks + preallocated zero tail
    let mut f0 = cat(0..10);
    f0.extend_from_slice(&[0u8; 4096]);
    write_atomic(&dir.join("blk00000.dat"), &f0);

    let stop = Arc::new(AtomicBool::new(false));
    let options = BlockReaderOptions {
        follow: Some(Duration::from_millis(20)),
        stop_flag: Arc::clone(&stop),
        ..Default::default()
    };

    let delivered: Arc<Mutex<Vec<(u32, BlockHash)>>> = Arc::new(Mutex::new(Vec::new()));
    let files_done: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let branch: Arc<Mutex<Vec<BlockHash>>> = Arc::new(Mutex::new(Vec::new()));

    let handle = {
        let dir = dir.clone();
        let delivered = Arc::clone(&delivered);
        let files_done = Arc::clone(&files_done);
        let branch = Arc::clone(&branch);
        std::thread::spawn(move || {
            let mut reader = BlockReader::new(options);
            reader.set_buffer_cb(Box::new({
                let branch = Arc::clone(&branch);
                move |index, len, block| {
                    let mut branch = branch.lock().unwrap();
                    if index == 0 {
                        branch.clear();
                    }
                    branch.push(block.block_hash);
                    assert!(branch.len() <= len);
                }
            }));
            reader.set_block_cb(Box::new({
                let delivered = Arc::clone(&delivered);
                move |block, height| {
                    delivered.lock().unwrap().push((height, block.block_hash));
                }
            }));
            reader.set_file_cb(Box::new(move |file, _, _| {
                files_done.lock().unwrap().push(file);
            }));
            reader.read(&dir).unwrap();
        })
    };

    // 10 blocks inserted, depth-10 buffer keeps 9: one delivered
    wait_for("first delivery", || delivered.lock().unwrap().len() == 1);
    assert!(files_done.lock().unwrap().is_empty(), "last file must not be FileDone'd");

    // The node keeps writing: 10 more records over the zero region
    let mut f0 = cat(0..20);
    f0.extend_from_slice(&[0u8; 4096]);
    write_atomic(&dir.join("blk00000.dat"), &f0);

    wait_for("growth catch-up", || delivered.lock().unwrap().len() == 11);

    // Rollover: a new file appears, the previous one is finalized
    write_atomic(&dir.join("blk00001.dat"), &cat(20..30));
    write_atomic(&dir.join("blk00000.dat"), &cat(0..20));

    wait_for("rollover catch-up", || delivered.lock().unwrap().len() == 21);

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    handle.join().unwrap();

    let delivered = delivered.lock().unwrap();
    for (i, (height, hash)) in delivered.iter().enumerate() {
        assert_eq!(*height, i as u32);
        assert_eq!(*hash, blocks[i].block_hash(), "block {} out of order", i);
    }

    // FileDone fired for the finalized file only, once
    let files_done = files_done.lock().unwrap();
    assert_eq!(files_done.len(), 1);
    assert!(files_done[0].ends_with("blk00000.dat"));

    // The last published branch is exactly the undelivered suffix
    // (blocks 21..30), in chain order
    let branch = branch.lock().unwrap();
    assert_eq!(branch.len(), 9);
    for (i, hash) in branch.iter().enumerate() {
        assert_eq!(*hash, blocks[21 + i].block_hash(), "branch out of order");
    }
}

#[test]
fn resume_with_root_and_start_at() {
    let dir = temp_dir("resume");
    let blocks = make_chain(30);
    let recs: Vec<Vec<u8>> = blocks.iter().map(record).collect();

    let f0: Vec<u8> = recs[0..15].iter().flatten().copied().collect();
    let f1: Vec<u8> = recs[15..30].iter().flatten().copied().collect();
    write_atomic(&dir.join("blk00000.dat"), &f0);
    write_atomic(&dir.join("blk00001.dat"), &f1);

    // Resume at block 20: root = hash of block 19, offset of record 20
    // inside file 1
    let offset: u64 = recs[15..20].iter().map(|r| r.len() as u64).sum();

    let options = BlockReaderOptions {
        root: Some(blocks[19].block_hash()),
        start_at: Some((1, offset)),
        ..Default::default()
    };

    let delivered: Arc<Mutex<Vec<(u32, BlockHash)>>> = Arc::new(Mutex::new(Vec::new()));

    let mut reader = BlockReader::new(options);
    reader.set_block_cb(Box::new({
        let delivered = Arc::clone(&delivered);
        move |block, height| {
            delivered.lock().unwrap().push((height, block.block_hash));
        }
    }));
    reader.read(&dir).unwrap();

    // Blocks 20..30 inserted (10), buffer keeps 9: block 20 delivered at
    // local height 0
    let delivered = delivered.lock().unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].0, 0);
    assert_eq!(delivered[0].1, blocks[20].block_hash());
}

#[test]
fn resume_skips_known_blocks() {
    let dir = temp_dir("resume-skip");
    let blocks = make_chain(30);
    let data: Vec<u8> = blocks.iter().flat_map(record).collect();
    write_atomic(&dir.join("blk00000.dat"), &data);

    // Resume at block 20, re-reading the whole file: blocks 0..20 are
    // below the root and can never attach
    let options = BlockReaderOptions {
        root: Some(blocks[19].block_hash()),
        start_at: Some((0, 0)),
        max_orphans: None,
        ..Default::default()
    };

    let known: Vec<BlockHash> = blocks[..20].iter().map(|b| b.block_hash()).collect();
    let delivered: Arc<Mutex<Vec<BlockHash>>> = Arc::new(Mutex::new(Vec::new()));

    let mut reader = BlockReader::new(options);
    reader.set_skip_cb(Box::new(move |hash| known.contains(hash)));
    reader.set_block_cb(Box::new({
        let delivered = Arc::clone(&delivered);
        move |block, _height| delivered.lock().unwrap().push(block.block_hash)
    }));
    reader.read(&dir).unwrap();

    assert_eq!(reader.orphans(), 0);
    assert_eq!(*delivered.lock().unwrap(), vec![blocks[20].block_hash()]);
}

#[test]
fn stale_fork_reporting() {
    let dir = temp_dir("stale");
    let blocks = make_chain(25);

    // A losing fork of two blocks off block 4
    let fork1 = make_block(blocks[4].block_hash(), 1000);
    let fork2 = make_block(fork1.block_hash(), 1001);

    let mut data: Vec<u8> = Vec::new();
    for (i, block) in blocks.iter().enumerate() {
        data.extend(record(block));

        // The fork shows up shortly after the branch point
        if i == 6 {
            data.extend(record(&fork1));
            data.extend(record(&fork2));
        }
    }
    write_atomic(&dir.join("blk00000.dat"), &data);

    let sealed = std::cell::RefCell::new(Vec::<(u32, BlockHash)>::new());
    let stales = std::cell::RefCell::new(Vec::<BlockHash>::new());

    let mut reader = BlockReader::new(BlockReaderOptions::default());
    reader.set_block_cb(Box::new(|block, height| {
        sealed.borrow_mut().push((height, block.block_hash));
    }));
    reader.set_stale_cb(Box::new(|block| {
        stales.borrow_mut().push(block.block_hash);
    }));
    reader.read(&dir).unwrap();
    drop(reader);

    let sealed = sealed.into_inner();
    let stales = stales.into_inner();

    // The main branch alone gets sealed, in order
    assert_eq!(sealed[4], (4, blocks[4].block_hash()));
    assert!(sealed.iter().all(|(h, hash)| blocks[*h as usize].block_hash() == *hash));

    // Both fork blocks reported once the branch point settled, and the
    // report came after the winning block at the same height
    assert_eq!(stales.len(), 2);
    assert!(stales.contains(&fork1.block_hash()));
    assert!(stales.contains(&fork2.block_hash()));
    assert!(sealed.iter().any(|(h, _)| *h >= 5), "fork point settled");
}
