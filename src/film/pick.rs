//! The audio and subtitle tracks a film starts with, from the viewer's language settings (Settings → Audio) and
//! what the server would start with. The settings live on the TV and apply to Jellyfin and Plex alike.
use crate::lang::canon;

/// The settings: `audio` and `sub_lang` are language codes ("" for none); `sub_mode` is "default" (as the server
/// chooses), "off", "on", "forced" or "smart" (subtitles when the audio is not in the subtitle language).
#[derive(Clone, Default, Debug)]
pub struct Langs {
    pub audio: String,
    pub sub_mode: String,
    pub sub_lang: String,
}

pub struct Cand {
    pub index: i64,
    pub lang: String,
    pub forced: bool,
}

/// The first track in the preferred audio language, else `default`.
pub fn audio(p: &Langs, tracks: &[Cand], default: Option<i64>) -> Option<i64> {
    let want = canon(&p.audio);
    if want.is_empty() {
        return default;
    }
    tracks
        .iter()
        .find(|t| canon(&t.lang) == want)
        .map(|t| t.index)
        .or(default)
}

/// The subtitle track to start with, or none. `audio_lang` is the language of the audio track that starts. With no
/// language to look for (neither a subtitle language nor an audio preference) the server's choice stands.
pub fn sub(p: &Langs, audio_lang: &str, tracks: &[Cand], default: Option<i64>) -> Option<i64> {
    if p.sub_mode == "off" {
        return None;
    }
    let lang = canon(if p.sub_lang.is_empty() { &p.audio } else { &p.sub_lang });
    if lang.is_empty() || !matches!(p.sub_mode.as_str(), "on" | "forced" | "smart") {
        return default;
    }
    let mine = |t: &&Cand| canon(&t.lang) == lang;
    let forced = || tracks.iter().filter(mine).find(|t| t.forced).map(|t| t.index);
    match p.sub_mode.as_str() {
        "forced" => forced(),
        "smart" if canon(audio_lang) == lang => forced(),
        _ => tracks
            .iter()
            .filter(mine)
            .find(|t| !t.forced)
            .map(|t| t.index)
            .or_else(forced),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(index: i64, lang: &str, forced: bool) -> Cand {
        Cand {
            index,
            lang: lang.into(),
            forced,
        }
    }

    fn langs(audio: &str, mode: &str, sub: &str) -> Langs {
        Langs {
            audio: audio.into(),
            sub_mode: mode.into(),
            sub_lang: sub.into(),
        }
    }

    #[test]
    fn audio_follows_the_language_in_any_spelling() {
        let t = [c(1, "eng", false), c(2, "fra", false)];
        assert_eq!(audio(&langs("fre", "", ""), &t, Some(1)), Some(2));
        assert_eq!(audio(&langs("fr", "", ""), &t, Some(1)), Some(2));
        assert_eq!(
            audio(&langs("jpn", "", ""), &t, Some(1)),
            Some(1),
            "none in it: the server's"
        );
        assert_eq!(audio(&langs("", "", ""), &t, Some(1)), Some(1));
    }

    #[test]
    fn subtitle_modes() {
        let t = [c(3, "eng", false), c(4, "fre", true), c(5, "fre", false)];
        let (def, off) = (Some(3), None);
        assert_eq!(sub(&langs("fre", "default", ""), "fre", &t, def), def);
        assert_eq!(sub(&langs("fre", "off", ""), "eng", &t, def), off);
        assert_eq!(
            sub(&langs("fre", "on", ""), "eng", &t, def),
            Some(5),
            "the full track before the forced one"
        );
        assert_eq!(sub(&langs("fre", "forced", ""), "eng", &t, def), Some(4));
        assert_eq!(
            sub(&langs("fre", "forced", "eng"), "fre", &t, def),
            off,
            "no forced English track"
        );
        // smart: none while the audio is in the subtitle language (but its forced track), subtitles otherwise
        assert_eq!(sub(&langs("", "smart", "fre"), "fre", &t, def), Some(4));
        assert_eq!(sub(&langs("", "smart", "fre"), "eng", &t, def), Some(5));
        // no language to look for: the server's choice
        assert_eq!(sub(&langs("", "on", ""), "eng", &t, def), def);
        // the subtitle language wins over the audio one
        assert_eq!(sub(&langs("fre", "on", "eng"), "fre", &t, def), Some(3));
    }
}
