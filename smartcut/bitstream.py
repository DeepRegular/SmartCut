"""Minimal Annex-B access-unit surgery.

Only one operation is needed: drop specific pictures from the front of a
copied segment.  When a copy starts at an open-GOP I picture, the pictures
that follow it in decode order but present before it reference the previous
GOP, which is not in the output.  ffmpeg cannot drop them -- a stream copy
has no way to filter by presentation time -- so we cut them out of the
elementary stream ourselves.
"""
from __future__ import annotations

# start-of-picture detection differs per codec, everything else is shared
VCL_H264 = range(1, 6)
VCL_HEVC = range(0, 32)


def nal_units(data: bytes) -> list[tuple[int, int, int]]:
    """(start, payload_start, end) for every NAL unit, start codes included."""
    out: list[tuple[int, int, int]] = []
    n = len(data)
    i = data.find(b"\x00\x00\x01")
    while i >= 0:
        payload = i + 3
        # a 4-byte start code is a 3-byte one with an extra leading zero
        start = i - 1 if i > 0 and data[i - 1] == 0 else i
        nxt = data.find(b"\x00\x00\x01", payload)
        end = n if nxt < 0 else (nxt - 1 if data[nxt - 1] == 0 else nxt)
        out.append((start, payload, end))
        i = nxt
    return out


def _picture_start(data: bytes, payload: int, end: int, codec: str) -> bool | None:
    """True at the first slice of a picture, False for other slices, None for non-VCL."""
    if codec in ("mpeg2video", "mpeg4"):
        # MPEG-2 start codes, not NAL units: 0x00 opens a picture, 0x01-0xAF
        # are its slices, and everything from 0xB0 up is a header that belongs
        # to the picture that follows it.
        code = data[payload]
        if code == 0x00:
            return True
        if code <= 0xAF:
            return False
        return None
    if codec == "hevc":
        if end - payload < 3:
            return None
        nal_type = (data[payload] >> 1) & 0x3F
        if nal_type not in VCL_HEVC:
            return None
        # first_slice_segment_in_pic_flag is the leading bit of the slice header
        return bool(data[payload + 2] & 0x80)
    if end - payload < 2:
        return None
    nal_type = data[payload] & 0x1F
    if nal_type not in VCL_H264:
        return None
    # first_mb_in_slice is a ue(v); it is zero exactly when the leading bit is set
    return bool(data[payload + 1] & 0x80)


def access_units(data: bytes, codec: str) -> list[tuple[int, int]]:
    """Byte ranges of each access unit, in decode order.

    Headers that precede a picture's data -- parameter sets, SEI, sequence and
    GOP headers, delimiters -- are carried with that picture, which is what
    keeps the SPS/PPS attached to the IDR at the head of a copied segment.

    The two codec families mark a picture differently: in H.264/HEVC the first
    slice *is* the start of the picture, while MPEG-2 opens with a picture
    start code and only reaches its slices a few headers later.  So a header
    closes the current access unit only once actual picture data has been
    seen, not merely because a picture was announced.
    """
    mpeg_style = codec in ("mpeg2video", "mpeg4")
    units: list[tuple[int, int]] = []
    au_start: int | None = None
    saw_data = False
    for start, payload, end in nal_units(data):
        kind = _picture_start(data, payload, end, codec)
        if kind is None:                              # header
            if saw_data:
                units.append((au_start, start))
                au_start, saw_data = start, False
            elif au_start is None:
                au_start = start
            continue
        if kind:                                      # picture begins
            if au_start is not None and saw_data:
                units.append((au_start, start))
                au_start = start
            elif au_start is None:
                au_start = start
            # in H.264/HEVC that first slice already is picture data
            saw_data = not mpeg_style
        else:                                         # further slices
            if au_start is None:
                au_start = start
            saw_data = True
    if au_start is not None and au_start < len(data):
        units.append((au_start, len(data)))
    return units


def drop_access_units(path: str, indices: list[int], codec: str) -> int:
    """Remove the given access units (by decode-order index) from a file."""
    if not indices:
        return 0
    with open(path, "rb") as fh:
        data = fh.read()
    units = access_units(data, codec)
    drop = {i for i in indices if 0 <= i < len(units)}
    if not drop:
        return 0
    with open(path, "wb") as fh:
        for i, (start, end) in enumerate(units):
            if i not in drop:
                fh.write(data[start:end])
    return len(drop)


# HEVC leading pictures: the _N variants are sub-layer non-reference
HEVC_LEADING_NONREF = {6, 8}      # RADL_N, RASL_N
HEVC_LEADING_REF = {7, 9}         # RADL_R, RASL_R


def au_is_reference(data: bytes, au: tuple[int, int], codec: str) -> bool:
    """Is this access unit used as a reference by later pictures?

    Decides whether a leading picture may simply be cut out of a copied
    segment.  MPEG-2 B pictures never reference-back, but H.264/HEVC
    encoders routinely build B-pyramids whose leading pictures *are*
    references -- drop one of those and every picture that depended on it
    decodes to garbage.
    """
    start, end = au
    if codec in ("mpeg2video", "mpeg4"):
        # picture_coding_type sits just past the 10-bit temporal_reference:
        # 1=I, 2=P, 3=B.  Only B pictures are never referenced back.
        for ns, payload, ne in nal_units(data[start:end]):
            if data[start + payload] != 0x00 or ne - payload < 3:
                continue
            return ((data[start + payload + 2] >> 3) & 0x07) != 3
        return True
    for ns, payload, ne in nal_units(data[start:end]):
        if codec == "hevc":
            if ne - payload < 2:
                continue
            nal_type = (data[start + payload] >> 1) & 0x3F
            if nal_type in HEVC_LEADING_NONREF:
                return False
            if nal_type in HEVC_LEADING_REF or nal_type in VCL_HEVC:
                return True
        else:
            if ne - payload < 1:
                continue
            b = data[start + payload]
            if (b & 0x1F) in VCL_H264:
                return bool((b >> 5) & 0x03)      # nal_ref_idc
    return True                   # unknown -> assume referenced, stay safe
