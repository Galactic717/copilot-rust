#!/usr/bin/env python3
"""Rebuilds resources/silero_vad_16k.safetensors from the official ONNX export.

The safetensors file in the Silero repository is a different snapshot from its ONNX v6.2 model, so the weights are lifted
out of the ONNX file instead. Reads protobuf by hand to need nothing beyond the standard library.

    curl -LO https://github.com/snakers4/silero-vad/raw/master/src/silero_vad/data/silero_vad_16k_op15.onnx
    python scripts/export_silero_weights.py silero_vad_16k_op15.onnx resources/silero_vad_16k.safetensors
"""
import json
import struct
import sys

NAMES = {
    "stft.forward_basis_buffer": "stft_conv.weight",
    "encoder.0.reparam_conv.weight": "conv1.weight",
    "encoder.0.reparam_conv.bias": "conv1.bias",
    "encoder.1.reparam_conv.weight": "conv2.weight",
    "encoder.1.reparam_conv.bias": "conv2.bias",
    "encoder.2.reparam_conv.weight": "conv3.weight",
    "encoder.2.reparam_conv.bias": "conv3.bias",
    "encoder.3.reparam_conv.weight": "conv4.weight",
    "encoder.3.reparam_conv.bias": "conv4.bias",
    "decoder.rnn.weight_ih": "lstm_cell.weight_ih",
    "decoder.rnn.weight_hh": "lstm_cell.weight_hh",
    "decoder.rnn.bias_ih": "lstm_cell.bias_ih",
    "decoder.rnn.bias_hh": "lstm_cell.bias_hh",
    "decoder.decoder.2.weight": "final_conv.weight",
    "decoder.decoder.2.bias": "final_conv.bias",
}


def fields(buf):
    """Yields (field number, wire type, value) for one protobuf message."""
    i = 0

    def varint():
        nonlocal i
        shift = value = 0
        while True:
            b = buf[i]
            i += 1
            value |= (b & 0x7F) << shift
            shift += 7
            if b < 0x80:
                return value

    while i < len(buf):
        key = varint()
        field, wire = key >> 3, key & 7
        if wire == 0:
            yield field, wire, varint()
        elif wire == 1:
            yield field, wire, buf[i : i + 8]
            i += 8
        elif wire == 2:
            n = varint()
            yield field, wire, buf[i : i + n]
            i += n
        elif wire == 5:
            yield field, wire, buf[i : i + 4]
            i += 4
        else:
            raise ValueError(f"wire type {wire}")


def tensor(buf):
    dims, name, raw, floats = [], "", None, []
    for f, w, v in fields(buf):
        if f == 1 and w == 0:
            dims.append(v)
        elif f == 1 and w == 2:
            for _, _, d in fields_packed(v):
                dims.append(d)
        elif f == 8:
            name = v.decode()
        elif f == 9:
            raw = v
        elif f == 4:
            floats.append(v)
    return name, dims, raw if raw is not None else b"".join(floats)


def fields_packed(buf):
    i = 0
    while i < len(buf):
        shift = value = 0
        while True:
            b = buf[i]
            i += 1
            value |= (b & 0x7F) << shift
            shift += 7
            if b < 0x80:
                break
        yield 0, 0, value


def walk_graph(buf, out):
    for f, _, v in fields(buf):
        if f == 5:  # GraphProto.initializer
            name, dims, raw = tensor(v)
            out.setdefault(name, (dims, raw))
        elif f == 1:  # GraphProto.node
            for nf, _, nv in fields(v):
                if nf == 5:  # NodeProto.attribute
                    for af, _, av in fields(nv):
                        if af == 6:  # AttributeProto.g: the If branches
                            walk_graph(av, out)
                        elif af == 5:  # AttributeProto.t: a Constant node's value
                            name, dims, raw = tensor(av)
                            out.setdefault(name, (dims, raw))


def main(src, dest):
    model = open(src, "rb").read()
    found = {}
    for f, _, v in fields(model):
        if f == 7:  # ModelProto.graph
            walk_graph(v, found)
    header, blobs, offset = {}, [], 0
    for onnx_name, st_name in NAMES.items():
        match = [(n, t) for n, t in found.items() if n.endswith(onnx_name)]
        if len(match) != 1:
            sys.exit(f"{onnx_name}: {[n for n, _ in match]}; initializers: {sorted(found)[:40]}")
        dims, raw = match[0][1]
        header[st_name] = {"dtype": "F32", "shape": dims, "data_offsets": [offset, offset + len(raw)]}
        blobs.append(raw)
        offset += len(raw)
        print(st_name, dims)
    head = json.dumps(header, separators=(",", ":")).encode()
    with open(dest, "wb") as w:
        w.write(struct.pack("<Q", len(head)) + head + b"".join(blobs))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
