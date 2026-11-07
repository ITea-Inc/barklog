"""Конвертирует веса YAMNet (Keras .h5) в safetensors для barklog-ml.

Исходные веса (Apache 2.0): https://storage.googleapis.com/audioset/yamnet.h5

    pip install h5py numpy safetensors
    python tools/convert_yamnet.py yamnet.h5 models/yamnet.safetensors

Имена тензоров упрощаются: "layer1/conv/bn/layer1/conv/bn/beta:0" -> "layer1/conv/bn/beta".
"""

import sys

import h5py
import numpy as np
from safetensors.numpy import save_file


def tensor_name(path: str) -> str:
    path = path.rsplit(":", 1)[0]
    first = path.split("/", 1)[0]
    # Keras дублирует путь слоя: <layer>/<layer>/<weight>
    second = path.find("/" + first + "/")
    return path[second + 1:] if second >= 0 else path


def main(src: str, dst: str) -> None:
    tensors = {}
    with h5py.File(src, "r") as f:
        def visit(path, obj):
            if isinstance(obj, h5py.Dataset):
                tensors[tensor_name(path)] = np.ascontiguousarray(obj[()], dtype=np.float32)
        f.visititems(visit)

    save_file(tensors, dst, metadata={"source": "yamnet.h5", "format": "keras-nhwc"})
    total = sum(t.size for t in tensors.values())
    print(f"{len(tensors)} tensors, {total:,} params -> {dst}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
