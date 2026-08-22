"""A minimal MessagePack codec — enough for butai's wire format.

The daemon encodes with rmp-serde's `to_vec_named`, so every struct arrives as
a map with string keys and every externally-tagged enum is either a one-entry
map or a bare string. That subset is small enough to implement here, which
keeps the whole suite on the Python standard library while still exercising the
encoding the shipped TUI actually negotiates.

Not supported (and not used by butai): ext/timestamp types.
"""

import struct

__all__ = ["encode", "decode", "MsgpackError"]


class MsgpackError(Exception):
    pass


# ---------------------------------------------------------------------------
# encode
# ---------------------------------------------------------------------------


def encode(obj):
    """Serialize a Python value to MessagePack bytes."""
    out = bytearray()
    _enc(obj, out)
    return bytes(out)


def _enc(obj, out):
    if obj is None:
        out.append(0xC0)
    elif obj is True:
        out.append(0xC3)
    elif obj is False:
        out.append(0xC2)
    elif isinstance(obj, int):
        _enc_int(obj, out)
    elif isinstance(obj, float):
        out.append(0xCB)
        out += struct.pack(">d", obj)
    elif isinstance(obj, str):
        _enc_str(obj, out)
    elif isinstance(obj, (bytes, bytearray)):
        _enc_bin(bytes(obj), out)
    elif isinstance(obj, (list, tuple)):
        _enc_array(obj, out)
    elif isinstance(obj, dict):
        _enc_map(obj, out)
    else:
        raise MsgpackError(f"cannot encode {type(obj).__name__}")


def _enc_int(n, out):
    if 0 <= n < 0x80:
        out.append(n)
    elif -0x20 <= n < 0:
        out.append(n & 0xFF)
    elif 0 <= n <= 0xFF:
        out += b"\xcc" + struct.pack(">B", n)
    elif 0 <= n <= 0xFFFF:
        out += b"\xcd" + struct.pack(">H", n)
    elif 0 <= n <= 0xFFFFFFFF:
        out += b"\xce" + struct.pack(">I", n)
    elif 0 <= n <= 0xFFFFFFFFFFFFFFFF:
        out += b"\xcf" + struct.pack(">Q", n)
    elif -0x80 <= n < 0:
        out += b"\xd0" + struct.pack(">b", n)
    elif -0x8000 <= n < 0:
        out += b"\xd1" + struct.pack(">h", n)
    elif -0x80000000 <= n < 0:
        out += b"\xd2" + struct.pack(">i", n)
    elif -0x8000000000000000 <= n < 0:
        out += b"\xd3" + struct.pack(">q", n)
    else:
        raise MsgpackError(f"integer out of range: {n}")


def _enc_str(s, out):
    raw = s.encode("utf-8")
    n = len(raw)
    if n < 32:
        out.append(0xA0 | n)
    elif n <= 0xFF:
        out += b"\xd9" + struct.pack(">B", n)
    elif n <= 0xFFFF:
        out += b"\xda" + struct.pack(">H", n)
    else:
        out += b"\xdb" + struct.pack(">I", n)
    out += raw


def _enc_bin(raw, out):
    n = len(raw)
    if n <= 0xFF:
        out += b"\xc4" + struct.pack(">B", n)
    elif n <= 0xFFFF:
        out += b"\xc5" + struct.pack(">H", n)
    else:
        out += b"\xc6" + struct.pack(">I", n)
    out += raw


def _enc_array(items, out):
    n = len(items)
    if n < 16:
        out.append(0x90 | n)
    elif n <= 0xFFFF:
        out += b"\xdc" + struct.pack(">H", n)
    else:
        out += b"\xdd" + struct.pack(">I", n)
    for item in items:
        _enc(item, out)


def _enc_map(d, out):
    n = len(d)
    if n < 16:
        out.append(0x80 | n)
    elif n <= 0xFFFF:
        out += b"\xde" + struct.pack(">H", n)
    else:
        out += b"\xdf" + struct.pack(">I", n)
    for key, value in d.items():
        _enc(key, out)
        _enc(value, out)


# ---------------------------------------------------------------------------
# decode
# ---------------------------------------------------------------------------


def decode(raw):
    """Deserialize MessagePack bytes to a Python value.

    Raises if the buffer holds trailing bytes — butai frames are exactly one
    value, so trailing data means a framing bug worth failing on.
    """
    value, pos = _dec(raw, 0)
    if pos != len(raw):
        raise MsgpackError(f"{len(raw) - pos} trailing bytes after value")
    return value


def _dec(raw, pos):
    if pos >= len(raw):
        raise MsgpackError("truncated input")
    tag = raw[pos]
    pos += 1

    if tag <= 0x7F:  # positive fixint
        return tag, pos
    if tag >= 0xE0:  # negative fixint
        return tag - 0x100, pos
    if 0x80 <= tag <= 0x8F:  # fixmap
        return _dec_map(raw, pos, tag & 0x0F)
    if 0x90 <= tag <= 0x9F:  # fixarray
        return _dec_array(raw, pos, tag & 0x0F)
    if 0xA0 <= tag <= 0xBF:  # fixstr
        return _dec_str(raw, pos, tag & 0x1F)

    if tag == 0xC0:
        return None, pos
    if tag == 0xC2:
        return False, pos
    if tag == 0xC3:
        return True, pos

    if tag in (0xC4, 0xC5, 0xC6):
        width = {0xC4: 1, 0xC5: 2, 0xC6: 4}[tag]
        n, pos = _dec_uint(raw, pos, width)
        return _take(raw, pos, n)

    if tag == 0xCA:
        return struct.unpack_from(">f", raw, pos)[0], pos + 4
    if tag == 0xCB:
        return struct.unpack_from(">d", raw, pos)[0], pos + 8

    if tag in (0xCC, 0xCD, 0xCE, 0xCF):
        width = {0xCC: 1, 0xCD: 2, 0xCE: 4, 0xCF: 8}[tag]
        return _dec_uint(raw, pos, width)

    if tag in (0xD0, 0xD1, 0xD2, 0xD3):
        fmt = {0xD0: ">b", 0xD1: ">h", 0xD2: ">i", 0xD3: ">q"}[tag]
        size = struct.calcsize(fmt)
        return struct.unpack_from(fmt, raw, pos)[0], pos + size

    if tag in (0xD9, 0xDA, 0xDB):
        width = {0xD9: 1, 0xDA: 2, 0xDB: 4}[tag]
        n, pos = _dec_uint(raw, pos, width)
        return _dec_str(raw, pos, n)

    if tag in (0xDC, 0xDD):
        width = 2 if tag == 0xDC else 4
        n, pos = _dec_uint(raw, pos, width)
        return _dec_array(raw, pos, n)

    if tag in (0xDE, 0xDF):
        width = 2 if tag == 0xDE else 4
        n, pos = _dec_uint(raw, pos, width)
        return _dec_map(raw, pos, n)

    raise MsgpackError(f"unsupported msgpack tag 0x{tag:02x}")


def _dec_uint(raw, pos, width):
    fmt = {1: ">B", 2: ">H", 4: ">I", 8: ">Q"}[width]
    if pos + width > len(raw):
        raise MsgpackError("truncated integer")
    return struct.unpack_from(fmt, raw, pos)[0], pos + width


def _take(raw, pos, n):
    if pos + n > len(raw):
        raise MsgpackError("truncated payload")
    return raw[pos : pos + n], pos + n


def _dec_str(raw, pos, n):
    payload, pos = _take(raw, pos, n)
    return payload.decode("utf-8", "replace"), pos


def _dec_array(raw, pos, n):
    items = []
    for _ in range(n):
        value, pos = _dec(raw, pos)
        items.append(value)
    return items, pos


def _dec_map(raw, pos, n):
    out = {}
    for _ in range(n):
        key, pos = _dec(raw, pos)
        value, pos = _dec(raw, pos)
        out[key] = value
    return out, pos
