//! Патчи, образы PKG и работа с игровыми файлами.
//!
//! Поддерживается:
//!  * поиск и замена байтов по шаблону (в том числе с wildcard `??`);
//!  * патчи в стиле «адрес = байт» для обхода проверок;
//!  * минимальный парсер заголовка PKG (формат Sony, magic `\x7fCNT`);
//!  * извлечение фалов из PKG не поддерживается (шифрование), но метаданные
//!    и таблица записей читаются;
//!  * контейнер `.fpkg` c открытым списком файлов (для тестов).

use std::fmt;
use std::path::Path;

#[derive(Debug)]
pub enum PatchError {
    BadPattern(String),
    NotFound(String),
    Io(String),
    Format(String),
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatchError::BadPattern(s) => write!(f, "неверный шаблон: {}", s),
            PatchError::NotFound(s) => write!(f, "не найдено: {}", s),
            PatchError::Io(s) => write!(f, "ошибка ввода/вывода: {}", s),
            PatchError::Format(s) => write!(f, "ошибка формата: {}", s),
        }
    }
}

/// Шаблон поиска: байты и «джокер» (None = любой байт).
#[derive(Clone, Debug)]
pub struct Pattern {
    pub bytes: Vec<Option<u8>>,
}

impl Pattern {
    /// Разобрать строку вида `48 8B 05 ?? ?? ?? ?? 48 85 C0`.
    pub fn parse(s: &str) -> Result<Self, PatchError> {
        let mut bytes = Vec::new();
        for tok in s.split_whitespace() {
            if tok == "??" || tok == "?" || tok == "**" {
                bytes.push(None);
                continue;
            }
            let b = u8::from_str_radix(tok.trim_start_matches("0x"), 16)
                .map_err(|_| PatchError::BadPattern(tok.to_string()))?;
            bytes.push(Some(b));
        }
        if bytes.is_empty() {
            return Err(PatchError::BadPattern("пустой шаблон".into()));
        }
        Ok(Pattern { bytes })
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Все позиции вхождения.
    pub fn find_all(&self, data: &[u8]) -> Vec<usize> {
        let mut out = Vec::new();
        if self.bytes.len() > data.len() {
            return out;
        }
        'outer: for i in 0..=data.len() - self.bytes.len() {
            for (j, pat) in self.bytes.iter().enumerate() {
                if let Some(b) = pat {
                    if data[i + j] != *b {
                        continue 'outer;
                    }
                }
            }
            out.push(i);
        }
        out
    }
}

/// Один патч: замена найденного шаблона.
#[derive(Clone, Debug)]
pub struct Patch {
    pub name: String,
    pub search: Pattern,
    pub replace: Vec<u8>,
    pub expected_hits: Option<usize>,
    pub enabled: bool,
}

impl Patch {
    pub fn new(name: &str, search: &str, replace_hex: &str) -> Result<Self, PatchError> {
        let search = Pattern::parse(search)?;
        let replace = parse_hex_bytes(replace_hex)?;
        if replace.len() != search.len() {
            return Err(PatchError::BadPattern(format!(
                "длина замены ({}) не совпадает с шаблоном ({})",
                replace.len(),
                search.len()
            )));
        }
        Ok(Patch {
            name: name.to_string(),
            search,
            replace,
            expected_hits: None,
            enabled: true,
        })
    }

    pub fn expect(mut self, n: usize) -> Self {
        self.expected_hits = Some(n);
        self
    }

    /// Применить патч «на месте».
    pub fn apply(&self, data: &mut [u8]) -> usize {
        if !self.enabled {
            return 0;
        }
        let hits = self.search.find_all(data);
        for i in &hits {
            data[*i..*i + self.replace.len()].copy_from_slice(&self.replace);
        }
        hits.len()
    }
}

pub fn parse_hex_bytes(s: &str) -> Result<Vec<u8>, PatchError> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace() && *c != ',').collect();
    if clean.len() % 2 != 0 {
        return Err(PatchError::BadPattern(format!("нечётное число символов: {}", s)));
    }
    let mut out = Vec::with_capacity(clean.len() / 2);
    let b = clean.as_bytes();
    for i in (0..b.len()).step_by(2) {
        let h = std::str::from_utf8(&b[i..i + 2])
            .map_err(|_| PatchError::BadPattern(s.to_string()))?;
        out.push(u8::from_str_radix(h, 16).map_err(|_| PatchError::BadPattern(s.to_string()))?);
    }
    Ok(out)
}

/// Набор патчей, читаемый из простого текстового формата:
/// ```text
/// # комментарий
/// patch "обход проверки" 48 85 C0 74 05 -> 48 85 C0 EB 05
/// expect 1
/// ```
pub fn parse_patch_file(text: &str) -> Result<Vec<Patch>, PatchError> {
    let mut out: Vec<Patch> = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with("//") {
            continue;
        }
        // Строка "expect N" относится к предыдущему патчу.
        if let Some(rest) = l.strip_prefix("expect ") {
            if let Some(last) = out.last_mut() {
                if let Ok(n) = rest.trim().parse::<usize>() {
                    last.expected_hits = Some(n);
                }
            }
            continue;
        }
        let _ = lineno;
        let (lhs, rhs) = l
            .split_once("->")
            .ok_or_else(|| PatchError::BadPattern(format!("строка {}: нет '->'", lineno + 1)))?;
        let lhs = lhs.trim();
        let (name, search) = split_name(lhs);
        let rep = rhs.split("expect").next().unwrap_or("").trim();
        let p = Patch::new(&name, &search, rep)?;
        out.push(p);
    }
    Ok(out)
}

fn split_name(s: &str) -> (String, String) {
    if let Some(rest) = s.strip_prefix("patch ") {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('"') {
            if let Some(e) = rest.find('"') {
                return (rest[..e].to_string(), rest[e + 1..].trim().to_string());
            }
        }
        let mut it = rest.splitn(2, char::is_whitespace);
        let name = it.next().unwrap_or("").to_string();
        let pat = it.next().unwrap_or("").trim().to_string();
        return (name, pat);
    }
    (String::from("патч"), s.to_string())
}

/// Заголовок PKG-образа (упрощённо).
#[derive(Clone, Debug, Default)]
pub struct PkgHeader {
    pub magic: u32,
    pub pkg_type: u32,
    pub file_count: u32,
    pub total_size: u64,
    pub entry_count: u32,
    pub table_offset: u64,
    pub content_id: String,
}

impl PkgHeader {
    /// Проверка сигнатуры `\x7fCNT` (`0x7F434E54`).
    pub const MAGIC: u32 = 0x7F43_4E54;
}

/// Разобрать заголовок PKG.
pub fn parse_pkg_header(data: &[u8]) -> Result<PkgHeader, PatchError> {
    if data.len() < 0x100 {
        return Err(PatchError::Format("файл слишком мал".into()));
    }
    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != PkgHeader::MAGIC {
        return Err(PatchError::Format(format!(
            "неверная сигнатура PKG: {:#010x}",
            magic
        )));
    }
    let pkg_type = u32::from_le_bytes([data[0x04], data[0x05], data[0x06], data[0x07]]);
    let file_count = u32::from_le_bytes([data[0x08], data[0x09], data[0x0A], data[0x0B]]);
    let entry_count = u32::from_le_bytes([data[0x0C], data[0x0D], data[0x0E], data[0x0F]]);
    let table_offset = u32::from_le_bytes([data[0x10], data[0x11], data[0x12], data[0x13]]) as u64;
    let total_size = u64::from_le_bytes([
        data[0x18], data[0x19], data[0x1A], data[0x1B], data[0x1C], data[0x1D], data[0x1E], data[0x1F],
    ]);
    let content_id = {
        let raw = &data[0x40..0x40 + 0x30.min(data.len().saturating_sub(0x40))];
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        String::from_utf8_lossy(&raw[..end]).trim().to_string()
    };
    Ok(PkgHeader {
        magic,
        pkg_type,
        file_count,
        total_size,
        entry_count,
        table_offset,
        content_id,
    })
}

/// Запись таблицы PKG.
#[derive(Clone, Debug)]
pub struct PkgEntry {
    pub id: u32,
    pub filename_offset: u32,
    pub flags: u32,
    pub offset: u32,
    pub size: u32,
    pub name: String,
}

/// Прочитать таблицу записей (32 байта на запись).
pub fn parse_pkg_entries(data: &[u8], hdr: &PkgHeader) -> Vec<PkgEntry> {
    let mut out = Vec::new();
    let base = hdr.table_offset as usize;
    if base == 0 || base + 0x1000 > data.len() {
        return out;
    }
    // Таблица имён обычно в конце первого блока.
    for i in 0..hdr.entry_count.min(4096) {
        let off = base + i as usize * 0x20;
        if off + 0x20 > data.len() {
            break;
        }
        let id = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        let name_off = u32::from_le_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);
        let flags = u32::from_le_bytes([data[off + 8], data[off + 9], data[off + 10], data[off + 11]]);
        let e_off = u32::from_le_bytes([data[off + 16], data[off + 17], data[off + 18], data[off + 19]]);
        let size = u32::from_le_bytes([data[off + 20], data[off + 21], data[off + 22], data[off + 23]]);
        let mut name = String::new();
        if name_off < 0x1000 && (base + name_off as usize) < data.len() {
            let start = base + name_off as usize;
            let rest = &data[start..data.len().min(start + 256)];
            let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
            name = String::from_utf8_lossy(&rest[..end]).to_string();
        }
        out.push(PkgEntry { id, filename_offset: name_off, flags, offset: e_off, size, name });
    }
    out
}

/// «Открытый» контейнер для тестов и отладки: `FPKG` + количество файлов,
/// затем записи «имя, размер, данные».
pub struct OpenPkg;

impl OpenPkg {
    pub const MAGIC: &'static [u8; 4] = b"FPKG";

    pub fn build(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(Self::MAGIC);
        out.extend_from_slice(&(files.len() as u32).to_le_bytes());
        for (name, data) in files {
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
        }
        out
    }

    pub fn parse(data: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
        if data.len() < 8 || &data[0..4] != Self::MAGIC {
            return None;
        }
        let count = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let mut pos = 8;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            if pos + 2 > data.len() {
                return None;
            }
            let nlen = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
            pos += 2;
            if pos + nlen + 4 > data.len() {
                return None;
            }
            let name = String::from_utf8_lossy(&data[pos..pos + nlen]).to_string();
            pos += nlen;
            let size = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
            pos += 4;
            if pos + size > data.len() {
                return None;
            }
            out.push((name, data[pos..pos + size].to_vec()));
            pos += size;
        }
        Some(out)
    }
}

/// Применить файл патчей к файлу образа.
pub fn apply_patch_file(elf_path: &Path, patch_path: &Path, out_path: &Path) -> Result<Vec<String>, PatchError> {
    let mut data = std::fs::read(elf_path).map_err(|e| PatchError::Io(e.to_string()))?;
    let text = std::fs::read_to_string(patch_path).map_err(|e| PatchError::Io(e.to_string()))?;
    let patches = parse_patch_file(&text)?;
    let mut log = Vec::new();
    for p in &patches {
        let n = p.apply(&mut data);
        log.push(format!("{}: применено {} раз", p.name, n));
        if let Some(exp) = p.expected_hits {
            if n != exp {
                log.push(format!(
                    "  предупреждение: ожидалось {} вхождений, найдено {}",
                    exp, n
                ));
            }
        }
    }
    std::fs::write(out_path, &data).map_err(|e| PatchError::Io(e.to_string()))?;
    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_with_wildcards() {
        let p = Pattern::parse("48 8B ?? 05").unwrap();
        assert_eq!(p.len(), 4);
        let data = [0x90, 0x48, 0x8B, 0xAA, 0x05, 0x90, 0x48, 0x8B, 0x00, 0x05];
        let hits = p.find_all(&data);
        assert_eq!(hits, vec![1, 6]);
    }

    #[test]
    fn patch_replaces_bytes() {
        let p = Patch::new("test", "74 05", "EB 05").unwrap();
        let mut data = vec![0x90, 0x74, 0x05, 0x90];
        assert_eq!(p.apply(&mut data), 1);
        assert_eq!(data, vec![0x90, 0xEB, 0x05, 0x90]);
    }

    #[test]
    fn patch_file_parsing() {
        let text = "# комментарий\npatch \"NOP ветки\" 74 05 -> 90 90\nexpect 2\n";
        let ps = parse_patch_file(text).unwrap();
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].name, "NOP ветки");
        assert_eq!(ps[0].expected_hits, Some(2));
    }

    #[test]
    fn pkg_header_roundtrip() {
        let mut data = vec![0u8; 0x200];
        data[0..4].copy_from_slice(&PkgHeader::MAGIC.to_le_bytes());
        data[0x04..0x08].copy_from_slice(&1u32.to_le_bytes());
        data[0x08..0x0C].copy_from_slice(&7u32.to_le_bytes());
        data[0x0C..0x10].copy_from_slice(&3u32.to_le_bytes());
        data[0x18..0x20].copy_from_slice(&123456u64.to_le_bytes());
        data[0x40..0x40 + 11].copy_from_slice(b"EP0000-CUSA");
        let h = parse_pkg_header(&data).unwrap();
        assert_eq!(h.file_count, 7);
        assert_eq!(h.entry_count, 3);
        assert_eq!(h.total_size, 123456);
        assert_eq!(h.content_id, "EP0000-CUSA");
    }

    #[test]
    fn pkg_bad_magic() {
        let data = vec![0u8; 0x200];
        assert!(parse_pkg_header(&data).is_err());
    }

    #[test]
    fn open_pkg_roundtrip() {
        let blob = OpenPkg::build(&[("eboot.bin", b"ELF..."), ("param.sfo", b"SFO")]);
        let files = OpenPkg::parse(&blob).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, "eboot.bin");
        assert_eq!(files[1].1, b"SFO");
    }
}
