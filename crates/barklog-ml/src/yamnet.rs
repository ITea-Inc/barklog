//! YAMNet (MobileNet v1, AudioSet 521 класс) на чистом Rust.
//!
//! Архитектура повторяет `yamnet.py`: свёртка 3×3 + 13 depthwise-separable блоков,
//! BatchNorm (без scale, eps = 1e-4) свёрнут в веса при загрузке, padding = 'same'
//! по правилам TensorFlow. Раскладка тензоров — NHWC, pointwise-свёртки через sgemm.

use std::path::Path;

use rayon::prelude::*;
use safetensors::{Dtype, SafeTensors};

use crate::MlError;
use crate::features::{MEL_BANDS, PATCH_FRAMES, PATCH_LEN, Patch};

pub const NUM_CLASSES: usize = 521;
pub const EMBEDDING_SIZE: usize = 1024;

const BN_EPS: f32 = 1e-4;
/// Сколько патчей обрабатывает один поток за раз.
const BATCH: usize = 8;

/// (stride, выходные каналы) для layer2..layer14; layer1 — обычная свёртка 3×3/2 → 32.
const SEPARABLE: [(usize, usize); 13] = [
    (1, 64),
    (2, 128),
    (1, 128),
    (2, 256),
    (1, 256),
    (2, 512),
    (1, 512),
    (1, 512),
    (1, 512),
    (1, 512),
    (1, 512),
    (2, 1024),
    (1, 1024),
];

/// Результат для одного патча.
#[derive(Clone)]
pub struct Prediction {
    /// Вероятности 521 класса AudioSet (sigmoid).
    pub scores: Box<[f32]>,
    /// Эмбеддинг 1024 — вход для персонального классификатора.
    pub embedding: Box<[f32]>,
}

struct Conv {
    /// `[3][3][cout]` (cin = 1).
    w: Vec<f32>,
    b: Vec<f32>,
    cout: usize,
}

struct Separable {
    stride: usize,
    cin: usize,
    cout: usize,
    /// `[3][3][cin]`.
    dw_w: Vec<f32>,
    dw_b: Vec<f32>,
    /// `[cin][cout]`.
    pw_w: Vec<f32>,
    pw_b: Vec<f32>,
}

pub struct Yamnet {
    conv1: Conv,
    blocks: Vec<Separable>,
    /// `[EMBEDDING_SIZE][NUM_CLASSES]`.
    logits_w: Vec<f32>,
    logits_b: Vec<f32>,
}

impl Yamnet {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, MlError> {
        let bytes = std::fs::read(path.as_ref())?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MlError> {
        let st = SafeTensors::deserialize(bytes).map_err(|e| MlError::Model(e.to_string()))?;
        let get = |name: &str, shape: &[usize]| -> Result<Vec<f32>, MlError> {
            let t = st
                .tensor(name)
                .map_err(|_| MlError::Model(format!("нет тензора {name}")))?;
            if t.dtype() != Dtype::F32 || t.shape() != shape {
                return Err(MlError::Model(format!(
                    "{name}: ожидалось f32 {shape:?}, получено {:?} {:?}",
                    t.dtype(),
                    t.shape()
                )));
            }
            Ok(t.data()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect())
        };
        let bn = |prefix: &str, c: usize| -> Result<(Vec<f32>, Vec<f32>), MlError> {
            let beta = get(&format!("{prefix}/beta"), &[c])?;
            let mean = get(&format!("{prefix}/moving_mean"), &[c])?;
            let var = get(&format!("{prefix}/moving_variance"), &[c])?;
            let scale: Vec<f32> = var.iter().map(|v| 1.0 / (v + BN_EPS).sqrt()).collect();
            let bias = beta
                .iter()
                .zip(&mean)
                .zip(&scale)
                .map(|((b, m), s)| b - m * s)
                .collect();
            Ok((scale, bias))
        };

        let (s, b) = bn("layer1/conv/bn", 32)?;
        let mut w = get("layer1/conv/kernel", &[3, 3, 1, 32])?;
        fold(&mut w, &s);
        let conv1 = Conv { w, b, cout: 32 };

        let mut blocks = Vec::with_capacity(SEPARABLE.len());
        let mut cin = 32;
        for (i, &(stride, cout)) in SEPARABLE.iter().enumerate() {
            let l = format!("layer{}", i + 2);
            let (s, dw_b) = bn(&format!("{l}/depthwise_conv/bn"), cin)?;
            let mut dw_w = get(
                &format!("{l}/depthwise_conv/depthwise_kernel"),
                &[3, 3, cin, 1],
            )?;
            fold(&mut dw_w, &s);
            let (s, pw_b) = bn(&format!("{l}/pointwise_conv/bn"), cout)?;
            let mut pw_w = get(&format!("{l}/pointwise_conv/kernel"), &[1, 1, cin, cout])?;
            fold(&mut pw_w, &s);
            blocks.push(Separable {
                stride,
                cin,
                cout,
                dw_w,
                dw_b,
                pw_w,
                pw_b,
            });
            cin = cout;
        }

        let logits_w = get("logits/kernel", &[EMBEDDING_SIZE, NUM_CLASSES])?;
        let logits_b = get("logits/bias", &[NUM_CLASSES])?;
        Ok(Self {
            conv1,
            blocks,
            logits_w,
            logits_b,
        })
    }

    /// Прогоняет патчи через сеть (параллельно по ядрам процессора).
    pub fn predict(&self, patches: &[Patch]) -> Vec<Prediction> {
        patches
            .par_chunks(BATCH)
            .flat_map_iter(|chunk| {
                let mut input = Vec::with_capacity(chunk.len() * PATCH_LEN);
                for p in chunk {
                    input.extend_from_slice(&p.features);
                }
                self.forward(&input, chunk.len())
            })
            .collect()
    }

    /// Прогоняет «сырые» признаки `[n][96][64]`.
    pub fn forward(&self, input: &[f32], n: usize) -> Vec<Prediction> {
        assert_eq!(input.len(), n * PATCH_LEN);
        let mut x = Tensor {
            data: input.to_vec(),
            n,
            h: PATCH_FRAMES,
            w: MEL_BANDS,
            c: 1,
        };
        x = conv3x3_s2(&x, &self.conv1);
        for block in &self.blocks {
            x = depthwise(&x, block);
            x = pointwise(&x, block);
        }

        // Global average pooling -> эмбеддинг, затем dense + sigmoid.
        let hw = x.h * x.w;
        let mut embeddings = vec![0f32; n * EMBEDDING_SIZE];
        for b in 0..n {
            let emb = &mut embeddings[b * EMBEDDING_SIZE..(b + 1) * EMBEDDING_SIZE];
            for p in 0..hw {
                let px = &x.data[(b * hw + p) * x.c..(b * hw + p + 1) * x.c];
                emb.iter_mut().zip(px).for_each(|(e, v)| *e += v);
            }
            emb.iter_mut().for_each(|e| *e /= hw as f32);
        }
        let scores = self.head(&embeddings, n);
        (0..n)
            .map(|b| Prediction {
                scores: scores[b * NUM_CLASSES..(b + 1) * NUM_CLASSES].into(),
                embedding: embeddings[b * EMBEDDING_SIZE..(b + 1) * EMBEDDING_SIZE].into(),
            })
            .collect()
    }

    /// Вероятности AudioSet по готовым эмбеддингам `[n][EMBEDDING_SIZE]` — без свёрточной
    /// части сети; результат `[n][NUM_CLASSES]`. Нужно, чтобы пересчитать сохранённые
    /// окна с другими порогами.
    pub fn scores_from_embeddings(&self, embeddings: &[f32]) -> Vec<f32> {
        assert_eq!(embeddings.len() % EMBEDDING_SIZE, 0);
        embeddings
            .par_chunks(BATCH * EMBEDDING_SIZE)
            .flat_map_iter(|chunk| self.head(chunk, chunk.len() / EMBEDDING_SIZE))
            .collect()
    }

    /// Dense + sigmoid: `[n][EMBEDDING_SIZE]` → `[n][NUM_CLASSES]`.
    fn head(&self, embeddings: &[f32], n: usize) -> Vec<f32> {
        let mut logits = vec![0f32; n * NUM_CLASSES];
        for b in 0..n {
            logits[b * NUM_CLASSES..(b + 1) * NUM_CLASSES].copy_from_slice(&self.logits_b);
        }
        gemm(
            embeddings,
            &self.logits_w,
            &mut logits,
            n,
            EMBEDDING_SIZE,
            NUM_CLASSES,
            1.0,
        );
        logits
            .iter_mut()
            .for_each(|z| *z = 1.0 / (1.0 + (-*z).exp()));
        logits
    }
}

/// Умножает веса на масштаб BatchNorm по последней оси (выходной канал).
fn fold(w: &mut [f32], scale: &[f32]) {
    for row in w.chunks_exact_mut(scale.len()) {
        row.iter_mut().zip(scale).for_each(|(w, s)| *w *= s);
    }
}

struct Tensor {
    data: Vec<f32>,
    n: usize,
    h: usize,
    w: usize,
    c: usize,
}

/// Padding 'same' как в TensorFlow: (размер выхода, отступ сверху/слева).
fn same_pad(input: usize, stride: usize) -> (usize, usize) {
    let out = input.div_ceil(stride);
    let total = ((out - 1) * stride + 3).saturating_sub(input);
    (out, total / 2)
}

fn conv3x3_s2(x: &Tensor, conv: &Conv) -> Tensor {
    debug_assert_eq!(x.c, 1);
    let (oh, pt) = same_pad(x.h, 2);
    let (ow, pl) = same_pad(x.w, 2);
    let co = conv.cout;
    let mut out = vec![0f32; x.n * oh * ow * co];
    for b in 0..x.n {
        for oy in 0..oh {
            for ox in 0..ow {
                let o = &mut out[((b * oh + oy) * ow + ox) * co..][..co];
                o.copy_from_slice(&conv.b);
                for ky in 0..3 {
                    let Some(iy) = (oy * 2 + ky).checked_sub(pt).filter(|&v| v < x.h) else {
                        continue;
                    };
                    for kx in 0..3 {
                        let Some(ix) = (ox * 2 + kx).checked_sub(pl).filter(|&v| v < x.w) else {
                            continue;
                        };
                        let v = x.data[(b * x.h + iy) * x.w + ix];
                        let w = &conv.w[(ky * 3 + kx) * co..][..co];
                        o.iter_mut().zip(w).for_each(|(o, w)| *o += v * w);
                    }
                }
                o.iter_mut().for_each(|v| *v = v.max(0.0));
            }
        }
    }
    Tensor {
        data: out,
        n: x.n,
        h: oh,
        w: ow,
        c: co,
    }
}

fn depthwise(x: &Tensor, l: &Separable) -> Tensor {
    let c = l.cin;
    let (oh, pt) = same_pad(x.h, l.stride);
    let (ow, pl) = same_pad(x.w, l.stride);
    let mut out = vec![0f32; x.n * oh * ow * c];
    for b in 0..x.n {
        for oy in 0..oh {
            for ox in 0..ow {
                let o = &mut out[((b * oh + oy) * ow + ox) * c..][..c];
                o.copy_from_slice(&l.dw_b);
                for ky in 0..3 {
                    let Some(iy) = (oy * l.stride + ky).checked_sub(pt).filter(|&v| v < x.h) else {
                        continue;
                    };
                    for kx in 0..3 {
                        let Some(ix) = (ox * l.stride + kx).checked_sub(pl).filter(|&v| v < x.w)
                        else {
                            continue;
                        };
                        let px = &x.data[((b * x.h + iy) * x.w + ix) * c..][..c];
                        let w = &l.dw_w[(ky * 3 + kx) * c..][..c];
                        for ((o, p), w) in o.iter_mut().zip(px).zip(w) {
                            *o += p * w;
                        }
                    }
                }
                o.iter_mut().for_each(|v| *v = v.max(0.0));
            }
        }
    }
    Tensor {
        data: out,
        n: x.n,
        h: oh,
        w: ow,
        c,
    }
}

fn pointwise(x: &Tensor, l: &Separable) -> Tensor {
    let m = x.n * x.h * x.w;
    let mut out = Vec::with_capacity(m * l.cout);
    for _ in 0..m {
        out.extend_from_slice(&l.pw_b);
    }
    gemm(&x.data, &l.pw_w, &mut out, m, l.cin, l.cout, 1.0);
    out.iter_mut().for_each(|v| *v = v.max(0.0));
    Tensor {
        data: out,
        n: x.n,
        h: x.h,
        w: x.w,
        c: l.cout,
    }
}

/// `c[m×n] = a[m×k] · b[k×n] + beta·c`, всё row-major.
fn gemm(a: &[f32], b: &[f32], c: &mut [f32], m: usize, k: usize, n: usize, beta: f32) {
    assert!(a.len() >= m * k && b.len() >= k * n && c.len() >= m * n);
    // SAFETY: размеры буферов проверены выше, шаги соответствуют row-major раскладке.
    unsafe {
        matrixmultiply::sgemm(
            m,
            k,
            n,
            1.0,
            a.as_ptr(),
            k as isize,
            1,
            b.as_ptr(),
            n as isize,
            1,
            beta,
            c.as_mut_ptr(),
            n as isize,
            1,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_padding_matches_tensorflow() {
        assert_eq!(same_pad(96, 2), (48, 0));
        assert_eq!(same_pad(64, 2), (32, 0));
        assert_eq!(same_pad(48, 1), (48, 1));
        assert_eq!(same_pad(3, 2), (2, 1));
    }

    #[test]
    fn gemm_small() {
        let a = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]; // 2×3
        let b = [1.0, 0.0, 0.0, 1.0, 1.0, 1.0]; // 3×2
        let mut c = [10.0, 20.0, 30.0, 40.0];
        gemm(&a, &b, &mut c, 2, 3, 2, 1.0);
        assert_eq!(c, [14.0, 25.0, 40.0, 51.0]);
    }
}
