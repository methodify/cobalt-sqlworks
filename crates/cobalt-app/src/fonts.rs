//! Glyph coverage beyond Latin. egui's bundled fonts (Ubuntu, Hack, Noto Emoji) have no CJK,
//! Arabic, Hebrew, Indic, Thai, mathematical alphanumerics… so those drew as tofu boxes.
//! Three layers fix that, cheapest first:
//!
//! 1. **System fonts on demand.** The text-painting sites (grid cells, the editor, tree rows,
//!    messages, typed input) call [`note`] with what they draw; a byte scan spots the scripts,
//!    and the first time a script shows up its font pack is read from the OS font folder on a
//!    worker thread and appended to both egui families as a fallback (`Context::add_font`).
//! 2. **Downloaded packs.** A registry (`assets/fonts.json`, overridden by the copy at
//!    cobaltsql.org/fonts.json when it can be fetched) lists Noto packs the user can download
//!    from Settings › Fonts into the app data folder; installed packs load at startup ahead of
//!    any system font. When a script has no system font, the app suggests the pack that covers
//!    it (once per session, switchable off).
//! 3. **Chosen fonts.** Settings › Fonts lets the user put a downloaded or any local font file
//!    first in the UI family or the editor / grid family.
//!
//! Han (kanji / hanzi) is unified across Japanese, Simplified and Traditional Chinese, and the
//! fonts differ in glyph style: kana or halfwidth katakana in the text picks the Japanese
//! pack, bopomofo the Traditional pack, hangul Korean, otherwise the user's locale decides.

use egui::epaint::text::{FontData, FontInsert, FontPriority, InsertFontFamily};
use egui::{Context, FontDefinitions, FontFamily};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// A set of scripts one font covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
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
    /// Arrows, math, mathematical alphanumerics, dingbats, box drawing, emoji beyond Noto Emoji.
    Symbols,
}

pub const ALL: [Pack; 13] = [Pack::Japanese, Pack::ChineseSimplified, Pack::ChineseTraditional, Pack::Korean, Pack::MiddleEast, Pack::Indic, Pack::Thai, Pack::Ethiopic, Pack::Myanmar, Pack::Tibetan, Pack::Mongolian, Pack::Cherokee, Pack::Symbols];

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
            Pack::Symbols => "Symbols and mathematical letters",
        }
    }
    fn bit(self) -> u32 {
        1 << (ALL.iter().position(|p| *p == self).unwrap_or(0) as u32)
    }
    /// The registry's spelling (`"Japanese"`, `"ChineseSimplified"`…).
    pub fn parse(s: &str) -> Option<Pack> {
        ALL.iter().copied().find(|p| format!("{p:?}") == s)
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
                    0x86..=0xAF => s.add(Pack::Symbols), // arrows, math, technical, box drawing, shapes, dingbats (U+2180–U+2BFF)
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
                    0xA4..=0xAB => s.han = true,            // CJK compatibility ideographs
                    0xAC..=0xB7 => s.add(Pack::MiddleEast), // alphabetic / Hebrew / Arabic presentation forms A
                    0xB9..=0xBB => s.add(Pack::MiddleEast), // Arabic presentation forms B
                    0xBC => s.han = true,                   // fullwidth Latin / punctuation
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
                match b1 {
                    0x9D if (0x90..=0x9F).contains(&b2) => s.add(Pack::Symbols), // mathematical alphanumerics U+1D400–U+1D7FF
                    0x9F => s.add(Pack::Symbols),                               // emoji, modifiers and symbols beyond Noto Emoji (U+1F000–U+1FFFF)
                    0xA0..=0xAF => s.han = true,                                // CJK ext B–F (U+20000–U+2FFFF)
                    _ => {}
                }
                i += 4;
            }
            _ => i += 1,
        }
    }
    s
}

// ---------------------------------------------------------------------------------------------
// The registry of downloadable packs
// ---------------------------------------------------------------------------------------------

pub const BUNDLED_REGISTRY: &str = include_str!("../../../assets/fonts.json");
pub const REGISTRY_URL: &str = "https://cobaltsql.org/fonts.json";

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Registry {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

/// A downloadable pack: a few files from one source, covering some scripts, or an alternative
/// UI / editor font.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `fallback` (script coverage), `ui` (a UI font choice), `mono` (an editor / grid choice).
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub packs: Vec<String>,
    #[serde(default)]
    pub license: String,
    pub files: Vec<FileSpec>,
}

fn default_kind() -> String {
    "fallback".into()
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FileSpec {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub size: u64,
    /// For a collection: the faces to load, `[pack name, face index]`.
    #[serde(default)]
    pub faces: Vec<(String, u32)>,
}

impl Entry {
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
    pub fn pack_set(&self) -> Vec<Pack> {
        self.packs.iter().filter_map(|s| Pack::parse(s)).collect()
    }
    pub fn covers(&self, p: Pack) -> bool {
        self.pack_set().contains(&p)
    }
    pub fn is_installed(&self, dir: &Path) -> bool {
        !self.files.is_empty() && self.files.iter().all(|f| dir.join(&f.name).is_file())
    }
    /// The font files of an installed entry, with the face index each pack wants, ordered so
    /// the locale's Han face comes first.
    pub fn installed_faces(&self, dir: &Path, locale: &str) -> Vec<(PathBuf, u32)> {
        let preferred = han_pack(locale);
        let mut out = Vec::new();
        for f in &self.files {
            let p = dir.join(&f.name);
            if f.faces.is_empty() {
                out.push((p, 0));
            } else {
                let mut faces: Vec<(Pack, u32)> = f.faces.iter().filter_map(|(n, i)| Pack::parse(n).map(|p| (p, *i))).collect();
                faces.sort_by_key(|(p, _)| if *p == preferred { 0 } else { 1 });
                for (_, i) in faces {
                    out.push((p.clone(), i));
                }
            }
        }
        out
    }
}

static REMOTE: OnceLock<Mutex<Option<Registry>>> = OnceLock::new();

/// The registry in force: the fetched copy when there is one, else the bundled one.
pub fn registry() -> Registry {
    if let Some(r) = REMOTE.get_or_init(|| Mutex::new(None)).lock().clone() {
        return r;
    }
    serde_json::from_str(BUNDLED_REGISTRY).unwrap_or_default()
}

pub fn remote_fetched() -> bool {
    REMOTE.get_or_init(|| Mutex::new(None)).lock().is_some()
}

/// Fetch the hosted registry (a newer list of packs); on success it replaces the bundled one.
pub fn fetch_registry(session: &crate::session::SessionManager, egui: Context, done: Arc<Mutex<Option<Result<usize, String>>>>) {
    session.spawn(async move {
        let result = async {
            let client = reqwest::Client::builder().user_agent(format!("cobalt-sqlworks/{}", crate::update::CURRENT_VERSION)).timeout(std::time::Duration::from_secs(15)).build().map_err(|e| e.to_string())?;
            let text = client.get(REGISTRY_URL).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?;
            let reg: Registry = serde_json::from_str(&text).map_err(|e| format!("the hosted list is not valid: {e}"))?;
            let n = reg.entries.len();
            *REMOTE.get_or_init(|| Mutex::new(None)).lock() = Some(reg);
            Ok(n)
        }
        .await;
        *done.lock() = Some(result);
        egui.request_repaint();
    });
}

/// Where downloaded packs live.
pub fn fonts_dir(paths: &cobalt_store::AppPaths) -> PathBuf {
    paths.data_dir.join("fonts")
}

/// Download every file of an entry into `dir` (streamed to `.part`, then renamed), reporting
/// through `progress`; `finished` is `Ok(dir)` when all files are in place.
pub fn download(session: &crate::session::SessionManager, egui: Context, entry: Entry, dir: PathBuf, progress: Arc<Mutex<crate::update::DownloadProgress>>) {
    session.spawn(async move {
        let result = download_entry(&entry, &dir, &progress, &egui).await;
        progress.lock().finished = Some(result);
        egui.request_repaint();
    });
}

async fn download_entry(entry: &Entry, dir: &Path, progress: &Arc<Mutex<crate::update::DownloadProgress>>, egui: &Context) -> Result<PathBuf, String> {
    use std::io::Write;
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let client = reqwest::Client::builder().user_agent(format!("cobalt-sqlworks/{}", crate::update::CURRENT_VERSION)).timeout(std::time::Duration::from_secs(60 * 30)).build().map_err(|e| e.to_string())?;
    let total: u64 = entry.total_size();
    let mut done_all: u64 = 0;
    {
        let mut p = progress.lock();
        p.total = total;
        p.done = 0;
    }
    for f in &entry.files {
        let final_path = dir.join(&f.name);
        if final_path.is_file() {
            done_all += f.size;
            progress.lock().done = done_all;
            continue;
        }
        progress.lock().stage = format!("Downloading {}", f.name);
        let part = dir.join(format!("{}.part", f.name));
        let mut resp = client.get(&f.url).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("{}: the server answered {}", f.name, resp.status()));
        }
        let mut file = std::fs::File::create(&part).map_err(|e| format!("could not write {}: {e}", part.display()))?;
        let mut last_paint = std::time::Instant::now();
        while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
            file.write_all(&chunk).map_err(|e| e.to_string())?;
            done_all += chunk.len() as u64;
            progress.lock().done = done_all;
            if last_paint.elapsed() > std::time::Duration::from_millis(150) {
                egui.request_repaint();
                last_paint = std::time::Instant::now();
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        drop(file);
        // a sanity check that it is a font and not an error page
        let head = std::fs::read(&part).map(|b| b.iter().take(4).copied().collect::<Vec<u8>>()).unwrap_or_default();
        let is_font = matches!(head.as_slice(), [0, 1, 0, 0] | b"OTTO" | b"ttcf" | b"true" | b"wOFF");
        if !is_font {
            let _ = std::fs::remove_file(&part);
            return Err(format!("{} is not a font file (the download may have been an error page)", f.name));
        }
        std::fs::rename(&part, &final_path).map_err(|e| e.to_string())?;
    }
    progress.lock().stage = "Installed".into();
    Ok(dir.to_path_buf())
}

/// Delete an entry's files.
pub fn remove(entry: &Entry, dir: &Path) -> Result<(), String> {
    for f in &entry.files {
        let p = dir.join(&f.name);
        if p.is_file() {
            std::fs::remove_file(&p).map_err(|e| format!("could not delete {}: {e}", p.display()))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The font definitions: bundled + chosen + installed + system fonts loaded so far
// ---------------------------------------------------------------------------------------------

/// A font loaded this session (kept so a rebuild of the definitions keeps it).
#[derive(Clone)]
struct Loaded {
    name: String,
    data: Arc<FontData>,
}

#[derive(Default)]
struct Loader {
    /// Packs a system font was asked for (loaded, loading, or found missing).
    requested: u32,
    /// Packs an installed download covers (no system font is sought for them).
    installed: u32,
    locale: Option<String>,
    /// System fonts loaded on demand, in load order.
    system: Vec<Loaded>,
    /// Packs no system font was found for, not yet reported.
    missing: Vec<Pack>,
}

static LOADER: OnceLock<Mutex<Loader>> = OnceLock::new();

fn loader() -> &'static Mutex<Loader> {
    LOADER.get_or_init(|| Mutex::new(Loader::default()))
}

/// The user's UI locale (`en-US`, `ja-JP`…), for the Han decision.
pub fn locale() -> String {
    let mut l = loader().lock();
    l.locale.get_or_insert_with(user_locale).clone()
}

/// Build the full font set: egui's bundled fonts and the icon font, the chosen UI and editor
/// fonts first in their families, the installed packs, then the system fonts loaded so far.
pub fn definitions(settings: &cobalt_core::Settings, paths: &cobalt_store::AppPaths) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    let dir = fonts_dir(paths);
    let reg = registry();
    let loc = locale();
    // the chosen fonts go first
    for (choice, family) in [(&settings.fonts.ui_font, FontFamily::Proportional), (&settings.fonts.mono_font, FontFamily::Monospace)] {
        if choice.trim().is_empty() {
            continue;
        }
        let faces: Vec<(PathBuf, u32)> = match reg.entries.iter().find(|e| e.id == *choice) {
            Some(e) => e.installed_faces(&dir, &loc).into_iter().take(1).collect(),
            None => vec![(PathBuf::from(choice), 0)],
        };
        if let Some((path, index)) = faces.first() {
            match std::fs::read(path) {
                Ok(bytes) => {
                    let name = format!("chosen-{:?}-{}", family, path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
                    fonts.font_data.insert(name.clone(), Arc::new({ let mut fd = FontData::from_owned(bytes); fd.index = *index; fd }));
                    fonts.families.entry(family.clone()).or_default().insert(0, name);
                }
                Err(e) => tracing::warn!(path = %path.display(), "the chosen font could not be read: {e}"),
            }
        }
    }
    // installed packs, as fallbacks for both families
    let mut installed_bits = 0u32;
    for e in reg.entries.iter().filter(|e| e.kind == "fallback" && e.is_installed(&dir)) {
        for (path, index) in e.installed_faces(&dir, &loc) {
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let name = format!("pack-{}-{}-{index}", e.id, path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
                    fonts.font_data.insert(name.clone(), Arc::new({ let mut fd = FontData::from_owned(bytes); fd.index = index; fd }));
                    for fam in [FontFamily::Proportional, FontFamily::Monospace] {
                        fonts.families.entry(fam).or_default().push(name.clone());
                    }
                }
                Err(err) => tracing::warn!(path = %path.display(), "installed font pack could not be read: {err}"),
            }
        }
        for p in e.pack_set() {
            installed_bits |= p.bit();
        }
    }
    // system fonts already loaded this session stay
    let mut l = loader().lock();
    l.installed = installed_bits;
    for s in &l.system {
        fonts.font_data.insert(s.name.clone(), Arc::clone(&s.data));
        for fam in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(fam).or_default().push(s.name.clone());
        }
    }
    fonts
}

/// Rebuild and apply the definitions (a chosen font changed, a pack was installed or removed).
pub fn reload(ctx: &Context, settings: &cobalt_core::Settings, paths: &cobalt_store::AppPaths) {
    ctx.set_fonts(definitions(settings, paths));
    ctx.request_repaint();
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
        if (l.requested | l.installed) & p.bit() != 0 {
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

/// Read the pack's system font and hand it to egui as the last fallback of both families.
fn load_pack(ctx: &Context, pack: Pack) {
    let Some((path, index)) = find(pack) else {
        tracing::warn!(?pack, "no system font found for this script");
        loader().lock().missing.push(pack);
        ctx.request_repaint();
        return;
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(?pack, path = %path.display(), "could not read the font: {e}");
            loader().lock().missing.push(pack);
            return;
        }
    };
    tracing::info!(?pack, path = %path.display(), index, size = bytes.len(), "system font loaded for glyph fallback");
    let data = Arc::new({ let mut fd = FontData::from_owned(bytes); fd.index = index; fd });
    let name = format!("system-{pack:?}");
    loader().lock().system.push(Loaded { name: name.clone(), data: Arc::clone(&data) });
    ctx.add_font(FontInsert {
        name,
        data: (*data).clone(),
        families: vec![InsertFontFamily { family: FontFamily::Proportional, priority: FontPriority::Lowest }, InsertFontFamily { family: FontFamily::Monospace, priority: FontPriority::Lowest }],
    });
    ctx.request_repaint();
}

/// Packs no system font was found for since the last call (the suggestion to download).
pub fn take_missing() -> Vec<Pack> {
    std::mem::take(&mut loader().lock().missing)
}

/// For the agent and Settings: which packs have been served by system fonts this session.
pub fn requested() -> Vec<&'static str> {
    let l = loader().lock();
    ALL.iter().filter(|p| l.requested & p.bit() != 0).map(|p| p.name()).collect()
}

/// For the agent: what is loaded from where.
pub fn status_json(settings: &cobalt_core::Settings, paths: &cobalt_store::AppPaths) -> serde_json::Value {
    let dir = fonts_dir(paths);
    let reg = registry();
    let l = loader().lock();
    serde_json::json!({
        "dir": dir,
        "registry_remote": remote_fetched(),
        "entries": reg.entries.iter().map(|e| serde_json::json!({"id": e.id, "name": e.name, "kind": e.kind, "packs": e.packs, "size": e.total_size(), "installed": e.is_installed(&dir)})).collect::<Vec<_>>(),
        "system_loaded": l.system.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        "requested": ALL.iter().filter(|p| l.requested & p.bit() != 0).map(|p| format!("{p:?}")).collect::<Vec<_>>(),
        "ui_font": settings.fonts.ui_font,
        "mono_font": settings.fonts.mono_font,
        "suggest_downloads": settings.fonts.suggest_downloads,
        "locale": l.locale,
    })
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
        // Segoe UI Symbol has the mathematical alphanumerics, dingbats and monochrome emoji; Cambria Math the math set
        Pack::Symbols => &[("seguisym.ttf", 0), ("seguiemj.ttf", 0), ("cambria.ttc", 1)],
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
        Pack::Symbols => &[("/System/Library/Fonts/Apple Symbols.ttf", 0), ("/System/Library/Fonts/Supplemental/STIXGeneral.otf", 0), ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0)],
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
    if pack == Pack::Symbols {
        out.push((PathBuf::from("/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf"), 0));
        out.push((PathBuf::from("/usr/share/fonts/truetype/noto/NotoSansMath-Regular.ttf"), 0));
        out.push((PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"), 0));
    }
    out
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn candidates(_pack: Pack) -> Vec<(PathBuf, u32)> {
    Vec::new()
}

/// fontconfig's best match for the pack's language (`fc-match -f '%{file}|%{index}' :lang=ja`),
/// accepted only when the match claims the language (DejaVu answers for everything otherwise).
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
        Pack::Symbols => return None,
    };
    let out = std::process::Command::new("fc-match").arg("-f").arg("%{file}|%{index}|%{lang}").arg(format!(":lang={lang}")).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.trim().split('|');
    let file = it.next()?;
    let index = it.next().unwrap_or("0");
    let langs = it.next().unwrap_or("");
    let base = lang.split('-').next().unwrap_or(lang);
    if !langs.is_empty() && !langs.split('|').any(|l| l.eq_ignore_ascii_case(lang) || l.to_ascii_lowercase().starts_with(&format!("{base}-")) || l.eq_ignore_ascii_case(base)) {
        return None;
    }
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

/// A short human size.
pub fn human_size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{} KB", bytes.div_ceil(1000))
    }
}

/// Download progress of packs in flight, keyed by entry id (shared between Settings and the
/// suggestion window).
pub type Downloads = HashMap<String, Arc<Mutex<crate::update::DownloadProgress>>>;

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
    }

    #[test]
    fn stylised_names_and_emoji_are_symbols() {
        assert_eq!(scan("✕NAHO NARUSHIMA".as_bytes()).packs("en-US"), vec![Pack::Symbols]);
        assert_eq!(scan("𝙈𝙖𝙧𝙞𝙙𝙞𝙤𝙧".as_bytes()).packs("en-US"), vec![Pack::Symbols]);
        assert_eq!(scan("𝓚𝓲𝔂𝓪 𝓑✰".as_bytes()).packs("en-US"), vec![Pack::Symbols]);
        assert_eq!(scan("🤴🏽𝑲𝑬𝑶𝑵𝑻𝑬’✨👌🏽".as_bytes()).packs("en-US"), vec![Pack::Symbols]);
        assert_eq!(scan("𝐬𝐚𝐫𝐚𝐡❀".as_bytes()).packs("en-US"), vec![Pack::Symbols]);
    }

    #[test]
    fn mixed_text_lists_every_pack_once() {
        let s = scan("東京 Tokyo 서울 القاهرة".as_bytes());
        assert_eq!(s.packs("en-US"), vec![Pack::ChineseSimplified, Pack::Korean, Pack::MiddleEast]);
    }

    #[test]
    fn bundled_registry_parses() {
        let r: Registry = serde_json::from_str(BUNDLED_REGISTRY).unwrap();
        assert!(r.entries.len() >= 6);
        let cjk = r.entries.iter().find(|e| e.id == "noto-cjk").unwrap();
        assert_eq!(cjk.pack_set().len(), 4);
        assert!(cjk.covers(Pack::Japanese));
        assert!(cjk.total_size() > 10_000_000);
        for e in &r.entries {
            for p in &e.packs {
                assert!(Pack::parse(p).is_some(), "unknown pack {p} in {}", e.id);
            }
            assert!(!e.files.is_empty());
        }
        assert!(r.entries.iter().any(|e| e.kind == "ui"));
        assert!(r.entries.iter().any(|e| e.kind == "mono"));
    }

    #[test]
    fn faces_prefer_the_locale() {
        let r: Registry = serde_json::from_str(BUNDLED_REGISTRY).unwrap();
        let cjk = r.entries.iter().find(|e| e.id == "noto-cjk").unwrap();
        let dir = Path::new("x");
        let faces = cjk.installed_faces(dir, "ja-JP");
        assert_eq!(faces[0].1, 0);
        let faces = cjk.installed_faces(dir, "zh-TW");
        assert_eq!(faces[0].1, 3);
        let faces = cjk.installed_faces(dir, "en-US");
        assert_eq!(faces[0].1, 2);
    }
}
