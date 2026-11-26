//! AV1 packets as the TV's decoder takes them: Section 5 temporal units, with a temporal delimiter first, the
//! sequence header of the file's `av1C` record in front of each keyframe and every OBU carrying its size. LG's
//! pipeline has no AV1 parser and is given no codec data, so the configuration has to travel in the stream. From
//! Native Jelly (`ff.rs`), where it played on a webOS 6.5.3 set. VP9 needs nothing: its frames describe themselves.

const SEQUENCE_HEADER: u8 = 1;
const TEMPORAL_DELIMITER: u8 = 2;
const METADATA: u8 = 5;
/// A temporal delimiter with an empty payload: header 0x12 (type 2, with a size), size 0.
const TD: [u8; 2] = [0x12, 0x00];

/// A leb128 value at the front of `b`, and how many bytes it took; at most 8 bytes and 32 bits, as the spec bounds it.
fn read_leb128(b: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (i, &byte) in b.iter().take(8).enumerate() {
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return (value <= u64::from(u32::MAX)).then_some((value, i + 1));
        }
    }
    None
}

fn write_leb128(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            return out.push(byte);
        }
        out.push(byte | 0x80);
    }
}

/// One OBU in a run: where its header starts, the header's length, where its payload starts and ends.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Obu {
    start: usize,
    header_len: usize,
    payload: usize,
    end: usize,
    kind: u8,
    sized: bool,
}

/// The OBUs of a run; None when a header or a size is broken. An OBU without a size runs to the end.
fn walk(b: &[u8]) -> Option<Vec<Obu>> {
    let (mut v, mut p) = (vec![], 0usize);
    while p < b.len() {
        let h = b[p];
        if h & 0x80 != 0 {
            return None;
        }
        let header_len = 1 + usize::from((h >> 2) & 1);
        let sized = (h >> 1) & 1 == 1;
        if p + header_len > b.len() {
            return None;
        }
        let body = p + header_len;
        let (payload, end) = if sized {
            let (size, n) = read_leb128(&b[body..])?;
            // In u64: on the 32-bit TV the sum could wrap a usize.
            if (body + n) as u64 + size > b.len() as u64 {
                return None;
            }
            (body + n, body + n + size as usize)
        } else {
            (body, b.len())
        };
        v.push(Obu {
            start: p,
            header_len,
            payload,
            end,
            kind: (h >> 3) & 0x0f,
            sized,
        });
        p = end;
    }
    Some(v)
}

/// One OBU with a size field: copied when it has one, given the size of its payload when not.
fn push_sized(b: &[u8], o: &Obu, out: &mut Vec<u8>) {
    if o.sized {
        return out.extend_from_slice(&b[o.start..o.end]);
    }
    out.push(b[o.start] | 0x02);
    if o.header_len == 2 {
        out.push(b[o.start + 1]);
    }
    write_leb128((o.end - o.payload) as u64, out);
    out.extend_from_slice(&b[o.payload..o.end]);
}

/// What goes in front of a keyframe: the sequence headers of the `av1C` record (or of a bare OBU run), then its
/// metadata (HDR10 levels can live only there). Empty without a sequence header.
pub fn config(ed: &[u8]) -> Vec<u8> {
    let obus = match ed {
        [0x81, _, _, _, rest @ ..] => rest,
        [h, ..] if h & 0x80 == 0 => ed,
        _ => return vec![],
    };
    let Some(list) = walk(obus) else { return vec![] };
    if !list.iter().any(|o| o.kind == SEQUENCE_HEADER) {
        return vec![];
    }
    let mut out = vec![];
    for kind in [SEQUENCE_HEADER, METADATA] {
        for o in list.iter().filter(|o| o.kind == kind) {
            push_sized(obus, o, &mut out);
        }
    }
    out
}

/// One demuxed packet as a temporal unit in `out`. A packet that is not OBUs is passed on as it is, never dropped.
pub fn temporal_unit(pkt: &[u8], key: bool, cfg: &[u8], out: &mut Vec<u8>) {
    out.clear();
    let Some(list) = walk(pkt) else {
        return out.extend_from_slice(pkt);
    };
    let mut rest = &list[..];
    match list.first() {
        Some(o) if o.kind == TEMPORAL_DELIMITER => {
            push_sized(pkt, o, out);
            rest = &list[1..];
        }
        _ => out.extend_from_slice(&TD),
    }
    if key && !cfg.is_empty() && !list.iter().any(|o| o.kind == SEQUENCE_HEADER) {
        out.extend_from_slice(cfg);
    }
    if let Some((last, body)) = rest.split_last() {
        for o in body {
            out.extend_from_slice(&pkt[o.start..o.end]);
        }
        push_sized(pkt, last, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keyframe_gets_a_delimiter_and_the_sequence_header_and_every_obu_a_size() {
        // av1C: marker/version, three fixed bytes, then a sequence header (type 1, sized, 2 bytes).
        let seq = [0x0a, 0x02, 0xaa, 0xbb];
        let ed = [&[0x81, 0, 0, 0][..], &seq].concat();
        let cfg = config(&ed);
        assert_eq!(cfg, seq);
        // A frame OBU (type 6) without a size field, as Matroska may store the last one.
        let pkt = [0x30, 1, 2, 3];
        let mut out = vec![];
        temporal_unit(&pkt, true, &cfg, &mut out);
        assert_eq!(out, [&TD[..], &seq, &[0x32, 3, 1, 2, 3]].concat());
        temporal_unit(&pkt, false, &cfg, &mut out);
        assert_eq!(
            out,
            [&TD[..], &[0x32, 3, 1, 2, 3]].concat(),
            "no header on other frames"
        );
        let junk = [0xff, 0x00];
        temporal_unit(&junk, true, &cfg, &mut out);
        assert_eq!(out, junk, "what does not parse goes on as it is");
        assert_eq!(read_leb128(&[0xff; 9]), None);
        assert!(walk(&[0x32, 0x7f, 1]).is_none(), "a size past the end");
    }
}
