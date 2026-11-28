"""Write a wake-word model as one file the app reads (`speech::spotter`).

    python export.py --models MODELS [--head head.pt] --name Jatayu --out jatayu.jww

The file: `JWW1`, a little-endian u32 header length, the header as JSON, then
every tensor as little-endian f32, one after another. The header lists the
mel spectrogram's constants, the embedding network as layers in order -- a
convolution over (time, frequency) with its activation, or a max pool -- and
the head as layers in order, each naming its tensors by offset and shape.

Without `--head`, only the front end: what the Rust tests check the streaming
embedding against.
"""

from __future__ import annotations

import argparse
import json
import os
import struct

import numpy as np


class Writer:
    def __init__(self):
        self.blobs: list[np.ndarray] = []
        self.offset = 0

    def add(self, array: np.ndarray) -> dict:
        array = np.ascontiguousarray(array, dtype="<f4")
        entry = {"offset": self.offset, "shape": list(array.shape)}
        self.blobs.append(array)
        self.offset += array.size
        return entry


def bound(value) -> float | None:
    """A clip's bound, or None for none: JSON has no infinity."""
    value = float(value)
    return None if np.isinf(value) else value


def front_end(models: str, w: Writer) -> dict:
    import onnx
    from onnx import numpy_helper

    mel = onnx.load(os.path.join(models, "melspectrogram.onnx"))
    init = {i.name: numpy_helper.to_array(i) for i in mel.graph.initializer}
    mel_spec = {
        "real": w.add(init["0.stft.conv_real.weight"].reshape(257, 512)),
        "imag": w.add(init["0.stft.conv_imag.weight"].reshape(257, 512)),
        "filters": w.add(init["1.melW"]),
        "low": bound(init["onnx::Clip_39"]),
        "high": bound(init["onnx::Clip_40"]),
        "top": bound(init["onnx::Clip_41"]),
        "top_db": 80.0,
        "window": 512,
        "hop": 160,
    }

    emb = onnx.load(os.path.join(models, "embedding_model.onnx"))
    init = {i.name: numpy_helper.to_array(i) for i in emb.graph.initializer}
    layers: list[dict] = []
    for node in emb.graph.node:
        attrs = {a.name: onnx.helper.get_attribute_value(a) for a in node.attribute}
        if node.op_type == "Conv":
            weight = init[node.input[1]]
            kt, kf = int(weight.shape[2]), int(weight.shape[3])
            pads = attrs.get("pads", [0, 0, 0, 0])
            assert pads[0] == 0 and pads[2] == 0, "time must not be padded: the stream depends on it"
            layer = {
                "op": "conv",
                "cin": int(weight.shape[1]),
                "cout": int(weight.shape[0]),
                "kt": kt,
                "kf": kf,
                "pad_f": int(pads[1]),
                "weight": w.add(weight),
                "act": False,
            }
            if len(node.input) > 2:
                layer["bias"] = w.add(init[node.input[2]])
            layers.append(layer)
        elif node.op_type == "LeakyRelu":
            assert layers[-1]["op"] == "conv"
            layers[-1]["act"] = True
            layers[-1]["slope"] = float(attrs["alpha"])
        elif node.op_type == "Max":
            floor = float(init[node.input[1]].ravel()[0])
            layers[-1]["floor"] = floor
        elif node.op_type == "MaxPool":
            kt, kf = attrs["kernel_shape"]
            assert list(attrs["strides"]) == [kt, kf]
            layers.append({"op": "pool", "pt": int(kt), "pf": int(kf)})
        elif node.op_type == "Reshape":
            continue
        else:
            raise ValueError(f"an embedding layer this file cannot describe: {node.op_type}")
    return {"mel": mel_spec, "embedding": layers, "mel_frames": 76, "step": 1280, "context": 480}


def head_layers(path: str, w: Writer) -> tuple[list[dict], dict]:
    """The head saved by train.py: its layers in order, and what it was
    trained and tested to."""
    import torch

    saved = torch.load(path, map_location="cpu")
    state, spec = saved["state"], saved["spec"]
    layers = []
    for layer in spec["layers"]:
        if layer["op"] == "dense":
            layers.append(
                {
                    "op": "dense",
                    "weight": w.add(state[layer["weight"]].numpy()),
                    "bias": w.add(state[layer["bias"]].numpy()),
                }
            )
        elif layer["op"] == "layernorm":
            layers.append(
                {
                    "op": "layernorm",
                    "weight": w.add(state[layer["weight"]].numpy()),
                    "bias": w.add(state[layer["bias"]].numpy()),
                    "eps": layer["eps"],
                }
            )
        else:
            layers.append({"op": layer["op"]})
    return layers, saved["meta"]


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--models", required=True)
    p.add_argument("--head")
    p.add_argument("--name", default="Jatayu")
    p.add_argument("--threshold", type=float)
    p.add_argument("--out", required=True)
    args = p.parse_args()
    w = Writer()
    header = {"format": 1, "name": args.name, **front_end(args.models, w)}
    if args.head:
        layers, meta = head_layers(args.head, w)
        header["head"] = {"window": 16, "layers": layers}
        header["threshold"] = args.threshold if args.threshold is not None else meta["threshold"]
        header["trained"] = meta
    text = json.dumps(header, separators=(",", ":")).encode()
    with open(args.out, "wb") as f:
        f.write(b"JWW1")
        f.write(struct.pack("<I", len(text)))
        f.write(text)
        for blob in w.blobs:
            f.write(blob.tobytes())
    print(f"{args.out}: {os.path.getsize(args.out)} bytes, {w.offset} floats")


if __name__ == "__main__":
    main()
