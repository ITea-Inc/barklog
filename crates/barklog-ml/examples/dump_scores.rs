//! Прогоняет сырой сигнал (f32 LE, 16 кГц моно) через Rust-YAMNet и сохраняет
//! вероятности `[n][521]` и эмбеддинги `[n][1024]` как f32 LE.
//! Используется `tools/verify_yamnet.py` для сверки с TensorFlow.
//!
//!     cargo run -p barklog-ml --release --example dump_scores -- model wave.f32 out_prefix

use barklog_ml::{FeatureExtractor, Yamnet};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, model, wave, out] = args.as_slice() else {
        eprintln!("usage: dump_scores <model.safetensors> <wave.f32> <out_prefix>");
        std::process::exit(2);
    };
    let model = Yamnet::load(model).expect("load model");
    let bytes = std::fs::read(wave).expect("read wave");
    let samples: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();

    let mut fx = FeatureExtractor::new();
    let mut patches = Vec::new();
    fx.push(&samples, &mut patches);
    fx.finish(&mut patches);
    let preds = model.predict(&patches);

    let mut scores = Vec::new();
    let mut emb = Vec::new();
    let mut feats = Vec::new();
    for (p, patch) in preds.iter().zip(&patches) {
        scores.extend(p.scores.iter().flat_map(|v| v.to_le_bytes()));
        emb.extend(p.embedding.iter().flat_map(|v| v.to_le_bytes()));
        feats.extend(patch.features.iter().flat_map(|v| v.to_le_bytes()));
    }
    std::fs::write(format!("{out}.scores.f32"), scores).unwrap();
    std::fs::write(format!("{out}.emb.f32"), emb).unwrap();
    std::fs::write(format!("{out}.patches.f32"), feats).unwrap();
    println!("{} patches", preds.len());
}
