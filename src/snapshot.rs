//! Снапшоты состояния эмуляции (сохранение/загрузка).
//!
//! Формат — собственный бинарный, с магическим числом, версией и
//! контрольной суммой (CRC-32). Позволяет «замораживать» процесс
//! посреди игры и продолжать с того же места.

use crate::emulator::Emulator;
use std::io::{Read, Write};

pub const MAGIC: &[u8; 8] = b"PS4SNAP\x01";
pub const VERSION: u32 = 1;

#[derive(Debug)]
pub enum SnapshotError {
    Io(String),
    BadMagic,
    Version(u32),
    Crc { expected: u32, actual: u32 },
    Corrupt(String),
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotError::Io(s) => write!(f, "ошибка ввода/вывода: {}", s),
            SnapshotError::BadMagic => write!(f, "неверная сигнатура снапшота"),
            SnapshotError::Version(v) => write!(f, "неподдерживаемая версия снапшота: {}", v),
            SnapshotError::Crc { expected, actual } => write!(
                f,
                "несовпадение контрольной суммы (ожидалось {:08x}, получено {:08x})",
                expected, actual
            ),
            SnapshotError::Corrupt(s) => write!(f, "повреждённые данные: {}", s),
        }
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in data {
        crc ^= *b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Writer { buf: Vec::new() }
    }
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.buf.extend_from_slice(v);
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], SnapshotError> {
        if self.pos + n > self.data.len() {
            return Err(SnapshotError::Corrupt("неожиданный конец файла".into()));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, SnapshotError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, SnapshotError> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> Result<u64, SnapshotError> {
        let s = self.take(8)?;
        Ok(u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
    }
    fn bytes(&mut self) -> Result<Vec<u8>, SnapshotError> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
}

/// Сериализация с доступом к памяти (основной путь).
pub fn save_mut(emu: &mut Emulator) -> Vec<u8> {
    let mut w = Writer::new();
    w.buf.extend_from_slice(MAGIC);
    w.u32(VERSION);
    for r in &emu.cpu.regs.gpr {
        w.u64(*r);
    }
    w.u64(emu.cpu.regs.rip);
    w.u64(emu.cpu.regs.rflags);
    w.u64(emu.cpu.regs.fs_base);
    w.u64(emu.cpu.regs.gs_base);
    for x in &emu.cpu.regs.xmm {
        w.buf.extend_from_slice(&x.to_le_bytes());
    }
    w.u64(emu.cpu.stats.total);
    for s in &emu.cpu.x87.st {
        w.u64(s.to_bits());
    }
    w.u8(emu.cpu.x87.top);
    w.u32(emu.cpu.x87.cw as u32);
    w.u32(emu.cpu.x87.sw as u32);
    let regions: Vec<(u64, u64, u32, String)> = emu
        .mem
        .regions()
        .iter()
        .map(|r| (r.base, r.size, r.prot, r.name.clone()))
        .collect();
    w.u32(regions.len() as u32);
    for (base, size, prot, name) in regions {
        w.u64(base);
        w.u64(size);
        w.u32(prot);
        w.bytes(name.as_bytes());
        // В снимок попадают только страницы с ненулевым содержимым:
        // так 512-МиБ адресное пространство укладывается в мегабайты.
        let mut pages: Vec<u32> = Vec::new();
        let mut content = Vec::new();
        let mut off = 0u64;
        let mut page = [0u8; crate::mem::PAGE as usize];
        while off < size {
            let n = emu.mem.read_partial(base + off, &mut page);
            if n == 0 {
                break;
            }
            if page[..n].iter().any(|b| *b != 0) {
                pages.push((off / crate::mem::PAGE) as u32);
                content.extend_from_slice(&page[..n]);
            }
            off += crate::mem::PAGE;
        }
        w.u32(pages.len() as u32);
        for p in &pages {
            w.u32(*p);
        }
        let packed = rle_encode(&content);
        w.bytes(&packed);
    }
    w.u64(emu.sys.heap_brk);
    w.u64(emu.sys.heap_start);
    w.u64(emu.sys.heap_end);
    w.u64(emu.sys.mmap_hint);
    w.u64(emu.sys.mmap_end);
    w.u32(emu.sys.pid as u32);
    w.u32(emu.sys.tid as u32);
    w.u32(emu.sys.next_fd as u32);
    w.bytes(emu.sys.stdout.as_bytes());
    w.bytes(emu.sys.stderr_buf.as_bytes());
    w.u32(emu.sys.vfs.len() as u32);
    for (k, v) in &emu.sys.vfs {
        w.bytes(k.as_bytes());
        w.bytes(v);
    }
    w.u32(emu.sys.open_files.len() as u32);
    for (fd, f) in &emu.sys.open_files {
        w.u32(*fd as u32);
        let (kind, path, pos, writable): (u8, String, u64, bool) = match f {
            crate::sys::FileObj::Stdin => (0, String::new(), 0, false),
            crate::sys::FileObj::Stdout => (1, String::new(), 0, true),
            crate::sys::FileObj::Stderr => (2, String::new(), 0, true),
            crate::sys::FileObj::Rng => (3, String::new(), 0, false),
            crate::sys::FileObj::Mem { path, pos, writable, .. } => (4, path.clone(), *pos, *writable),
            crate::sys::FileObj::Dir { path, .. } => (5, path.clone(), 0, false),
            crate::sys::FileObj::Socket { domain, ty, .. } => (6, String::new(), *domain, *ty != 0),
        };
        w.u8(kind);
        w.bytes(path.as_bytes());
        w.u64(pos);
        w.u8(writable as u8);
    }
    w.u32(emu.modules.len() as u32);
    for m in &emu.modules {
        w.bytes(m.name.as_bytes());
        w.u64(m.base);
        w.u64(m.end);
        w.u64(m.entry);
    }
    w.u32(emu.stub_slots.len() as u32);
    for (addr, name) in &emu.stub_slots {
        w.u64(*addr);
        w.bytes(name.as_bytes());
    }
    // Служебное состояние, без которого продолжение невозможно.
    w.u64(emu.tls_block);
    w.u64(emu.tls_memsz);
    w.u64(emu.tls_tp);
    w.u64(emu.hle.heap_next);
    w.u64(emu.hle.heap_end);
    w.u32(emu.breakpoints.len() as u32);
    for b in &emu.breakpoints {
        w.u64(*b);
    }
    let sum = crc32(&w.buf);
    w.u32(sum);
    w.buf
}

/// Восстановить состояние (память полностью перезаписывается).
pub fn load_mut(emu: &mut Emulator, data: &[u8]) -> Result<(), SnapshotError> {
    if data.len() < MAGIC.len() + 8 {
        return Err(SnapshotError::Corrupt("файл слишком мал".into()));
    }
    if &data[0..MAGIC.len()] != MAGIC {
        return Err(SnapshotError::BadMagic);
    }
    let body = &data[..data.len() - 4];
    let expected = u32::from_le_bytes([
        data[data.len() - 4],
        data[data.len() - 3],
        data[data.len() - 2],
        data[data.len() - 1],
    ]);
    let actual = crc32(body);
    if expected != actual {
        return Err(SnapshotError::Crc { expected, actual });
    }
    let mut r = Reader::new(body);
    r.take(MAGIC.len())?;
    let ver = r.u32()?;
    if ver != VERSION {
        return Err(SnapshotError::Version(ver));
    }
    for i in 0..16 {
        emu.cpu.regs.gpr[i] = r.u64()?;
    }
    emu.cpu.regs.rip = r.u64()?;
    emu.cpu.regs.rflags = r.u64()?;
    emu.cpu.regs.fs_base = r.u64()?;
    emu.cpu.regs.gs_base = r.u64()?;
    for i in 0..16 {
        let s = r.take(16)?;
        let mut a = [0u8; 16];
        a.copy_from_slice(s);
        emu.cpu.regs.xmm[i] = u128::from_le_bytes(a);
    }
    emu.cpu.stats.total = r.u64()?;
    for i in 0..8 {
        emu.cpu.x87.st[i] = f64::from_bits(r.u64()?);
    }
    emu.cpu.x87.top = r.u8()? & 7;
    emu.cpu.x87.cw = r.u32()? as u16;
    emu.cpu.x87.sw = r.u32()? as u16;

    let nregions = r.u32()? as usize;
    // Снимаем все текущие регионы, кроме служебных, затем восстанавливаем.
    let old: Vec<(u64, u64)> = emu.mem.regions().iter().map(|x| (x.base, x.size)).collect();
    for (b, s) in old {
        let _ = emu.mem.unmap(b, s);
    }
    for _ in 0..nregions {
        let base = r.u64()?;
        let size = r.u64()?;
        let prot = r.u32()?;
        let name = String::from_utf8_lossy(&r.bytes()?).to_string();
        let npages = r.u32()? as usize;
        let mut page_idx = Vec::with_capacity(npages);
        for _ in 0..npages {
            page_idx.push(r.u32()?);
        }
        let packed = r.bytes()?;
        let content = rle_decode(&packed)?;
        emu.mem
            .map(base, size, prot | crate::mem::P_WRITE, &name)
            .map_err(|e| SnapshotError::Corrupt(format!("регион {:#x}: {}", base, e)))?;
        let page_size = crate::mem::PAGE as usize;
        for (i, p) in page_idx.iter().enumerate() {
            let start = i * page_size;
            let end = (start + page_size).min(content.len());
            if start < content.len() {
                let addr = base + (*p as u64) * crate::mem::PAGE;
                emu.mem
                    .write_bytes(addr, &content[start..end])
                    .map_err(|e| SnapshotError::Corrupt(format!("содержимое {:#x}: {}", addr, e)))?;
            }
        }
        let _ = emu.mem.protect(base, size, prot);
    }
    emu.sys.heap_brk = r.u64()?;
    emu.sys.heap_start = r.u64()?;
    emu.sys.heap_end = r.u64()?;
    emu.sys.mmap_hint = r.u64()?;
    emu.sys.mmap_end = r.u64()?;
    emu.sys.pid = r.u32()? as i32;
    emu.sys.tid = r.u32()? as i32;
    emu.sys.next_fd = r.u32()? as i32;
    emu.sys.stdout = String::from_utf8_lossy(&r.bytes()?).to_string();
    emu.sys.stderr_buf = String::from_utf8_lossy(&r.bytes()?).to_string();
    let nvfs = r.u32()? as usize;
    emu.sys.vfs.clear();
    for _ in 0..nvfs {
        let k = String::from_utf8_lossy(&r.bytes()?).to_string();
        let v = r.bytes()?;
        emu.sys.vfs.insert(k, v);
    }
    let nfiles = r.u32()? as usize;
    emu.sys.open_files.clear();
    for _ in 0..nfiles {
        let fd = r.u32()? as i32;
        let kind = r.u8()?;
        let path = String::from_utf8_lossy(&r.bytes()?).to_string();
        let pos = r.u64()?;
        let wr = r.u8()? != 0;
        let obj = match kind {
            0 => crate::sys::FileObj::Stdin,
            1 => crate::sys::FileObj::Stdout,
            2 => crate::sys::FileObj::Stderr,
            3 => crate::sys::FileObj::Rng,
            4 => {
                let data = emu.sys.vfs.get(&path).cloned().unwrap_or_default();
                crate::sys::FileObj::Mem { path, data, pos, writable: wr }
            }
            5 => crate::sys::FileObj::Dir { path, entries: Vec::new(), done: false },
            _ => crate::sys::FileObj::Socket { domain: pos, ty: wr as u64, connected: true },
        };
        emu.sys.open_files.insert(fd, obj);
    }
    let nmods = r.u32()? as usize;
    emu.modules.clear();
    for _ in 0..nmods {
        let name = String::from_utf8_lossy(&r.bytes()?).to_string();
        let base = r.u64()?;
        let end = r.u64()?;
        let entry = r.u64()?;
        emu.modules.push(crate::emulator::Module { name: name.clone(), base, end, entry, path: name });
    }
    let nstubs = r.u32()? as usize;
    emu.stub_slots.clear();
    for _ in 0..nstubs {
        let addr = r.u64()?;
        let name = String::from_utf8_lossy(&r.bytes()?).to_string();
        emu.hle.exports.insert(name.clone(), addr);
        emu.stub_slots.insert(addr, name);
    }
    emu.tls_block = r.u64()?;
    emu.tls_memsz = r.u64()?;
    emu.tls_tp = r.u64()?;
    emu.hle.heap_next = r.u64()?;
    emu.hle.heap_end = r.u64()?;
    let nbp = r.u32()? as usize;
    emu.breakpoints.clear();
    for _ in 0..nbp {
        emu.breakpoints.push(r.u64()?);
    }
    Ok(())
}

/// Сохранить в файл.
pub fn save_file(emu: &mut Emulator, path: &str) -> Result<usize, SnapshotError> {
    let data = save_mut(emu);
    let mut f = std::fs::File::create(path).map_err(|e| SnapshotError::Io(e.to_string()))?;
    f.write_all(&data).map_err(|e| SnapshotError::Io(e.to_string()))?;
    Ok(data.len())
}

/// Загрузить из файла.
pub fn load_file(emu: &mut Emulator, path: &str) -> Result<(), SnapshotError> {
    let mut f = std::fs::File::open(path).map_err(|e| SnapshotError::Io(e.to_string()))?;
    let mut data = Vec::new();
    f.read_to_end(&mut data).map_err(|e| SnapshotError::Io(e.to_string()))?;
    load_mut(emu, &data)
}

/// Сжатие длин серий: поток команд `(счётчик, байт)`, где старший бит
/// счётчика означает «серия нулей» (счётчик 15 бит, максимум 32767).
fn rle_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 4 + 16);
    let mut i = 0usize;
    while i < data.len() {
        let b = data[i];
        let mut run = 1usize;
        while i + run < data.len() && data[i + run] == b && run < 0x7FFF {
            run += 1;
        }
        if b == 0 {
            out.push(0x80);
            out.push(b);
            out.extend_from_slice(&(run as u16).to_le_bytes());
        } else if run >= 4 {
            out.push(0x00);
            out.push(b);
            out.extend_from_slice(&(run as u16).to_le_bytes());
        } else {
            for _ in 0..run {
                out.push(0x01);
                out.push(b);
            }
        }
        i += run;
    }
    out
}

fn rle_decode(data: &[u8]) -> Result<Vec<u8>, SnapshotError> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        if i + 2 > data.len() {
            return Err(SnapshotError::Corrupt("обрезанный RLE-поток".into()));
        }
        let tag = data[i];
        let b = data[i + 1];
        i += 2;
        match tag {
            0x80 => {
                if i + 2 > data.len() {
                    return Err(SnapshotError::Corrupt("обрезанный RLE-поток".into()));
                }
                let n = u16::from_le_bytes([data[i], data[i + 1]]) as usize;
                i += 2;
                out.extend(std::iter::repeat(0u8).take(n));
            }
            0x00 => {
                if i + 2 > data.len() {
                    return Err(SnapshotError::Corrupt("обрезанный RLE-поток".into()));
                }
                let n = u16::from_le_bytes([data[i], data[i + 1]]) as usize;
                i += 2;
                out.extend(std::iter::repeat(b).take(n));
            }
            _ => out.push(b),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn crc32_known_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn save_and_restore_state() {
        let mut emu = Emulator::new(Config::default()).unwrap();
        emu.cpu.regs.gpr[0] = 0xDEAD_BEEF;
        emu.cpu.regs.rip = 0x401000;
        emu.mem.write_u64(0x2_0000_1000, 0x1122_3344).unwrap();
        emu.sys.stdout.push_str("привет");
        let data = save_mut(&mut emu);
        let mut emu2 = Emulator::new(Config::default()).unwrap();
        load_mut(&mut emu2, &data).unwrap();
        assert_eq!(emu2.cpu.regs.gpr[0], 0xDEAD_BEEF);
        assert_eq!(emu2.cpu.regs.rip, 0x401000);
        assert_eq!(emu2.mem.read_u64(0x2_0000_1000).unwrap(), 0x1122_3344);
        assert_eq!(emu2.sys.stdout, "привет");
    }

    #[test]
    fn corrupted_snapshot_detected() {
        let mut emu = Emulator::new(Config::default()).unwrap();
        let mut data = save_mut(&mut emu);
        let n = data.len();
        data[n / 2] ^= 0xFF;
        let mut emu2 = Emulator::new(Config::default()).unwrap();
        assert!(matches!(load_mut(&mut emu2, &data), Err(SnapshotError::Crc { .. })));
    }

    #[test]
    fn bad_magic_detected() {
        let mut emu = Emulator::new(Config::default()).unwrap();
        let mut data = save_mut(&mut emu);
        data[0] = b'X';
        assert!(matches!(load_mut(&mut emu, &data), Err(SnapshotError::BadMagic)));
    }
}
