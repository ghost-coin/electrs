use bitcoin::blockdata::block::Block;
use bitcoin::network::serialize::BitcoinHash;
use bitcoin::network::serialize::SimpleDecoder;
use bitcoin::network::serialize::{deserialize, RawDecoder};
use std::fs;
use std::io::{Cursor, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::thread;

use daemon::Daemon;
use metrics::{HistogramOpts, HistogramVec, Metrics};
use util::SyncChannel;

use errors::*;

/// An efficient parser for Bitcoin blk*.dat files.
pub struct Parser {
    files: Vec<PathBuf>,
    // metrics
    duration: HistogramVec,
}

impl Parser {
    pub fn new(daemon: &Daemon, metrics: &Metrics) -> Result<Parser> {
        Ok(Parser {
            files: daemon.list_blk_files()?,
            duration: metrics.histogram_vec(
                HistogramOpts::new("parse_duration", "Block parsing duration (in seconds)"),
                &["step"],
            ),
        })
    }

    pub fn start(self) -> Receiver<Result<Vec<Block>>> {
        let chan = SyncChannel::new(1);
        let tx = chan.sender();
        let blobs = read_files(self.files.clone(), self.duration.clone());
        let duration = self.duration.clone();
        thread::spawn(move || {
            for msg in blobs.iter() {
                match msg {
                    Ok(blob) => {
                        let timer = duration.with_label_values(&["parse"]).start_timer();
                        let blocks = parse_blocks(&blob);
                        timer.observe_duration();
                        tx.send(blocks).unwrap();
                    }
                    Err(err) => {
                        tx.send(Err(err)).unwrap();
                        return;
                    }
                }
            }
        });
        chan.into_receiver()
    }
}

fn read_files(files: Vec<PathBuf>, duration: HistogramVec) -> Receiver<Result<Vec<u8>>> {
    let chan = SyncChannel::new(1);
    let tx = chan.sender();
    thread::spawn(move || {
        info!("reading {} files", files.len());
        for f in &files {
            let timer = duration.with_label_values(&["read"]).start_timer();
            let msg = fs::read(f).chain_err(|| format!("failed to read {:?}", f));
            debug!("read {:?}", f);
            timer.observe_duration();
            tx.send(msg).unwrap();
        }
    });
    chan.into_receiver()
}

fn parse_blocks(data: &[u8]) -> Result<Vec<Block>> {
    let mut cursor = Cursor::new(&data);
    let mut blocks = vec![];
    let max_pos = data.len() as u64;
    while cursor.position() < max_pos {
        let mut decoder = RawDecoder::new(cursor);
        match decoder.read_u32().chain_err(|| "no magic")? {
            0 => break,
            0xD9B4BEF9 => (),
            x => bail!("incorrect magic {:x}", x),
        };
        let block_size = decoder.read_u32().chain_err(|| "no block size")?;
        cursor = decoder.into_inner();

        let start = cursor.position() as usize;
        cursor
            .seek(SeekFrom::Current(block_size as i64))
            .chain_err(|| format!("seek {} failed", block_size))?;
        let end = cursor.position() as usize;

        let block: Block = deserialize(&data[start..end])
            .chain_err(|| format!("failed to parse block at {}..{}", start, end))?;
        trace!("block {}, {} bytes", block.bitcoin_hash(), block_size);
        blocks.push(block);
    }
    debug!("parsed {} blocks", blocks.len());
    Ok(blocks)
}