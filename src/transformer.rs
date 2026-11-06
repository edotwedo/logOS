//! logOS Neural Engine
//! Pure no_std Transformer forward pass running on bare silicon.
//! Supports LLaMA / SmolLM architecture: RMSNorm, RoPE, Multi-Head Attention, SwiGLU FFN.

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;
use libm::{cosf, expf, sinf, sqrtf};

/// Hyperparameters defining the neural architecture
#[derive(Clone, Copy, Debug)]
pub struct ModelConfig {
    pub dim: usize,        // Transformer hidden dimension (e.g. 288 or 576)
    pub hidden_dim: usize, // FFN intermediate dimension
    pub n_layers: usize,   // Number of transformer layers
    pub n_heads: usize,    // Number of query attention heads
    pub n_kv_heads: usize, // Number of key/value heads (GQA support)
    pub vocab_size: usize, // Vocabulary size
    pub seq_len: usize,    // Maximum sequence context length
}

impl ModelConfig {
    /// Configuration for a compact embedded consciousness (e.g., SmolLM-style micro-brain)
    pub const fn micro() -> Self {
        Self {
            dim: 128,
            hidden_dim: 384,
            n_layers: 4,
            n_heads: 4,
            n_kv_heads: 4,
            vocab_size: 512,
            seq_len: 256,
        }
    }
}

/// Dynamic run state / activation buffers allocated in physical memory
pub struct RunState {
    pub x: Vec<f32>,      // Activation at current timestep (dim)
    pub xb: Vec<f32>,     // Inside residual buffer (dim)
    pub xb2: Vec<f32>,    // Additional buffer for FFN / SwiGLU (dim)
    pub hb: Vec<f32>,     // FFN hidden dimension buffer (hidden_dim)
    pub hb2: Vec<f32>,    // FFN hidden dimension buffer 2 (hidden_dim)
    pub q: Vec<f32>,      // Query vector (dim)
    pub k: Vec<f32>,      // Key vector (dim)
    pub v: Vec<f32>,      // Value vector (dim)
    pub att: Vec<f32>,    // Attention scores buffer (n_heads * seq_len)
    pub logits: Vec<f32>, // Output logits over vocabulary (vocab_size)
    // Key/Value Cache: [n_layers, seq_len, kv_dim]
    pub key_cache: Vec<f32>,
    pub value_cache: Vec<f32>,
}

impl RunState {
    pub fn new(cfg: &ModelConfig) -> Self {
        let kv_dim = (cfg.dim * cfg.n_kv_heads) / cfg.n_heads;
        Self {
            x: vec![0.0; cfg.dim],
            xb: vec![0.0; cfg.dim],
            xb2: vec![0.0; cfg.dim],
            hb: vec![0.0; cfg.hidden_dim],
            hb2: vec![0.0; cfg.hidden_dim],
            q: vec![0.0; cfg.dim],
            k: vec![0.0; kv_dim],
            v: vec![0.0; kv_dim],
            att: vec![0.0; cfg.n_heads * cfg.seq_len],
            logits: vec![0.0; cfg.vocab_size],
            key_cache: vec![0.0; cfg.n_layers * cfg.seq_len * kv_dim],
            value_cache: vec![0.0; cfg.n_layers * cfg.seq_len * kv_dim],
        }
    }
}

/// Root-Mean-Square Normalization in-place or out-of-place
pub fn rmsnorm(out: &mut [f32], x: &[f32], weight: &[f32], eps: f32) {
    let size = x.len();
    let mut sum_sq = 0.0f32;
    for &val in x {
        sum_sq += val * val;
    }
    let scale = 1.0 / sqrtf(sum_sq / (size as f32) + eps);
    for i in 0..size {
        out[i] = weight[i] * (x[i] * scale);
    }
}

pub fn rmsnorm_inplace(x: &mut [f32], weight: &[f32], eps: f32) {
    let size = x.len();
    let mut sum_sq = 0.0f32;
    for &val in x.iter() {
        sum_sq += val * val;
    }
    let scale = 1.0 / sqrtf(sum_sq / (size as f32) + eps);
    for i in 0..size {
        x[i] = weight[i] * (x[i] * scale);
    }
}

/// Softmax over a slice of activations in-place
pub fn softmax(x: &mut [f32]) {
    let mut max_val = x[0];
    for &val in x.iter().skip(1) {
        if val > max_val {
            max_val = val;
        }
    }
    let mut sum = 0.0f32;
    for val in x.iter_mut() {
        *val = expf(*val - max_val);
        sum += *val;
    }
    let inv_sum = 1.0 / sum;
    for val in x.iter_mut() {
        *val *= inv_sum;
    }
}

/// Matrix-vector multiplication: out = W @ x
pub fn matmul(out: &mut [f32], x: &[f32], w: &[f32], rows: usize, cols: usize) {
    for r in 0..rows {
        let mut sum = 0.0f32;
        let w_row = &w[r * cols..(r + 1) * cols];
        for c in 0..cols {
            sum += w_row[c] * x[c];
        }
        out[r] = sum;
    }
}

/// SwiGLU in-place: hb = silu(hb) * hb2
pub fn swiglu_inplace(hb: &mut [f32], hb2: &[f32]) {
    for i in 0..hb.len() {
        let x = hb[i];
        let silu = x / (1.0 + expf(-x));
        hb[i] = silu * hb2[i];
    }
}

/// Forward pass of one token through the neural engine
pub fn transformer_step(
    token: usize,
    pos: usize,
    cfg: &ModelConfig,
    state: &mut RunState,
    token_embedding_table: &[f32],
    rms_att_weight: &[f32], // [n_layers, dim]
    wq: &[f32],             // [n_layers, dim, dim]
    wk: &[f32],             // [n_layers, kv_dim, dim]
    wv: &[f32],             // [n_layers, kv_dim, dim]
    wo: &[f32],             // [n_layers, dim, dim]
    rms_ffn_weight: &[f32], // [n_layers, dim]
    w1: &[f32],             // [n_layers, hidden_dim, dim] (gate)
    w2: &[f32],             // [n_layers, dim, hidden_dim] (down)
    w3: &[f32],             // [n_layers, hidden_dim, dim] (up)
    rms_final_weight: &[f32],
    w_cls: &[f32],          // [vocab_size, dim]
) {
    let dim = cfg.dim;
    let kv_dim = (cfg.dim * cfg.n_kv_heads) / cfg.n_heads;
    let head_size = dim / cfg.n_heads;

    // 1. Copy token embedding into state.x
    let embed_offset = token * dim;
    state.x.copy_from_slice(&token_embedding_table[embed_offset..embed_offset + dim]);

    // 2. Loop through transformer layers
    for l in 0..cfg.n_layers {
        // Pre-attention RMSNorm: state.xb = norm(state.x)
        let norm_att_offset = l * dim;
        rmsnorm(&mut state.xb, &state.x, &rms_att_weight[norm_att_offset..norm_att_offset + dim], 1e-5);

        // Compute Q, K, V
        let w_offset_q = l * dim * dim;
        let w_offset_kv = l * kv_dim * dim;
        matmul(&mut state.q, &state.xb, &wq[w_offset_q..w_offset_q + dim * dim], dim, dim);
        matmul(&mut state.k, &state.xb, &wk[w_offset_kv..w_offset_kv + kv_dim * dim], kv_dim, dim);
        matmul(&mut state.v, &state.xb, &wv[w_offset_kv..w_offset_kv + kv_dim * dim], kv_dim, dim);

        // Apply Rotary Position Embeddings (RoPE)
        for h in 0..cfg.n_heads {
            for i in (0..head_size).step_by(2) {
                let freq = 1.0 / libm::powf(10000.0, (i as f32) / (head_size as f32));
                let val = (pos as f32) * freq;
                let fcr = cosf(val);
                let fci = sinf(val);

                let q_idx = h * head_size + i;
                let q0 = state.q[q_idx];
                let q1 = state.q[q_idx + 1];
                state.q[q_idx] = q0 * fcr - q1 * fci;
                state.q[q_idx + 1] = q0 * fci + q1 * fcr;

                if h < cfg.n_kv_heads {
                    let k_idx = h * head_size + i;
                    let k0 = state.k[k_idx];
                    let k1 = state.k[k_idx + 1];
                    state.k[k_idx] = k0 * fcr - k1 * fci;
                    state.k[k_idx + 1] = k0 * fci + k1 * fcr;
                }
            }
        }

        // Save K and V to KV-cache at current position
        let kv_cache_offset = l * cfg.seq_len * kv_dim + pos * kv_dim;
        state.key_cache[kv_cache_offset..kv_cache_offset + kv_dim].copy_from_slice(&state.k);
        state.value_cache[kv_cache_offset..kv_cache_offset + kv_dim].copy_from_slice(&state.v);

        // Multi-head Attention
        let scale = 1.0 / sqrtf(head_size as f32);
        for h in 0..cfg.n_heads {
            let q_head = &state.q[h * head_size..(h + 1) * head_size];
            let att_head_offset = h * cfg.seq_len;

            // Score Q against all cached K up to current position
            for t in 0..=pos {
                let k_cache_entry = l * cfg.seq_len * kv_dim + t * kv_dim + (h * kv_dim / cfg.n_heads) * head_size;
                let k_vector = &state.key_cache[k_cache_entry..k_cache_entry + head_size];

                let mut score = 0.0f32;
                for d in 0..head_size {
                    score += q_head[d] * k_vector[d];
                }
                state.att[att_head_offset + t] = score * scale;
            }

            // Softmax attention scores
            softmax(&mut state.att[att_head_offset..att_head_offset + pos + 1]);

            // Weighted sum of V vectors into state.xb
            let out_head = &mut state.xb[h * head_size..(h + 1) * head_size];
            for d in 0..head_size {
                out_head[d] = 0.0;
            }

            for t in 0..=pos {
                let a = state.att[att_head_offset + t];
                let v_cache_entry = l * cfg.seq_len * kv_dim + t * kv_dim + (h * kv_dim / cfg.n_heads) * head_size;
                let v_vector = &state.value_cache[v_cache_entry..v_cache_entry + head_size];
                for d in 0..head_size {
                    out_head[d] += a * v_vector[d];
                }
            }
        }

        // Output projection: state.xb2 = wo @ state.xb
        matmul(&mut state.xb2, &state.xb, &wo[w_offset_q..w_offset_q + dim * dim], dim, dim);

        // Residual connection: state.x += state.xb2
        for i in 0..dim {
            state.x[i] += state.xb2[i];
        }

        // Feed-Forward Network (SwiGLU)
        // 1. RMSNorm
        let norm_ffn_offset = l * dim;
        rmsnorm(&mut state.xb, &state.x, &rms_ffn_weight[norm_ffn_offset..norm_ffn_offset + dim], 1e-5);

        // 2. Gate (w1) and Up (w3) projections
        let w_offset_ffn = l * cfg.hidden_dim * dim;
        matmul(&mut state.hb, &state.xb, &w1[w_offset_ffn..w_offset_ffn + cfg.hidden_dim * dim], cfg.hidden_dim, dim);
        matmul(&mut state.hb2, &state.xb, &w3[w_offset_ffn..w_offset_ffn + cfg.hidden_dim * dim], cfg.hidden_dim, dim);

        // 3. SwiGLU in-place
        swiglu_inplace(&mut state.hb, &state.hb2);

        // 4. Down projection (w2)
        matmul(&mut state.xb, &state.hb, &w2[w_offset_ffn..w_offset_ffn + dim * cfg.hidden_dim], dim, cfg.hidden_dim);

        // 5. Residual connection
        for i in 0..dim {
            state.x[i] += state.xb[i];
        }
    }

    // Final RMSNorm in-place
    rmsnorm_inplace(&mut state.x, rms_final_weight, 1e-5);

    // Compute logits over vocabulary
    matmul(&mut state.logits, &state.x, w_cls, cfg.vocab_size, dim);
}
