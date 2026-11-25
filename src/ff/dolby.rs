//! What the picture's own metadata needs before LG's HEVC decoder sees it.
//!
//! - **Dolby Vision profile 7** (UHD Blu-ray rips) is two layers: the HDR10 base and an enhancement layer (EL), with an
//!   RPU of dynamic metadata for both. LG's decoder takes one layer, so the EL goes and the RPU is rewritten to profile
//!   8.1, the way `dovi_tool -m 2` does (quietvoid's `dolby_vision` crate, MIT). A minimal enhancement layer (MEL) is
//!   all metadata, so nothing is lost; a full one (FEL) also carries residual detail that is dropped, as in every
//!   single-layer player. Native Jelly does the same (`rewrite_dv_rpu_p7_to_p81`).
//! - **HDR10+** is dynamic metadata in a T.35 SEI message. LG's decoder does not use it and can stall on it (Kodi strips
//!   it for webOS for that reason, and so does Native Jelly), so the message is cut out of every SEI NAL. The static
//!   HDR10 messages in the same NAL stay: the panel's HDR mode comes from those.

/// Whether this TV shows Dolby Vision (`tv.config.supportDolbyHDRContents`): a Dolby Vision picture is declared and
/// fed as such only then (asked once; false when there is no `luna-send`).
pub fn tv_shows_dv() -> bool {
    static DV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DV.get_or_init(|| {
        std::process::Command::new("/usr/bin/luna-send")
            .args([
                "-n",
                "1",
                "-w",
                "3000",
                "luna://com.webos.service.config/getConfigs",
                r#"{"configNames":["tv.config.supportDolbyHDRContents"]}"#,
            ])
            .stdin(std::process::Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(r#""tv.config.supportDolbyHDRContents":true"#))
            .unwrap_or(false)
    })
}

use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

static EL_DROPPED: AtomicU32 = AtomicU32::new(0);
static RPU_CONVERTED: AtomicU32 = AtomicU32::new(0);
static RPU_FAILED: AtomicU32 = AtomicU32::new(0);
static PLUS_CUT: AtomicU32 = AtomicU32::new(0);

/// What the picture feed did to the metadata so far, for the log (None when it touched none).
pub fn summary() -> Option<String> {
    let n = |c: &AtomicU32| c.load(Relaxed);
    let (el, ok, bad, cut) = (n(&EL_DROPPED), n(&RPU_CONVERTED), n(&RPU_FAILED), n(&PLUS_CUT));
    (el + ok + bad + cut > 0).then(|| {
        format!("dolby vision: {el} enhancement-layer NALs dropped, {ok} RPUs converted, {bad} RPUs unconvertible; HDR10+: {cut} messages cut")
    })
}

/// What to do with a profile 7 picture's extra layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Dv7 {
    /// Not profile 7: every NAL passes.
    #[default]
    Off,
    /// The TV does not show Dolby Vision: the EL and the RPU go, the HDR10 base plays.
    Drop,
    /// The EL goes and the RPU becomes profile 8.1.
    Convert,
}

pub const NAL_RPU: u8 = 62;
pub const NAL_EL: u8 = 63;

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| (b >> 1) & 0x3f)
}

/// `nuh_layer_id`: the high bit is bit 0 of byte 0, the other five the top of byte 1. Zero in a single-layer stream.
fn layer_id(nal: &[u8]) -> u8 {
    if nal.len() < 2 {
        return 0;
    }
    ((nal[0] & 1) << 5) | (nal[1] >> 3)
}

/// The enhancement layer: wrapped in NAL 63 (as a single-track dual layer stores it) or in a layer above the base.
pub fn is_el(nal: &[u8]) -> bool {
    let el = nal.len() >= 2 && (nal_type(nal) == NAL_EL || layer_id(nal) > 0);
    if el {
        EL_DROPPED.fetch_add(1, Relaxed);
    }
    el
}

/// An RPU NAL (2-byte header, emulation prevention still in) as profile 8.1. A profile 8 RPU comes back as it was. None
/// when it cannot be parsed or converted: the caller drops it, since a frame with no RPU keeps the last one's metadata,
/// which beats a profile 8 declaration followed by a profile 7 RPU.
pub fn rpu_to_p81(nal: &[u8]) -> Option<Vec<u8>> {
    let mut rpu = dolby_vision::rpu::dovi_rpu::DoviRpu::parse_unspec62_nalu(nal).ok()?;
    let out = if rpu.dovi_profile != 7 {
        (rpu.dovi_profile == 8).then(|| nal.to_vec())
    } else {
        rpu.convert_with_mode(dolby_vision::rpu::ConversionMode::To81)
            .ok()
            .and_then(|_| rpu.write_hevc_unspec62_nalu().ok())
    };
    let c = if out.is_some() { &RPU_CONVERTED } else { &RPU_FAILED };
    c.fetch_add(1, Relaxed);
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum Sei {
    /// Not an SEI, or one without HDR10+: the original bytes.
    Keep,
    /// Every message was HDR10+ (or the list did not parse cleanly): nothing.
    Drop,
    /// The NAL without its HDR10+ messages.
    Rewrite(Vec<u8>),
}

/// Cuts the HDR10+ message (SEI payload type 4, country 0xB5, provider 0x003C) out of a prefix (39) or suffix (40) SEI
/// NAL. `nal` starts at the 2-byte header. A list that does not parse cleanly is dropped whole if an HDR10+ header was
/// seen, kept otherwise.
pub fn strip_hdr10plus(nal: &[u8]) -> Sei {
    if !matches!(nal_type(nal), 39 | 40) || nal.len() < 2 {
        return Sei::Keep;
    }
    let rbsp = unescape(&nal[2..]);
    let mut kept: Vec<(usize, usize)> = vec![];
    let (mut found, mut clean, mut p) = (false, false, 0usize);
    while p < rbsp.len() {
        if rbsp[p] == 0x80 {
            clean = p + 1 == rbsp.len();
            break;
        }
        let start = p;
        let mut read = || {
            let mut v = 0usize;
            while p < rbsp.len() && rbsp[p] == 0xff {
                v += 255;
                p += 1;
            }
            let last = *rbsp.get(p)?;
            p += 1;
            Some(v + last as usize)
        };
        let (Some(ptype), Some(psize)) = (read(), read()) else {
            break;
        };
        let end = p.saturating_add(psize);
        let payload = rbsp.get(p..end.min(rbsp.len())).unwrap_or(&[]);
        let plus = ptype == 4 && payload.starts_with(&[0xb5, 0x00, 0x3c]);
        found |= plus;
        if end > rbsp.len() {
            break;
        }
        if !plus {
            kept.push((start, end));
        }
        p = end;
    }
    if !found {
        return Sei::Keep;
    }
    PLUS_CUT.fetch_add(1, Relaxed);
    if !clean || kept.is_empty() {
        return Sei::Drop;
    }
    let mut out_rbsp: Vec<u8> = kept.iter().flat_map(|&(s, e)| rbsp[s..e].iter().copied()).collect();
    out_rbsp.push(0x80);
    let mut out = nal[..2].to_vec();
    let mut zeros = 0;
    for b in out_rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    Sei::Rewrite(out)
}

/// The payload with its emulation-prevention bytes removed (`00 00 03` reads `00 00`), so sizes count what they describe.
fn unescape(e: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(e.len());
    let mut zeros = 0;
    for &b in e {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A fixture file is a 4-byte start code and then the RPU after its `7C 01` NAL header.
    pub fn nal(f: &[u8]) -> Vec<u8> {
        [&[0x7c, 1][..], &f[4..]].concat()
    }
    const MEL: &[u8] = include_bytes!("dovi-assets/dovi-p7-mel-rpu.bin");
    const FEL: &[u8] = include_bytes!("dovi-assets/dovi-p7-fel-rpu.bin");
    const MEL81: &[u8] = include_bytes!("dovi-assets/dovi-p7-mel-to-81-rpu.bin");

    /// dovi_tool's own test assets (MIT): its MEL RPU comes out as the 8.1 RPU its test suite expects, byte for byte.
    #[test]
    fn a_mel_rpu_becomes_what_dovi_tool_writes() {
        assert_eq!(rpu_to_p81(&nal(MEL)), Some(nal(MEL81)));
    }

    #[test]
    fn a_fel_rpu_reads_back_as_profile_8() {
        let parse = |n: &[u8]| dolby_vision::rpu::dovi_rpu::DoviRpu::parse_unspec62_nalu(n).unwrap();
        assert_eq!(parse(&nal(FEL)).dovi_profile, 7);
        let out = rpu_to_p81(&nal(FEL)).expect("converts");
        assert_eq!(out[..2], [0x7c, 0x01]);
        assert_eq!(parse(&out).dovi_profile, 8);
    }

    #[test]
    fn a_profile_8_rpu_stays_and_junk_goes() {
        assert_eq!(rpu_to_p81(&nal(MEL81)), Some(nal(MEL81)));
        assert_eq!(rpu_to_p81(&[0x7c, 1, 0, 0, 0]), None);
    }

    #[test]
    fn the_enhancement_layer_is_found_by_type_or_layer() {
        assert!(is_el(&[63 << 1, 1, 9]));
        assert!(is_el(&[2, 9, 9]), "nuh_layer_id 1");
        assert!(!is_el(&[2, 1, 9]));
        assert!(!is_el(&[62 << 1, 1, 9]));
    }

    fn sei(messages: &[(u8, &[u8])]) -> Vec<u8> {
        let mut n = vec![39 << 1, 1];
        for &(t, p) in messages {
            n.extend([t, p.len() as u8]);
            n.extend_from_slice(p);
        }
        n.push(0x80);
        n
    }
    const PLUS: &[u8] = &[0xb5, 0x00, 0x3c, 0x00, 0x01, 0x04, 0x01];

    #[test]
    fn hdr10plus_goes_and_static_hdr_stays() {
        assert_eq!(strip_hdr10plus(&sei(&[(4, PLUS)])), Sei::Drop);
        assert_eq!(strip_hdr10plus(&sei(&[(137, &[0; 24]), (144, &[0; 4])])), Sei::Keep);
        assert_eq!(
            strip_hdr10plus(&sei(&[(4, &[0xb5, 0x00, 0x31, 0x44])])),
            Sei::Keep,
            "another provider"
        );
        assert_eq!(
            strip_hdr10plus(&[19 << 1, 1, 0xb5, 0x00, 0x3c]),
            Sei::Keep,
            "not an SEI"
        );
        let m = [0x11u8; 24];
        assert_eq!(
            strip_hdr10plus(&sei(&[(137, &m), (4, PLUS)])),
            Sei::Rewrite(sei(&[(137, &m)]))
        );
        let mut suffix = sei(&[(4, PLUS), (144, &[1, 2, 3, 4])]);
        suffix[0] = 40 << 1;
        let mut want = sei(&[(144, &[1, 2, 3, 4])]);
        want[0] = 40 << 1;
        assert_eq!(strip_hdr10plus(&suffix), Sei::Rewrite(want));
    }

    #[test]
    fn what_stays_gets_its_emulation_prevention_back() {
        let payload = [0, 0, 0, 7, 0, 0, 1, 9, 0, 0, 2, 0, 0, 4];
        let Sei::Rewrite(out) = strip_hdr10plus(&sei(&[(5, &payload), (4, PLUS)])) else {
            panic!("a rewrite");
        };
        let mut want = vec![39 << 1, 1, 5, payload.len() as u8];
        want.extend([0, 0, 3, 0, 7, 0, 0, 3, 1, 9, 0, 0, 3, 2, 0, 0, 4, 0x80]);
        assert_eq!(out, want);
    }

    #[test]
    fn sizes_count_bytes_without_emulation_prevention() {
        // 00 00 03 00 is the payload 00 00 00: reading its size against the escaped bytes would miss the next message.
        let mut n = vec![39 << 1, 1, 5, 3, 0, 0, 3, 0, 4, PLUS.len() as u8];
        n.extend_from_slice(PLUS);
        n.push(0x80);
        let Sei::Rewrite(out) = strip_hdr10plus(&n) else {
            panic!("a rewrite");
        };
        assert_eq!(unescape(&out[2..]), [5, 3, 0, 0, 0, 0x80]);
    }
}
