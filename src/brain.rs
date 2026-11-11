//! The brain: a llama2.c-format model and tokenizer, read from the boot drive before the firmware
//! leaves, then run entirely by logOS. Without a model file it falls back to a small untrained
//! network so logOS still boots.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::transformer::{ModelConfig, RunState, transformer_step};

pub struct Brain {
    pub cfg: ModelConfig,
    data: Vec<f32>,
    off: [usize; 12],
    vocab: Vec<Vec<u8>>,
    scores: Vec<f32>,
    sorted: Vec<u32>, // vocab ids sorted by their bytes, for encoding
    pub trained: bool,
}

const BOS: usize = 1;
const EOS: usize = 2;

impl Brain {
    /// Lay out the tensors the way llama2.c writes them.
    fn layout(cfg: &ModelConfig, shared: bool, start: usize) -> ([usize; 12], usize) {
        let (d, h, l, v) = (cfg.dim, cfg.hidden_dim, cfg.n_layers, cfg.vocab_size);
        let head = d / cfg.n_heads;
        let kv = cfg.n_kv_heads * head;
        let sizes = [v * d, l * d, l * d * d, l * d * kv, l * d * kv, l * d * d, l * d, l * d * h, l * h * d, l * d * h, d];
        let mut off = [0usize; 12];
        let mut at = start;
        for (i, s) in sizes.iter().enumerate() {
            off[i] = at;
            at += s;
        }
        at += cfg.seq_len * head; // the old freq_cis_real and freq_cis_imag tables, unused
        off[11] = if shared { off[0] } else { at };
        let end = if shared { at } else { at + v * d };
        (off, end)
    }

    fn t(&self, i: usize, len: usize) -> &[f32] {
        &self.data[self.off[i]..self.off[i] + len]
    }

    pub fn step(&self, token: usize, pos: usize, st: &mut RunState) {
        let c = &self.cfg;
        let (d, h, l, v) = (c.dim, c.hidden_dim, c.n_layers, c.vocab_size);
        let kv = c.n_kv_heads * (d / c.n_heads);
        transformer_step(
            token % v, pos, c, st,
            self.t(0, v * d), self.t(1, l * d), self.t(2, l * d * d), self.t(3, l * d * kv), self.t(4, l * d * kv),
            self.t(5, l * d * d), self.t(6, l * d), self.t(7, l * d * h), self.t(8, l * h * d), self.t(9, l * d * h),
            self.t(10, d), self.t(11, v * d),
        );
    }

    /// The untrained fallback: seeded weights, no tokenizer.
    pub fn micro() -> Brain {
        let cfg = ModelConfig::micro();
        let (off, end) = Self::layout(&cfg, true, 0);
        let mut data = vec![0.0f32; end];
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let k = 1.0 / libm::sqrtf(cfg.dim as f32);
        for (i, x) in data.iter_mut().enumerate() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let r = (seed >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0;
            *x = if i < off[1] { r } else { r * k };
        }
        // RMSNorm weights are 1.
        let (d, l) = (cfg.dim, cfg.n_layers);
        for (o, n) in [(off[1], l * d), (off[6], l * d), (off[10], d)] {
            data[o..o + n].fill(1.0);
        }
        Brain { cfg, data, off, vocab: Vec::new(), scores: Vec::new(), sorted: Vec::new(), trained: false }
    }

    /// Build a brain from the bytes of a model file (as f32 words) and a tokenizer file.
    pub fn from_files(model: Vec<f32>, tok: &[u8]) -> Option<Brain> {
        let hdr: Vec<i32> = model.get(..7)?.iter().map(|f| f.to_bits() as i32).collect();
        let shared = hdr[5] > 0;
        let cfg = ModelConfig {
            dim: hdr[0] as usize,
            hidden_dim: hdr[1] as usize,
            n_layers: hdr[2] as usize,
            n_heads: hdr[3] as usize,
            n_kv_heads: hdr[4] as usize,
            vocab_size: hdr[5].unsigned_abs() as usize,
            seq_len: hdr[6] as usize,
        };
        let (off, end) = Self::layout(&cfg, shared, 7);
        if end > model.len() || cfg.dim == 0 || cfg.n_heads == 0 {
            return None;
        }
        let (vocab, scores) = parse_tokenizer(tok, cfg.vocab_size)?;
        let mut sorted: Vec<u32> = (0..vocab.len() as u32).collect();
        sorted.sort_by(|a, b| vocab[*a as usize].cmp(&vocab[*b as usize]));
        Some(Brain { cfg, data: model, off, vocab, scores, sorted, trained: true })
    }

    fn lookup(&self, piece: &[u8]) -> Option<usize> {
        self.sorted.binary_search_by(|id| self.vocab[*id as usize].as_slice().cmp(piece)).ok().map(|i| self.sorted[i] as usize)
    }

    /// llama2.c's byte-pair encoder: a leading space, one token per character, then greedy merges.
    pub fn encode(&self, text: &str) -> Vec<usize> {
        let mut out = vec![BOS];
        let mut toks: Vec<usize> = Vec::new();
        if !text.is_empty() {
            if let Some(sp) = self.lookup(b" ") {
                toks.push(sp);
            }
        }
        for ch in text.chars() {
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf).as_bytes();
            match self.lookup(s) {
                Some(id) => toks.push(id),
                None => toks.extend(s.iter().map(|b| *b as usize + 3)), // byte fallback tokens
            }
        }
        loop {
            let mut best: Option<(f32, usize, usize)> = None;
            for i in 0..toks.len().saturating_sub(1) {
                let mut pair = self.vocab[toks[i]].clone();
                pair.extend_from_slice(&self.vocab[toks[i + 1]]);
                if let Some(id) = self.lookup(&pair) {
                    if best.is_none_or(|(s, _, _)| self.scores[id] > s) {
                        best = Some((self.scores[id], id, i));
                    }
                }
            }
            let Some((_, id, i)) = best else { break };
            toks[i] = id;
            toks.remove(i + 1);
        }
        out.extend(toks);
        out
    }

    /// Text for one generated token.
    pub fn decode(&self, prev: usize, token: usize) -> String {
        let Some(bytes) = self.vocab.get(token) else { return String::new() };
        let mut b: &[u8] = bytes;
        if prev == BOS && b.first() == Some(&b' ') {
            b = &b[1..];
        }
        // "<0x0A>"-style raw byte tokens
        if b.len() == 6 && b.starts_with(b"<0x") && b[5] == b'>' {
            if let Ok(v) = u8::from_str_radix(core::str::from_utf8(&b[3..5]).unwrap_or(""), 16) {
                return String::from(if v == b'\n' { " " } else { "" });
            }
        }
        String::from_utf8_lossy(b).into_owned()
    }

    pub fn is_end(token: usize) -> bool {
        token == BOS || token == EOS
    }
}

fn parse_tokenizer(b: &[u8], n: usize) -> Option<(Vec<Vec<u8>>, Vec<f32>)> {
    let rd = |at: usize| -> Option<[u8; 4]> { b.get(at..at + 4)?.try_into().ok() };
    let mut at = 4; // max_token_length
    let mut vocab = Vec::with_capacity(n);
    let mut scores = Vec::with_capacity(n);
    for _ in 0..n {
        scores.push(f32::from_le_bytes(rd(at)?));
        let len = i32::from_le_bytes(rd(at + 4)?) as usize;
        at += 8;
        vocab.push(b.get(at..at + len)?.to_vec());
        at += len;
    }
    Some((vocab, scores))
}

/// Pick the next token: temperature, then nucleus (top-p) sampling.
pub fn sample(logits: &mut [f32], temp: f32, top_p: f32, rng: &mut u64) -> usize {
    let max = logits.iter().cloned().fold(f32::MIN, f32::max);
    let mut sum = 0.0;
    for l in logits.iter_mut() {
        *l = libm::expf((*l - max) / temp);
        sum += *l;
    }
    let mut idx: Vec<(f32, u32)> = logits.iter().enumerate().map(|(i, p)| (*p / sum, i as u32)).filter(|(p, _)| *p > 1e-6).collect();
    idx.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(core::cmp::Ordering::Equal));
    let mut cum = 0.0;
    let mut cut = idx.len();
    for (i, (p, _)) in idx.iter().enumerate() {
        cum += p;
        if cum > top_p {
            cut = i + 1;
            break;
        }
    }
    *rng ^= *rng << 13;
    *rng ^= *rng >> 7;
    *rng ^= *rng << 17;
    let r = ((*rng >> 40) as f32 / (1u64 << 24) as f32) * cum.min(1.0);
    let mut acc = 0.0;
    for (p, i) in &idx[..cut] {
        acc += p;
        if acc >= r {
            return *i as usize;
        }
    }
    idx[0].1 as usize
}

/// Read `path` from the boot drive into a buffer of 32-bit words. Firmware must still be here.
pub fn read_file(path: &uefi::CStr16) -> Option<Vec<u8>> {
    use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode};
    let mut fs = uefi::boot::get_image_file_system(uefi::boot::image_handle()).ok()?;
    let mut root = fs.open_volume().ok()?;
    let mut f = root.open(path, FileMode::Read, FileAttribute::empty()).ok()?.into_regular_file()?;
    let size = f.get_boxed_info::<FileInfo>().ok()?.file_size() as usize;
    let mut buf = vec![0u8; size];
    let mut done = 0;
    while done < size {
        let n = f.read(&mut buf[done..]).ok()?;
        if n == 0 {
            break;
        }
        done += n;
    }
    (done == size).then_some(buf)
}

/// Bytes to f32 words (little-endian), into a fresh aligned buffer.
pub fn words(bytes: Vec<u8>) -> Vec<f32> {
    let mut out = vec![0f32; bytes.len() / 4];
    for (o, c) in out.iter_mut().zip(bytes.chunks_exact(4)) {
        *o = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
    }
    out
}
