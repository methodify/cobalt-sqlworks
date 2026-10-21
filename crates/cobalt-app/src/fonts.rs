//! Glyph coverage beyond Latin: egui's bundled fonts (Ubuntu, Hack, Noto Emoji) have no CJK,
//! Arabic, Hebrew, Indic, Thai… so those characters drew as tofu boxes. Instead of bundling
//! tens of megabytes of fonts, the app borrows the system's: the text-painting sites (grid
//! cells, the editor, tree rows, messages, typed input) call [`note`] with what they draw, a
//! byte scan spots the scripts in it, and the first time a script shows up its font pack is
//! read from the OS font folder on a worker thread and appended to both egui families as a
//! fallback (`Context::add_font`), so the next frame draws it. Nothing loads for ASCII-only
//! sessions; a Japanese dataset costs one ~10 MB font once.
//!
//! Han (kanji / hanzi) is unified across Japanese, Simplified and Traditional Chinese, and the
//! fonts differ in glyph style: kana or halfwidth katakana in the text picks the Japanese
//! pack, bopomofo the Traditional pack, otherwise the user's locale decides (ja → Japanese,
//! zh-TW / zh-HK / Hant → Traditional, else Simplified).

use egui::epaint::text::{FontData, FontInsert, FontPriority, InsertFontFamily};
use egui::{Context, FontFamily};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::OnceLock;

/// A set of scripts one system font covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pack {
    Japanese,
    ChineseSimplified,
    ChineseTraditional,
    Korean,
    /// Arabic, Hebrew, Syriac, Thaana, Armenian, Georgian… (Segoe UI on Windows).
    MiddleEast,
    /// Devanagari … Sinhala.
    Indic,
    /// Thai, Lao, Khmer.
    Thai,
    Ethiopic,
    Myanmar,
    Tibetan,
    Mongolian,
    /// Cherokee, Canadian syllabics.
    Cherokee,
    /// Arrows, math, box drawing and the like beyond what egui bundles.
    Symbols,
}

const ALL: [Pack; 13] = [Pack::Japanese, Pack::ChineseSimplified, Pack::ChineseTraditional, Pack::Korean, Pack::MiddleEast, Pack::Indic, Pack::Thai, Pack::Ethiopic, Pack::Myanmar, Pack::Tibetan, Pack::Mongolian, Pack::Cherokee, Pack::Symbols];

impl Pack {
    pub fn name(self) -> &'static str {
        match self {
            Pack::Japanese => "Japanese",
            Pack::ChineseSimplified => "Chinese (Simplified)",
            Pack::ChineseTraditional => "Chinese (Traditional)",
            Pack::Korean => "Korean",
            Pack::MiddleEast => "Arabic, Hebrew and neighbours",
            Pack::Indic => "Indic scripts",
            Pack::Thai => "Thai, Lao, Khmer",
            Pack::Ethiopic => "Ethiopic",
            Pack::Myanmar => "Myanmar",
            Pack::Tibetan => "Tibetan",
            Pack::Mongolian => "Mongolian",
            Pack::Cherokee => "Cherokee and Canadian syllabics",
            Pack::Symbols => "Symbols",
        }
    }
    fn bit(self) -> u32 {
        1 << (ALL.iter().position(|p| *p == self).unwrap_or(0) as u32)
    }
}

/// What a byte scan found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scripts {
    packs: u32,
    /// Han ideographs without a decided pack (kana / bopomofo decide it, else the locale).
    han: bool,
}

impl Scripts {
    pub fn is_empty(&self) -> bool {
        self.packs == 0 && !self.han
    }
    fn add(&mut self, p: Pack) {
        self.packs |= p.bit();
    }
    fn has(&self, p: Pack) -> bool {
        self.packs & p.bit() != 0
    }
    /// The packs to load, Han resolved.
    pub fn packs(&self, locale: &str) -> Vec<Pack> {
        let mut out: Vec<Pack> = ALL.iter().copied().filter(|p| self.has(*p)).collect();
        if self.han && !self.has(Pack::Japanese) && !self.has(Pack::ChineseTraditional) && !self.has(Pack::ChineseSimplified) {
            out.push(han_pack(locale));
        }
        out.sort();
        out
    }
}

/// Which CJK pack Han-only text takes, from the locale (`ja-JP`, `zh-TW`, `zh-Hant-HK`…).
pub fn han_pack(locale: &str) -> Pack {
    let l = locale.to_ascii_lowercase().replace('_', "-");
    if l.starts_with("ja") {
        Pack::Japanese
    } else if l.starts_with("zh") && (l.contains("tw") || l.contains("hk") || l.contains("mo") || l.contains("hant")) {
        Pack::ChineseTraditional
    } else if l.starts_with("ko") {
        Pack::Korean
    } else {
        Pack::ChineseSimplified
    }
}

/// The scripts in a UTF-8 text, from the lead bytes (no decoding): a few nanoseconds per byte.
pub fn scan(bytes: &[u8]) -> Scripts {
    let mut s = Scripts::default();
    let mut i = 0;
    let n = bytes.len();
    while i < n {
        let b = bytes[i];
        if b < 0x80 {
            i += 1;
            continue;
        }
        let b1 = bytes.get(i + 1).copied().unwrap_or(0);
        let b2 = bytes.get(i + 2).copied().unwrap_or(0);
        match b {
            0xC0..=0xDF => {
                // U+0080–U+07FF: Latin ext, Greek, Cyrillic (bundled); Hebrew D6 90.., Arabic D8..DF
                if b >= 0xD6 {
                    s.add(Pack::MiddleEast);
                }
                i += 2;
            }
            0xE0 => {
                // U+0800–U+0FFF
                match b1 {
                    0xA0..=0xA3 => s.add(Pack::MiddleEast), // Samaritan, Mandaic, Arabic ext
                    0xA4..=0xB7 => s.add(Pack::Indic),      // U+0900–U+0DFF
                    0xB8..=0xBB => s.add(Pack::Thai),       // Thai, Lao
                    0xBC..=0xBF => s.add(Pack::Tibetan),
                    _ => {}
                }
                i += 3;
            }
            0xE1 => {
                // U+1000–U+1FFF
                match b1 {
                    0x80..=0x82 => s.add(Pack::Myanmar),
                    0x83 => s.add(Pack::MiddleEast), // Georgian
                    0x84..=0x87 => s.add(Pack::Korean), // Hangul Jamo
                    0x88..=0x8D => s.add(Pack::Ethiopic),
                    0x8E..=0x99 => s.add(Pack::Cherokee), // Cherokee, UCAS, Ogham, Runic
                    0x9E..=0x9F => s.add(Pack::Thai),     // Khmer
                    0xA0..=0xA1 => s.add(Pack::Mongolian),
                    _ => {}
                }
                i += 3;
            }
            0xE2 => {
                // U+2000–U+2FFF: punctuation and symbols; CJK radicals from U+2E80
                match b1 {
                    0x86..=0xAF => s.add(Pack::Symbols), // arrows, math, technical, box drawing, shapes (U+2180–U+2BFF)
                    0xBA..=0xBF => s.han = true,          // CJK radicals, Kangxi, ideographic description
                    _ => {}
                }
                i += 3;
            }
            0xE3 => {
                // U+3000–U+3FFF
                match b1 {
                    0x80 => s.han = true,                 // CJK symbols and punctuation
                    0x81..=0x83 => s.add(Pack::Japanese), // Hiragana, Katakana
                    0x84 => {
                        if b2 < 0xB0 { s.add(Pack::ChineseTraditional) } else { s.add(Pack::Korean) } // Bopomofo | Hangul compat jamo
                    }
                    0x85..=0x86 => s.add(Pack::Korean),
                    0x87 => s.add(Pack::ChineseTraditional), // Bopomofo ext (U+31A0)
                    _ => s.han = true,                         // Kanbun, CJK strokes, Katakana ext, enclosed, compat, ext A
                }
                i += 3;
            }
            0xE4..=0xE9 => {
                s.han = true; // U+4000–U+9FFF
                i += 3;
            }
            0xEA => {
                if b1 >= 0xB0 {
                    s.add(Pack::Korean); // Hangul syllables from U+AC00
                }
                i += 3;
            }
            0xEB..=0xED => {
                if b != 0xED || b1 < 0xA0 {
                    s.add(Pack::Korean);
                }
                i += 3;
            }
            0xEE => i += 3, // private use
            0xEF => {
                // U+F000–U+FFFF
                match b1 {
                    0xA4..=0xAB => s.han = true,          // CJK compatibility ideographs
                    0xAC => s.add(Pack::MiddleEast),      // Alphabetic / Hebrew presentation forms
                    0xAD..=0xB7 => s.add(Pack::MiddleEast), // Arabic presentation forms A
                    0xB9..=0xBB => s.add(Pack::MiddleEast), // Arabic presentation forms B
                    0xBC => s.han = true, // fullwidth Latin / punctuation
                    0xBD => {
                        if b2 >= 0xA1 { s.add(Pack::Japanese) } else { s.han = true } // halfwidth katakana from U+FF61
                    }
                    0xBE => {
                        if b2 <= 0x9F { s.add(Pack::Japanese) } else { s.add(Pack::Korean) } // … to U+FF9F, then halfwidth hangul
                    }
                    _ => {}
                }
                i += 3;
            }
            0xF0 => {
                if (0xA0..=0xAF).contains(&b1) {
                    s.han = true; // CJK ext B–F (U+20000–U+2FFFF)
                }
                i += 4;
            }
            _ => i += 1,
        }
    }
    s
}

/// What is loaded, loading, or missing on this machine.
#[derive(Default)]
struct Loader {
    requested: u32,
    locale: Option<String>,
}

static LOADER: OnceLock<Mutex<Loader>> = OnceLock::new();

fn loader() -> &'static Mutex<Loader> {
    LOADER.get_or_init(|| Mutex::new(Loader::default()))
}

/// Note a text about to be drawn: the scripts it uses get their fonts (once).
pub fn note(ctx: &Context, text: &str) {
    note_bytes(ctx, text.as_bytes());
}

pub fn note_bytes(ctx: &Context, bytes: &[u8]) {
    // ASCII is the common case: a tight scan before taking the lock
    if bytes.is_ascii() {
        return;
    }
    let s = scan(bytes);
    if s.is_empty() {
        return;
    }
    let mut l = loader().lock();
    let locale = l.locale.get_or_insert_with(user_locale).clone();
    for p in s.packs(&locale) {
        if l.requested & p.bit() != 0 {
            continue;
        }
        l.requested |= p.bit();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name(format!("font-{p:?}"))
            .spawn(move || load_pack(&ctx, p))
            .ok();
    }
}

/// Read the pack's font and hand it to egui as the last fallback of both families.
fn load_pack(ctx: &Context, pack: Pack) {
    let Some((path, index)) = find(pack) else {
        tracing::warn!(?pack, "no system font found for this script; it stays as boxes");
        return;
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(?pack, path = %path.display(), "could not read the font: {e}");
            return;
        }
    };
    tracing::info!(?pack, path = %path.display(), index, size = bytes.len(), "system font loaded for glyph fallback");
    let data = FontData { font: Cow::Owned(bytes), index, tweak: Default::default() };
    ctx.add_font(FontInsert {
        name: format!("system-{pack:?}"),
        data,
        families: vec![InsertFontFamily { family: FontFamily::Proportional, priority: FontPriority::Lowest }, InsertFontFamily { family: FontFamily::Monospace, priority: FontPriority::Lowest }],
    });
    ctx.request_repaint();
}

/// The font file (and face index in a collection) for a pack on this OS.
fn find(pack: Pack) -> Option<(PathBuf, u32)> {
    for (p, index) in candidates(pack) {
        if p.is_file() {
            return Some((p, index));
        }
    }
    #[cfg(target_os = "linux")]
    {
        return fc_match(pack);
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(target_os = "windows")]
fn candidates(pack: Pack) -> Vec<(PathBuf, u32)> {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let mut dirs = vec![PathBuf::from(windir).join("Fonts")];
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Microsoft").join("Windows").join("Fonts"));
    }
    let files: &[(&str, u32)] = match pack {
        Pack::Japanese => &[("YuGothR.ttc", 0), ("meiryo.ttc", 0), ("msgothic.ttc", 0), ("NotoSansJP-VF.ttf", 0)],
        Pack::ChineseSimplified => &[("msyh.ttc", 0), ("simsun.ttc", 0), ("NotoSansSC-VF.ttf", 0)],
        Pack::ChineseTraditional => &[("msjh.ttc", 0), ("mingliu.ttc", 0), ("NotoSansTC-VF.ttf", 0)],
        Pack::Korean => &[("malgun.ttf", 0), ("NotoSansKR-VF.ttf", 0), ("gulim.ttc", 0)],
        Pack::MiddleEast => &[("segoeui.ttf", 0), ("tahoma.ttf", 0), ("arial.ttf", 0)],
        Pack::Indic => &[("Nirmala.ttc", 0), ("Nirmala.ttf", 0), ("NirmalaUI.ttf", 0)],
        Pack::Thai => &[("LeelawUI.ttf", 0), ("leelawad.ttf", 0), ("tahoma.ttf", 0)],
        Pack::Ethiopic => &[("ebrima.ttf", 0)],
        Pack::Myanmar => &[("mmrtext.ttf", 0)],
        Pack::Tibetan => &[("himalaya.ttf", 0)],
        Pack::Mongolian => &[("monbaiti.ttf", 0)],
        Pack::Cherokee => &[("gadugi.ttf", 0)],
        Pack::Symbols => &[("seguisym.ttf", 0), ("arial.ttf", 0)],
    };
    dirs.iter().flat_map(|d| files.iter().map(move |(f, i)| (d.join(f), *i))).collect()
}

#[cfg(target_os = "macos")]
fn candidates(pack: Pack) -> Vec<(PathBuf, u32)> {
    let files: &[(&str, u32)] = match pack {
        Pack::Japanese => &[("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", 0), ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0), ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0)],
        Pack::ChineseSimplified => &[("/System/Library/Fonts/PingFang.ttc", 0), ("/System/Library/Fonts/STHeiti Light.ttc", 0), ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0)],
        Pack::ChineseTraditional => &[("/System/Library/Fonts/PingFang.ttc", 0), ("/System/Library/Fonts/STHeiti Light.ttc", 0), ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0)],
        Pack::Korean => &[("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0), ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0)],
        Pack::MiddleEast => &[("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0), ("/System/Library/Fonts/GeezaPro.ttc", 0)],
        Pack::Thai => &[("/System/Library/Fonts/Supplemental/Thonburi.ttc", 0), ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0)],
        _ => &[("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0)],
    };
    files.iter().map(|(f, i)| (PathBuf::from(f), *i)).collect()
}

#[cfg(target_os = "linux")]
fn candidates(pack: Pack) -> Vec<(PathBuf, u32)> {
    // the Noto CJK collection (fonts-noto-cjk, google-noto-cjk-fonts): JP 0, KR 1, SC 2, TC 3
    let cjk = ["/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc"];
    let idx = match pack {
        Pack::Japanese => Some(0),
        Pack::Korean => Some(1),
        Pack::ChineseSimplified => Some(2),
        Pack::ChineseTraditional => Some(3),
        _ => None,
    };
    let mut out = Vec::new();
    if let Some(i) = idx {
        out.extend(cjk.iter().map(|p| (PathBuf::from(p), i)));
        out.push((PathBuf::from("/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf"), 0));
        out.push((PathBuf::from("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc"), 0));
    }
    out
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn candidates(_pack: Pack) -> Vec<(PathBuf, u32)> {
    Vec::new()
}

/// fontconfig's best match for the pack's language (`fc-match -f '%{file}|%{index}' :lang=ja`).
#[cfg(target_os = "linux")]
fn fc_match(pack: Pack) -> Option<(PathBuf, u32)> {
    let lang = match pack {
        Pack::Japanese => "ja",
        Pack::ChineseSimplified => "zh-cn",
        Pack::ChineseTraditional => "zh-tw",
        Pack::Korean => "ko",
        Pack::MiddleEast => "ar",
        Pack::Indic => "hi",
        Pack::Thai => "th",
        Pack::Ethiopic => "am",
        Pack::Myanmar => "my",
        Pack::Tibetan => "bo",
        Pack::Mongolian => "mn",
        Pack::Cherokee => "chr",
        Pack::Symbols => "und-zsym",
    };
    let out = std::process::Command::new("fc-match").arg("-f").arg("%{file}|%{index}").arg(format!(":lang={lang}")).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let (file, index) = s.trim().split_once('|')?;
    let p = PathBuf::from(file);
    if !p.is_file() {
        return None;
    }
    Some((p, index.parse().unwrap_or(0)))
}

/// The user's UI locale (`en-US`, `ja-JP`…), for the Han decision.
fn user_locale() -> String {
    #[cfg(target_os = "windows")]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetUserDefaultLocaleName(name: *mut u16, len: i32) -> i32;
        }
        let mut buf = [0u16; 85];
        // SAFETY: a kernel32 export with the documented signature; the buffer is LOCALE_NAME_MAX_LENGTH wide
        let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        if n > 1 {
            return String::from_utf16_lossy(&buf[..(n as usize - 1)]);
        }
    }
    for k in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return v.split('.').next().unwrap_or(&v).to_string();
            }
        }
    }
    "en-US".into()
}

/// For the agent: which packs have been requested so far.
pub fn requested() -> Vec<&'static str> {
    let l = loader().lock();
    ALL.iter().filter(|p| l.requested & p.bit() != 0).map(|p| p.name()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_needs_nothing() {
        assert!(scan(b"SELECT * FROM t").is_empty());
        assert!(scan("café – naïve — résumé".as_bytes()).is_empty());
        assert!(scan("Привет Ελλάδα".as_bytes()).is_empty());
    }

    #[test]
    fn japanese_with_kana() {
        let s = scan("田中 タロウ ひらがな".as_bytes());
        assert!(s.has(Pack::Japanese) && s.han);
        assert_eq!(s.packs("en-US"), vec![Pack::Japanese]);
        assert!(scan("ｶﾀｶﾅ".as_bytes()).has(Pack::Japanese));
    }

    #[test]
    fn han_only_follows_the_locale() {
        let s = scan("北京 上海".as_bytes());
        assert!(s.han && !s.has(Pack::Japanese));
        assert_eq!(s.packs("en-US"), vec![Pack::ChineseSimplified]);
        assert_eq!(s.packs("ja-JP"), vec![Pack::Japanese]);
        assert_eq!(s.packs("zh-TW"), vec![Pack::ChineseTraditional]);
        assert_eq!(s.packs("zh-Hant-HK"), vec![Pack::ChineseTraditional]);
        assert_eq!(s.packs("zh-CN"), vec![Pack::ChineseSimplified]);
    }

    #[test]
    fn other_scripts() {
        assert_eq!(scan("한국어".as_bytes()).packs("en-US"), vec![Pack::Korean]);
        assert_eq!(scan("ㄅㄆㄇ 台灣".as_bytes()).packs("en-US"), vec![Pack::ChineseTraditional]);
        assert_eq!(scan("مرحبا".as_bytes()).packs("en-US"), vec![Pack::MiddleEast]);
        assert_eq!(scan("שלום".as_bytes()).packs("en-US"), vec![Pack::MiddleEast]);
        assert_eq!(scan("नमस्ते".as_bytes()).packs("en-US"), vec![Pack::Indic]);
        assert_eq!(scan("สวัสดี".as_bytes()).packs("en-US"), vec![Pack::Thai]);
        assert_eq!(scan("ሰላም".as_bytes()).packs("en-US"), vec![Pack::Ethiopic]);
        assert_eq!(scan("→ ∑ ▒".as_bytes()).packs("en-US"), vec![Pack::Symbols]);
        assert_eq!(scan("😀".as_bytes()).packs("en-US"), vec![]); // egui bundles Noto Emoji
    }

    #[test]
    fn mixed_text_lists_every_pack_once() {
        let s = scan("東京 Tokyo 서울 القاهرة".as_bytes());
        assert_eq!(s.packs("en-US"), vec![Pack::ChineseSimplified, Pack::Korean, Pack::MiddleEast]);
    }
}
