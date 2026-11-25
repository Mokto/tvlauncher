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

/// The index of the cue to show at `ms`.
pub fn at(cues: &[Cue], ms: i64) -> Option<usize> {
    let after = cues.partition_point(|c| c.start_ms <= ms);
    // A long cue can still run while a later one has started: look back a few.
    (after.saturating_sub(4)..after).rev().find(|&i| cues[i].end_ms > ms)
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
    fn finds_the_cue_for_a_time() {
        let cues = parse_srt(SRT);
        assert_eq!(at(&cues, 500), None);
        assert_eq!(at(&cues, 1000), Some(0));
        assert_eq!(at(&cues, 3499), Some(0));
        assert_eq!(at(&cues, 3500), None, "a cue ends at its end time");
        assert_eq!(at(&cues, 63_000), Some(1));
        assert_eq!(at(&cues, 999_000), None);
    }

    #[test]
    fn a_long_cue_survives_a_short_one_inside_it() {
        let cues = parse_srt("1\n00:00:01,000 --> 00:00:20,000\nlong\n\n2\n00:00:02,000 --> 00:00:03,000\nshort\n");
        assert_eq!(at(&cues, 10_000), Some(0));
    }
}
