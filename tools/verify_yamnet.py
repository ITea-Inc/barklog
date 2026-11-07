"""Сверяет Rust-реализацию YAMNet с оригиналом на TensorFlow.

Нужны: tensorflow, numpy, исходники YAMNet (yamnet.py, params.py, features.py)
и yamnet.h5 в одной папке (https://github.com/tensorflow/models/tree/master/research/audioset/yamnet).

    python tools/verify_yamnet.py <yamnet_src_dir> <work_dir>

Скрипт генерирует тестовый сигнал, запускает TF-модель и Rust-пример `dump_scores`,
затем печатает максимальные расхождения признаков, вероятностей и эмбеддингов.
"""

import os
import subprocess
import sys

import numpy as np


def make_signal(sr=16000, seconds=10.0, seed=7):
    rng = np.random.default_rng(seed)
    t = np.arange(int(sr * seconds)) / sr
    chirp = 0.3 * np.sin(2 * np.pi * (300 + 400 * t) * t)
    bursts = (np.sin(2 * np.pi * 1.5 * t) > 0.7) * rng.normal(0, 0.2, t.size)
    tone = 0.2 * np.sin(2 * np.pi * 900 * t) * (t > 5)
    return (chirp + bursts + tone).astype(np.float32)


def main(src_dir, work_dir):
    os.makedirs(work_dir, exist_ok=True)
    sys.path.insert(0, src_dir)
    import params as yparams
    import yamnet as ymodel

    wave = make_signal()
    wave_path = os.path.join(work_dir, "wave.f32")
    wave.tofile(wave_path)

    p = yparams.Params()
    model = ymodel.yamnet_frames_model(p)
    model.load_weights(os.path.join(src_dir, "yamnet.h5"))
    tf_scores, tf_emb, _ = model(wave)
    tf_scores, tf_emb = np.asarray(tf_scores), np.asarray(tf_emb)

    import features as yfeatures
    import tensorflow as tf
    padded = yfeatures.pad_waveform(tf.constant(wave), p)
    _, tf_patches = yfeatures.waveform_to_log_mel_spectrogram_patches(padded, p)
    tf_patches = np.asarray(tf_patches)

    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    out = os.path.join(work_dir, "rust")
    subprocess.run(
        ["cargo", "run", "-q", "-p", "barklog-ml", "--release", "--example", "dump_scores", "--",
         os.path.join(root, "models", "yamnet.safetensors"), wave_path, out],
        check=True, cwd=root)
    rs_scores = np.fromfile(out + ".scores.f32", np.float32).reshape(-1, 521)
    rs_emb = np.fromfile(out + ".emb.f32", np.float32).reshape(-1, 1024)
    rs_patches = np.fromfile(out + ".patches.f32", np.float32).reshape(-1, 96, 64)

    print(f"patches: tf={tf_scores.shape[0]} rust={rs_scores.shape[0]}")
    n = min(tf_scores.shape[0], rs_scores.shape[0])
    print(f"log-mel   max |diff| = {np.abs(tf_patches[:n] - rs_patches[:n]).max():.2e}")
    print(f"scores    max |diff| = {np.abs(tf_scores[:n] - rs_scores[:n]).max():.2e}")
    print(f"embedding max |diff| = {np.abs(tf_emb[:n] - rs_emb[:n]).max():.2e}")
    same_top = (tf_scores[:n].argmax(1) == rs_scores[:n].argmax(1)).mean()
    print(f"top-1 agreement = {same_top:.1%}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
