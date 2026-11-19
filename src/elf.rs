//! Разбор 64-битных ELF (FreeBSD-вариант, как у PS4/Orbis OS) и SELF.

use std::fmt;

pub const ET_EXEC: u16 = 2;
pub const ET_DYN: u16 = 3;
pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_INTERP: u32 = 3;
pub const PT_NOTE: u32 = 4;
pub const PT_PHDR: u32 = 6;
pub const PT_TLS: u32 = 7;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

#[derive(Debug)]
pub enum ElfError {
    TooShort,
    BadMagic,
    Not64Bit,
    NotLE,
    BadClass(String),
    Truncated(String),
    Unsupported(String),
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElfError::TooShort => write!(f, "файл слишком мал для ELF"),
            ElfError::BadMagic => write!(f, "неверная сигнатура ELF"),
            ElfError::Not64Bit => write!(f, "поддерживаются только 64-битные ELF"),
            ElfError::NotLE => write!(f, "поддерживаются только little-endian ELF"),
            ElfError::BadClass(s) => write!(f, "неверный заголовок: {}", s),
            ElfError::Truncated(s) => write!(f, "усечённые данные: {}", s),
            ElfError::Unsupported(s) => write!(f, "не поддерживается: {}", s),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProgramHeader {
    pub p_type: u32,
    pub flags: u32,
    pub offset: u64,
    pub vaddr: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub align: u64,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name_off: u32,
    pub value: u64,
    pub size: u64,
    pub info: u8,
    pub shndx: u16,
}

impl Symbol {
    pub fn is_func(&self) -> bool {
        self.info & 0xF == 2
    }
    pub fn bind(&self) -> u8 {
        self.info >> 4
    }
}

#[derive(Clone, Debug)]
pub struct Section {
    pub name_off: u32,
    pub sh_type: u32,
    pub flags: u64,
    pub addr: u64,
    pub offset: u64,
    pub size: u64,
    pub link: u32,
    pub entsize: u64,
}

#[derive(Clone, Debug, Default)]
pub struct DynInfo {
    pub needed: Vec<String>,
    pub soname: Option<String>,
    pub rpath: Vec<String>,
    pub init: u64,
    pub fini: u64,
}

pub struct Elf {
    pub entry: u64,
    pub e_type: u16,
    pub e_machine: u16,
    pub phdrs: Vec<ProgramHeader>,
    pub sections: Vec<Section>,
    pub shstrtab: Vec<u8>,
    pub symbols: Vec<Symbol>,
    /// Строки для символов (одна общая таблица, если она есть).
    pub strtab: Vec<u8>,
    pub dynamic: DynInfo,
    /// Байты динамической секции.
    pub dyn_entries: Vec<(u64, u64)>,
    pub interp: Option<String>,
    /// Базовый адрес, на который будет загружен образ.
    pub load_base: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Machine {
    X86_64,
    Unknown(u16),
}

pub fn read_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}
pub fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
pub fn read_u64(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}

pub fn parse(data: &[u8]) -> Result<Elf, ElfError> {
    if data.len() < 64 {
        return Err(ElfError::TooShort);
    }
    if &data[0..4] != b"\x7fELF" {
        return Err(ElfError::BadMagic);
    }
    if data[4] != 2 {
        return Err(ElfError::Not64Bit);
    }
    if data[5] != 1 {
        return Err(ElfError::NotLE);
    }
    let e_type = read_u16(data, 16);
    let e_machine = read_u16(data, 18);
    let entry = read_u64(data, 24);
    let phoff = read_u64(data, 32) as usize;
    let shoff = read_u64(data, 40) as usize;
    let phentsize = read_u16(data, 54) as usize;
    let phnum = read_u16(data, 56) as usize;
    let shentsize = read_u16(data, 58) as usize;
    let shnum = read_u16(data, 60) as usize;
    let shstrndx = read_u16(data, 62) as usize;

    if e_machine != 62 {
        return Err(ElfError::BadClass(format!("машина {} (нужен x86-64)", e_machine)));
    }

    let mut phdrs = Vec::with_capacity(phnum);
    for i in 0..phnum {
        let o = phoff + i * phentsize;
        if o + 56 > data.len() {
            return Err(ElfError::Truncated("program header".into()));
        }
        phdrs.push(ProgramHeader {
            p_type: read_u32(data, o),
            flags: read_u32(data, o + 4),
            offset: read_u64(data, o + 8),
            vaddr: read_u64(data, o + 16),
            filesz: read_u64(data, o + 32),
            memsz: read_u64(data, o + 40),
            align: read_u64(data, o + 48),
        });
    }

    let mut sections = Vec::new();
    if shoff != 0 && shnum > 0 && shoff < data.len() {
        for i in 0..shnum {
            let o = shoff + i * shentsize;
            if o + 64 > data.len() {
                break;
            }
            sections.push(Section {
                name_off: read_u32(data, o),
                sh_type: read_u32(data, o + 4),
                flags: read_u64(data, o + 8),
                addr: read_u64(data, o + 16),
                offset: read_u64(data, o + 24) as usize as u64,
                size: read_u64(data, o + 32),
                link: read_u32(data, o + 40),
                entsize: read_u64(data, o + 56),
            });
        }
    }

    let mut shstrtab = Vec::new();
    if shstrndx < sections.len() {
        let s = &sections[shstrndx];
        let (a, b) = (s.offset as usize, (s.offset + s.size) as usize);
        if b <= data.len() {
            shstrtab = data[a..b].to_vec();
        }
    }

    let mut symbols = Vec::new();
    let mut strtab = Vec::new();
    for s in &sections {
        if s.sh_type == 2 {
            // SYMTAB
            if let Some(strsec) = sections.get(s.link as usize) {
                let (a, b) = (strsec.offset as usize, (strsec.offset + strsec.size) as usize);
                if b <= data.len() {
                    strtab = data[a..b].to_vec();
                }
            }
            let ent = if s.entsize > 0 { s.entsize as usize } else { 24 };
            let start = s.offset as usize;
            let end = ((s.offset + s.size) as usize).min(data.len());
            let mut o = start;
            while o + 24 <= end {
                symbols.push(Symbol {
                    name_off: read_u32(data, o),
                    info: data[o + 4],
                    shndx: read_u16(data, o + 6),
                    value: read_u64(data, o + 8),
                    size: read_u64(data, o + 16),
                });
                o += ent;
            }
        }
    }

    // Заголовочная строка .shstrtab нужна для диагностики; оставляем.
    let _ = &shstrtab;

    let mut interp = None;
    for ph in &phdrs {
        if ph.p_type == PT_INTERP && ph.filesz > 0 {
            let (a, b) = (ph.offset as usize, (ph.offset + ph.filesz) as usize);
            if b <= data.len() {
                let s = &data[a..b];
                let e = s.iter().position(|&c| c == 0).unwrap_or(s.len());
                interp = Some(String::from_utf8_lossy(&s[..e]).into_owned());
            }
        }
    }

    let mut dyn_entries = Vec::new();
    for ph in &phdrs {
        if ph.p_type == PT_DYNAMIC {
            let mut o = ph.offset as usize;
            let end = ((ph.offset + ph.filesz) as usize).min(data.len());
            while o + 16 <= end {
                let tag = read_u64(data, o) as i64;
                let val = read_u64(data, o + 8);
                dyn_entries.push((tag as u64, val));
                if tag == 0 {
                    break;
                }
                o += 16;
            }
        }
    }

    let mut dynamic = DynInfo::default();
    for (tag, val) in &dyn_entries {
        match *tag {
            1 => {
                let s = cstr_from_vaddr(&sections, data, *val);
                if let Some(s) = s {
                    dynamic.needed.push(s);
                }
            }
            5 => dynamic.soname = cstr_from_vaddr(&sections, data, *val),
            12 => dynamic.init = *val,
            13 => dynamic.fini = *val,
            _ => {}
        }
    }

    let load_base = phdrs
        .iter()
        .filter(|p| p.p_type == PT_LOAD)
        .map(|p| p.vaddr)
        .min()
        .unwrap_or(0);

    Ok(Elf {
        entry,
        e_type,
        e_machine,
        phdrs,
        sections,
        shstrtab,
        symbols,
        strtab,
        dynamic,
        dyn_entries,
        interp,
        load_base,
    })
}

fn cstr_from_vaddr(sections: &[Section], data: &[u8], vaddr: u64) -> Option<String> {
    // Ищем секцию, содержащую адрес (обычно .dynstr).
    for s in sections {
        if vaddr >= s.addr && vaddr < s.addr + s.size {
            let off = (s.offset + (vaddr - s.addr)) as usize;
            if off < data.len() {
                let rest = &data[off..];
                let e = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
                return Some(String::from_utf8_lossy(&rest[..e]).into_owned());
            }
        }
    }
    None
}

impl Elf {
    pub fn symbol_name(&self, s: &Symbol) -> &str {
        let o = s.name_off as usize;
        if o >= self.strtab.len() {
            return "";
        }
        let rest = &self.strtab[o..];
        let e = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
        std::str::from_utf8(&rest[..e]).unwrap_or("")
    }

    /// Найти символ по адресу (для трассировки).
    pub fn symbol_at(&self, addr: u64, load_bias: u64) -> Option<(String, u64)> {
        let mut best: Option<(&Symbol, String)> = None;
        for s in &self.symbols {
            if !s.is_func() || s.value == 0 {
                continue;
            }
            let start = s.value.wrapping_add(load_bias);
            if addr >= start && addr < start + s.size.max(1) {
                if best.as_ref().map(|(b, _)| start >= b.value + load_bias).unwrap_or(true) {
                    best = Some((s, self.symbol_name(s).to_string()));
                }
            }
        }
        best.map(|(s, n)| (n, s.value + load_bias))
    }

    pub fn sections_by_name(&self, name: &str) -> Vec<&Section> {
        let mut out = Vec::new();
        for s in &self.sections {
            let o = s.name_off as usize;
            if o < self.shstrtab.len() {
                let rest = &self.shstrtab[o..];
                let e = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
                if &rest[..e] == name.as_bytes() {
                    out.push(s);
                }
            }
        }
        out
    }

    /// Таблицы динамических символов (.dynsym/.dynstr) — индексы в RELA
    /// ссылаются именно на них.
    pub fn dynsym(&self, data: &[u8]) -> (Vec<u64>, Vec<u8>) {
        let mut sym_off = 0usize;
        let mut sym_size = 0usize;
        let mut str_off = 0usize;
        let mut str_size = 0usize;
        for s in &self.sections {
            if s.sh_type == 11 {
                sym_off = s.offset as usize;
                sym_size = s.size as usize;
                if let Some(link) = self.sections.get(s.link as usize) {
                    str_off = link.offset as usize;
                    str_size = link.size as usize;
                }
            }
        }
        let syms = if sym_size > 0 && sym_off + sym_size <= data.len() {
            data[sym_off..sym_off + sym_size]
                .chunks_exact(24)
                .map(|c| read_u64(c, 8))
                .collect()
        } else {
            Vec::new()
        };
        let strs = if str_size > 0 && str_off + str_size <= data.len() {
            data[str_off..str_off + str_size].to_vec()
        } else {
            Vec::new()
        };
        (syms, strs)
    }

    /// Имя символа из строковой таблицы.
    fn str_at(strs: &[u8], off: u64) -> String {
        let o = off as usize;
        if strs.is_empty() || o >= strs.len() {
            return String::new();
        }
        let rest = &strs[o..];
        let e = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
        String::from_utf8_lossy(&rest[..e]).into_owned()
    }

    /// Импортированные (неразрешённые) функции: (имя, адрес GOT/PLT-слота).
    pub fn imports(&self, data: &[u8]) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        // Индексы символов в .rela.dyn/.rela.plt указывают на .dynsym,
        // поэтому имена берём именно оттуда (а не из .symtab).
        let mut sym_name_off: Vec<u64> = Vec::new();
        for s in &self.sections {
            if s.sh_type == 11 && s.entsize > 0 {
                let start = s.offset as usize;
                let end = ((s.offset + s.size) as usize).min(data.len());
                let mut o = start;
                while o + 24 <= end {
                    sym_name_off.push(read_u32(data, o) as u64);
                    o += 24;
                }
            }
        }
        let (_, dynstr) = self.dynsym(data);
        for s in &self.sections {
            if s.sh_type != 4 {
                continue;
            }
            let ent = if s.entsize > 0 { s.entsize as usize } else { 24 };
            let start = s.offset as usize;
            let end = ((s.offset + s.size) as usize).min(data.len());
            let mut o = start;
            while o + 24 <= end {
                let r_offset = read_u64(data, o);
                let r_info = read_u64(data, o + 8);
                let sym = (r_info >> 32) as usize;
                if let Some(name_off) = sym_name_off.get(sym) {
                    let name = Self::str_at(&dynstr, *name_off);
                    if !name.is_empty() {
                        out.push((name, r_offset));
                    }
                }
                o += ent;
            }
        }
        out
    }
}
