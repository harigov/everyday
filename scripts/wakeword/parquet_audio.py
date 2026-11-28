"""Hugging Face audio parquet shards (LibriSpeech, People's Speech) to raw
16 kHz mono int16, one file per shard, for `features.py long`.

    python parquet_audio.py OUT.raw SHARD.parquet...
"""

import io
import sys

import numpy as np
import pyarrow.parquet as pq
import soundfile as sf
from scipy.signal import resample_poly


def main():
    out, shards = sys.argv[1], sys.argv[2:]
    with open(out, "wb") as f:
        for shard in shards:
            table = pq.ParquetFile(shard)
            for batch in table.iter_batches(columns=["audio"], batch_size=256):
                for item in batch.column("audio").to_pylist():
                    audio, rate = sf.read(io.BytesIO(item["bytes"]), dtype="float32", always_2d=True)
                    audio = audio.mean(axis=1)
                    if rate != 16_000:
                        audio = resample_poly(audio, 16_000, rate)
                    f.write((np.clip(audio, -1, 1) * 32767).astype("<i2").tobytes())


if __name__ == "__main__":
    main()
