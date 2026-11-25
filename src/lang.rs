//! Language codes as the files and servers write them (ISO 639-2 in its "B" and "T" forms, ISO 639-1, with a region
//! such as `pt-BR`), and the names Settings offers.

/// Name, 639-2/B (the canonical form here), 639-2/T, 639-1.
pub const LANGS: &[(&str, &str, &str, &str)] = &[
    ("English", "eng", "eng", "en"),
    ("French", "fre", "fra", "fr"),
    ("German", "ger", "deu", "de"),
    ("Spanish", "spa", "spa", "es"),
    ("Italian", "ita", "ita", "it"),
    ("Portuguese", "por", "por", "pt"),
    ("Dutch", "dut", "nld", "nl"),
    ("Russian", "rus", "rus", "ru"),
    ("Japanese", "jpn", "jpn", "ja"),
    ("Korean", "kor", "kor", "ko"),
    ("Chinese", "chi", "zho", "zh"),
    ("Arabic", "ara", "ara", "ar"),
    ("Hindi", "hin", "hin", "hi"),
    ("Swedish", "swe", "swe", "sv"),
    ("Danish", "dan", "dan", "da"),
    ("Norwegian", "nor", "nob", "no"),
    ("Finnish", "fin", "fin", "fi"),
    ("Polish", "pol", "pol", "pl"),
    ("Turkish", "tur", "tur", "tr"),
    ("Czech", "cze", "ces", "cs"),
    ("Hungarian", "hun", "hun", "hu"),
    ("Greek", "gre", "ell", "el"),
    ("Hebrew", "heb", "heb", "he"),
    ("Thai", "tha", "tha", "th"),
    ("Ukrainian", "ukr", "ukr", "uk"),
    ("Romanian", "rum", "ron", "ro"),
];

/// The 639-2/B code of `code` when it is a known language, else the code in lower case. "" for none or "und".
pub fn canon(code: &str) -> String {
    let c = code.split(['-', '_']).next().unwrap_or("").trim().to_lowercase();
    if c == "und" || c == "zxx" || c == "mis" {
        return String::new();
    }
    let c = if c == "nb" || c == "nn" { "no".to_string() } else { c };
    match LANGS.iter().find(|l| [l.1, l.2, l.3].contains(&c.as_str())) {
        Some(l) => l.1.to_string(),
        None => c,
    }
}

/// "French" for any form of its code, the code in capitals for a language not listed, "" for none.
pub fn name(code: &str) -> String {
    let c = canon(code);
    match LANGS.iter().find(|l| l.1 == c) {
        Some(l) => l.0.to_string(),
        None => c.to_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_form_of_a_code_is_one_language() {
        for c in ["fre", "fra", "fr", "FR", "fr-CA", "fra_FR"] {
            assert_eq!(canon(c), "fre", "{c}");
        }
        assert_eq!(canon("nb"), "nor");
        assert_eq!(canon("und"), "");
        assert_eq!(canon(""), "");
        assert_eq!(canon("tlh"), "tlh");
        assert_eq!(name("deu"), "German");
        assert_eq!(name("tlh"), "TLH");
        assert_eq!(name("und"), "");
    }
}
