//! Where a binary file changed, as a strip of cells from its start to its end.
//!
//! Binary files can't be diffed line by line, and keeping their old bytes
//! would cost as much memory as the files themselves. Instead a file is cut
//! into [`CELLS`] blocks and only a hash of each block is kept ([`Blocks`]).
//! When the file changes, its new bytes are hashed along the *old* block
//! borders, and the blocks whose hash differs are the ones that changed.
//!
//! Everything is computed in one streaming pass ([`scan`]), so memory stays
//! the same whatever the file size, and the hash eats 32 bytes per step so
//! that pass runs at about memory speed.

use std::io::{self, Read};

/// How many cells the strip has, and how many blocks a file is cut into.
pub const CELLS: usize = 48;
/// Bytes per cell looked at for its texture. Texture is decoration, so a
/// sample is plenty and keeps huge files cheap.
const TEXTURE_SAMPLE: u64 = 4096;

/// Hashes of a file's bytes, block by block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocks {
    len: u64,
    size: u64,
    hashes: Vec<u64>,
}

impl Blocks {
    /// Memory this takes, for the cache budget.
    pub fn memory(&self) -> usize {
        self.hashes.len() * size_of::<u64>()
    }
}

fn block_size(len: u64) -> u64 {
    len.div_ceil(CELLS as u64).max(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Same,
    Changed,
    /// Past the old end: the file grew here.
    Added,
    /// Past the new end: the file was cut off here.
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub state: State,
    /// How random the bytes here are, 0 (all alike) to 3 (compressed or
    /// encrypted): the texture of unchanged cells.
    pub texture: u8,
}

/// The strip: up to [`CELLS`] cells spread evenly over the larger of the
/// old and new size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteMap {
    pub cells: Vec<Cell>,
    /// Bytes per cell, to label where a change starts.
    pub cell_size: u64,
    pub len: u64,
    /// Where the shorter of the two versions ends: exactly where the file
    /// grew or was cut off.
    pub shared: u64,
}

/// Reads the first `len` bytes of `file` once. Returns their block hashes,
/// and, given what was known `before`, the map of what changed.
pub fn scan(
    file: impl Read,
    len: u64,
    before: Option<&Blocks>,
) -> io::Result<(Blocks, Option<ByteMap>)> {
    let mut own = Blocker::new(block_size(len), len);
    // The new bytes cut along the old borders, as far as both files go.
    let mut old_borders = before.map(|b| Blocker::new(b.size, b.len.min(len)));
    let full = before.map_or(len, |b| b.len.max(len));
    let cell_size = block_size(full);
    let mut textures = Textures::new(cell_size);

    let mut reader = file.take(len);
    let mut buf = vec![0; 256 * 1024];
    let mut at = 0;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        own.feed(at, &buf[..n]);
        if let Some(b) = &mut old_borders {
            b.feed(at, &buf[..n]);
        }
        textures.feed(at, &buf[..n]);
        at += n as u64;
    }
    // The file may have shrunk while we read; what we got is what it is.
    let len = at;
    let blocks = own.finish(len);
    let map = before.zip(old_borders).map(|(before, b)| {
        let shared = before.len.min(len);
        let changed: Vec<bool> = b
            .finish(shared)
            .hashes
            .iter()
            .zip(&before.hashes)
            .map(|(a, b)| a != b)
            .collect();
        let full = before.len.max(len);
        let cells = (0..full.div_ceil(cell_size))
            .map(|i| {
                let (start, end) = (i * cell_size, ((i + 1) * cell_size).min(full));
                let state = if start >= shared {
                    if len > before.len {
                        State::Added
                    } else {
                        State::Removed
                    }
                } else {
                    let (first, last) = (start / before.size, (end.min(shared) - 1) / before.size);
                    let hit = changed[first as usize..=last as usize].iter().any(|&c| c);
                    if hit { State::Changed } else { State::Same }
                };
                Cell {
                    state,
                    texture: textures.level(i as usize),
                }
            })
            .collect();
        ByteMap {
            cells,
            cell_size,
            len: full,
            shared,
        }
    });
    Ok((blocks, map))
}

/// Hashes a stream in blocks of `size`, up to `limit` bytes.
struct Blocker {
    size: u64,
    limit: u64,
    hash: FastHash,
    hashes: Vec<u64>,
}

impl Blocker {
    fn new(size: u64, limit: u64) -> Self {
        Self {
            size,
            limit,
            hash: FastHash::default(),
            hashes: Vec::new(),
        }
    }

    /// `bytes` start at offset `at` of the file.
    fn feed(&mut self, mut at: u64, mut bytes: &[u8]) {
        let room = self.limit.saturating_sub(at) as usize;
        bytes = &bytes[..bytes.len().min(room)];
        while !bytes.is_empty() {
            let to_border = (self.size - at % self.size) as usize;
            let (now, rest) = bytes.split_at(to_border.min(bytes.len()));
            self.hash.write(now);
            at += now.len() as u64;
            if at.is_multiple_of(self.size) {
                self.hashes.push(std::mem::take(&mut self.hash).finish());
            }
            bytes = rest;
        }
    }

    fn finish(mut self, len: u64) -> Blocks {
        let len = len.min(self.limit);
        if !len.is_multiple_of(self.size) {
            self.hashes.push(self.hash.finish());
        }
        Blocks {
            len,
            size: self.size,
            hashes: self.hashes,
        }
    }
}

/// Byte counts from the start of each cell, for its texture.
struct Textures {
    cell_size: u64,
    counts: Vec<[u32; 256]>,
}

impl Textures {
    fn new(cell_size: u64) -> Self {
        Self {
            cell_size,
            counts: Vec::new(),
        }
    }

    fn feed(&mut self, at: u64, bytes: &[u8]) {
        let mut pos = at;
        while pos < at + bytes.len() as u64 {
            let cell = pos / self.cell_size;
            let sample_end = cell * self.cell_size + TEXTURE_SAMPLE.min(self.cell_size);
            if pos >= sample_end {
                pos = (cell + 1) * self.cell_size; // skip to the next cell
                continue;
            }
            let end = sample_end.min(at + bytes.len() as u64);
            if self.counts.len() <= cell as usize {
                self.counts.resize(cell as usize + 1, [0; 256]);
            }
            for &b in &bytes[(pos - at) as usize..(end - at) as usize] {
                self.counts[cell as usize][b as usize] += 1;
            }
            pos = end;
        }
    }

    /// Shannon entropy of the cell's sample, as 0..=3.
    fn level(&self, cell: usize) -> u8 {
        let Some(counts) = self.counts.get(cell) else {
            return 0;
        };
        let n: u32 = counts.iter().sum();
        let bits: f64 = counts
            .iter()
            .filter(|&&c| c > 0)
            .map(|&c| {
                let p = c as f64 / n as f64;
                -p * p.log2()
            })
            .sum();
        (bits / 8.0 * 3.0).round() as u8
    }
}

/// A fast non-cryptographic hash, 32 bytes per step in four lanes. It only
/// has to tell accidental edits apart, not resist anyone crafting
/// collisions, and the std SipHash is several times slower on big files.
#[derive(Default)]
struct FastHash {
    lanes: [u64; 4],
    tail: [u8; 32],
    tail_len: usize,
    len: u64,
}

const PRIME: u64 = 0x9E37_79B1_85EB_CA87;

fn mix(lane: u64, word: u64) -> u64 {
    (lane ^ word).wrapping_mul(PRIME).rotate_left(31)
}

impl FastHash {
    fn write(&mut self, mut bytes: &[u8]) {
        self.len += bytes.len() as u64;
        if self.tail_len > 0 {
            let take = (32 - self.tail_len).min(bytes.len());
            self.tail[self.tail_len..self.tail_len + take].copy_from_slice(&bytes[..take]);
            self.tail_len += take;
            bytes = &bytes[take..];
            if self.tail_len < 32 {
                return;
            }
            let tail = self.tail;
            self.step(&tail);
            self.tail_len = 0;
        }
        let (chunks, rest) = bytes.as_chunks::<32>();
        for chunk in chunks {
            self.step(chunk);
        }
        self.tail[..rest.len()].copy_from_slice(rest);
        self.tail_len = rest.len();
    }

    fn step(&mut self, chunk: &[u8; 32]) {
        let (words, _) = chunk.as_chunks::<8>();
        for (lane, word) in self.lanes.iter_mut().zip(words) {
            *lane = mix(*lane, u64::from_le_bytes(*word));
        }
    }

    fn finish(mut self) -> u64 {
        let mut last = [0u8; 32];
        last[..self.tail_len].copy_from_slice(&self.tail[..self.tail_len]);
        self.step(&last);
        let mut h = self.len.wrapping_mul(PRIME);
        for (i, lane) in self.lanes.iter().enumerate() {
            h = mix(h, lane.rotate_left(i as u32 * 16));
        }
        h ^ (h >> 29)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(map: &ByteMap) -> String {
        map.cells
            .iter()
            .map(|c| match c.state {
                State::Same => '.',
                State::Changed => '#',
                State::Added => '+',
                State::Removed => '-',
            })
            .collect()
    }

    fn file(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn blocks(bytes: &[u8]) -> Blocks {
        scan(bytes, bytes.len() as u64, None).unwrap().0
    }

    fn compare(before: &[u8], after: &[u8]) -> ByteMap {
        let (_, map) = scan(after, after.len() as u64, Some(&blocks(before))).unwrap();
        map.unwrap()
    }

    #[test]
    fn finds_where_bytes_changed() {
        let before = file(4800);
        let mut after = before.clone();
        after[150] ^= 1; // block 1
        after[4000] ^= 1; // block 40
        let map = compare(&before, &after);
        let mut want = ".".repeat(CELLS);
        want.replace_range(1..2, "#");
        want.replace_range(40..41, "#");
        assert_eq!(states(&map), want);
        assert_eq!((map.cell_size, map.len), (100, 4800));
        assert_eq!(states(&compare(&before, &before)), ".".repeat(CELLS));
    }

    #[test]
    fn growth_and_truncation_show_at_the_end() {
        let before = file(4800);
        let mut grown = before.clone();
        grown.extend(file(4800));
        assert_eq!(
            states(&compare(&before, &grown)),
            ".".repeat(24) + &"+".repeat(24)
        );
        let cut = &before[..2400];
        assert_eq!(
            states(&compare(&before, cut)),
            ".".repeat(24) + &"-".repeat(24)
        );
    }

    #[test]
    fn tiny_and_empty_files_dont_panic() {
        assert_eq!(states(&compare(b"ab", b"ax")), ".#");
        assert!(compare(b"", b"").cells.is_empty());
        assert_eq!(states(&compare(b"", b"abc")), "+++");
        assert_eq!(states(&compare(b"abc", b"")), "---");
    }

    #[test]
    fn hashing_doesnt_depend_on_how_reads_are_split() {
        // A reader that hands out 7 bytes at a time, crossing every border.
        struct Trickle<'a>(&'a [u8]);
        impl Read for Trickle<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let n = buf.len().min(7).min(self.0.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let data = file(100_003);
        let whole = blocks(&data);
        let trickled = scan(Trickle(&data), data.len() as u64, None).unwrap().0;
        assert_eq!(whole, trickled);
        // And every single flipped byte is noticed.
        for at in [0, 31, 32, 2083, 50_000, 100_002] {
            let mut edited = data.clone();
            edited[at] ^= 0x80;
            assert_ne!(blocks(&edited), whole, "byte {at}");
        }
    }

    #[test]
    fn texture_follows_how_random_the_bytes_are() {
        let zeros = vec![0u8; 48_000];
        let noise: Vec<u8> = (0..48_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        assert!(compare(&zeros, &zeros).cells.iter().all(|c| c.texture == 0));
        assert!(compare(&zeros, &noise).cells.iter().all(|c| c.texture == 3));
    }
}
