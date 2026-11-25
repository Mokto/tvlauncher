//! SRT subtitles, which Jellyfin converts every text track to on request.

#[derive(Debug, PartialEq)]
pub struct Cue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// `00:01:02,500` (or with a dot) in milliseconds.
fn stamp(s: &str) -> Option<i64> {
    let mut p = s.trim().split([':', ',', '.']);
    let mut n = || p.next()?.trim().parse::<i64>().ok();
    let (h, m, sec, ms) = (n()?, n()?, n()?, n()?);
    Some(((h * 60 + m) * 60 + sec) * 1000 + ms)
}

/// Markup the player cannot draw: `<i>`, `<font ...>` and `{\an8}`.
fn plain(line: &str) -> String {
    let (mut out, mut skip) = (String::new(), None::<char>);
    for c in line.chars() {
        match (skip, c) {
            (Some(end), _) if c == end => skip = None,
            (Some(_), _) => {}
            (None, '<') => skip = Some('>'),
            (None, '{') => skip = Some('}'),
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

/// The cues of an SRT file, in time order; blocks it cannot read are skipped.
pub fn parse_srt(text: &str) -> Vec<Cue> {
    let text = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut cues: Vec<Cue> = text
        .split("\n\n")
        .filter_map(|block| {
            let mut lines = block.lines().skip_while(|l| !l.contains("-->"));
            let (a, b) = lines.next()?.split_once("-->")?;
            let text: Vec<String> = lines.map(plain).filter(|l| !l.is_empty()).collect();
            Some(Cue {
                start_ms: stamp(a)?,
                end_ms: stamp(b.split_whitespace().next()?)?,
                text: text.join("\n"),
            })
        })
        .filter(|c| !c.text.is_empty())
        .collect();
    cues.sort_by_key(|c| c.start_ms);
    cues
}

/// `0:01:02.50` in milliseconds.
fn ass_stamp(s: &str) -> Option<i64> {
    let mut p = s.trim().split([':', '.']);
    let mut n = || p.next()?.trim().parse::<i64>().ok();
    let (h, m, sec, cs) = (n()?, n()?, n()?, n()?);
    Some(((h * 60 + m) * 60 + sec) * 1000 + cs * 10)
}

/// The dialogue of an ASS or SSA file as plain cues: the `Format:` line of `[Events]` says where start, end and text
/// are; override blocks and line breaks are cleaned as in `ff::demux::plain_text`.
pub fn parse_ass(text: &str) -> Vec<Cue> {
    let mut cols: Vec<String> = vec![];
    let mut cues: Vec<Cue> = vec![];
    for line in text.lines() {
        let line = line.trim();
        if let Some(f) = line.strip_prefix("Format:") {
            cols = f.split(',').map(|c| c.trim().to_ascii_lowercase()).collect();
        } else if let Some(d) = line.strip_prefix("Dialogue:") {
            let pos = |n: &str| cols.iter().position(|c| c == n);
            let (Some(s), Some(e), Some(t)) = (pos("start"), pos("end"), pos("text")) else {
                continue;
            };
            let f: Vec<&str> = d.trim_start().splitn(cols.len(), ',').collect();
            let (Some(a), Some(b), Some(body)) = (f.get(s), f.get(e), f.get(t)) else {
                continue;
            };
            let (text, _) = crate::ff::demux::plain_text(body, false);
            if let (Some(start_ms), Some(end_ms), false) = (ass_stamp(a), ass_stamp(b), text.is_empty()) {
                cues.push(Cue { start_ms, end_ms, text });
            }
        }
    }
    cues.sort_by_key(|c| c.start_ms);
    cues
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRT: &str = "\u{feff}1\r\n00:00:01,000 --> 00:00:03,500\r\n<i>Bonjour</i>\r\nle monde\r\n\r\n\
                       2\r\n00:01:02.250 --> 00:01:04,000 X1:100\r\n{\\an8}Haut\r\n\r\n\
                       3\r\nnot a time\r\nlost\r\n\r\n4\r\n00:00:10,000 --> 00:00:11,000\r\n\r\n";

    #[test]
    fn reads_cues_and_drops_markup_and_junk() {
        let cues = parse_srt(SRT);
        assert_eq!(cues.len(), 2, "the junk block and the empty cue are dropped");
        assert_eq!(
            cues[0],
            Cue {
                start_ms: 1000,
                end_ms: 3500,
                text: "Bonjour\nle monde".into()
            }
        );
        assert_eq!(
            (cues[1].start_ms, cues[1].end_ms, cues[1].text.as_str()),
            (62_250, 64_000, "Haut")
        );
    }

    #[test]
    fn ass_dialogue_becomes_cues() {
        let ass = "[Script Info]\nTitle: x\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
                   Dialogue: 0,0:00:03.00,0:00:06.50,Default,,0,0,0,,{\\b1}Styled{\\b0}, text\\Nsecond\n\
                   Comment: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,ignored\n";
        let cues = parse_ass(ass);
        assert_eq!(
            cues,
            [Cue {
                start_ms: 3000,
                end_ms: 6500,
                text: "Styled, text\nsecond".into()
            }]
        );
    }
}
