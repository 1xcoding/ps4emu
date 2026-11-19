//! Виртуальная память гостя.
//!
//! Модель близка к реальной: плоское 48-битное адресное пространство,
//! разбитое на регионы с правами доступа. Небольшие регионы хранятся
//! линейно (быстрый путь), большие — разреженно по страницам 4 КиБ.

use std::collections::BTreeMap;
use std::fmt;

pub const PAGE: u64 = 0x1000;
pub const PAGE_MASK: u64 = PAGE - 1;

pub const P_READ: u32 = 1;
pub const P_WRITE: u32 = 2;
pub const P_EXEC: u32 = 4;

/// Порог, выше которого регион хранится разреженно.
const SPARSE_THRESHOLD: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemError {
    /// Адрес не принадлежит ни одному региону.
    Unmapped { addr: u64, write: bool },
    /// Регион есть, но прав не хватает.
    Prot { addr: u64, write: bool, prot: u32 },
}

impl fmt::Display for MemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemError::Unmapped { addr, write } => {
                write!(f, "обращение к неотображённой памяти {:#x} ({})", addr,
                       if *write { "запись" } else { "чтение" })
            }
            MemError::Prot { addr, write, prot } => write!(
                f,
                "нарушение прав доступа {:#x} ({}, права {})",
                addr,
                if *write { "запись" } else { "чтение" },
                prot_str(*prot)
            ),
        }
    }
}

pub fn prot_str(p: u32) -> String {
    format!(
        "{}{}{}",
        if p & P_READ != 0 { 'r' } else { '-' },
        if p & P_WRITE != 0 { 'w' } else { '-' },
        if p & P_EXEC != 0 { 'x' } else { '-' }
    )
}

#[derive(Debug)]
enum Backend {
    Linear(Vec<u8>),
    Sparse(BTreeMap<u64, Box<[u8; PAGE as usize]>>),
}

#[derive(Debug)]
pub struct Region {
    pub base: u64,
    pub size: u64,
    pub prot: u32,
    pub name: String,
    backend: Backend,
    /// Страницы, выделенные разреженно (для статистики).
    pub touched: u64,
}

impl Region {
    pub fn end(&self) -> u64 {
        self.base + self.size
    }
    fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr < self.end()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MemStats {
    pub reads: u64,
    pub writes: u64,
    pub faults: u64,
}

/// Наблюдательная точка (для отладчика).
#[derive(Clone, Debug)]
pub struct Watchpoint {
    pub addr: u64,
    pub len: u64,
    pub hits: u64,
}

pub struct Memory {
    regions: Vec<Region>,
    /// Индекс последнего успешного попадания (кэш для ускорения).
    hot: usize,
    /// Счётчик изменений кода — инвалидация кэша декодированных инструкций.
    pub code_gen: u64,
    pub stats: MemStats,
    pub watchpoints: Vec<Watchpoint>,
    /// Арена для внутренних (HLE) выделений.
    pub arena_next: u64,
    pub arena_end: u64,
}

impl Memory {
    pub fn new() -> Self {
        Memory {
            regions: Vec::new(),
            hot: usize::MAX,
            code_gen: 0,
            stats: MemStats::default(),
            watchpoints: Vec::new(),
            arena_next: 0,
            arena_end: 0,
        }
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    pub fn region_at(&self, addr: u64) -> Option<&Region> {
        self.regions.iter().find(|r| r.contains(addr))
    }

    /// Отображение региона. `size` округляется до страницы.
    pub fn map(&mut self, base: u64, size: u64, prot: u32, name: &str) -> Result<(), MemError> {
        let size = (size + PAGE_MASK) & !PAGE_MASK;
        if size == 0 {
            return Ok(());
        }
        if base & PAGE_MASK != 0 {
            return Err(MemError::Unmapped { addr: base, write: true });
        }
        self.unmap(base, size)?;
        let backend = if size >= SPARSE_THRESHOLD {
            Backend::Sparse(BTreeMap::new())
        } else {
            Backend::Linear(vec![0u8; size as usize])
        };
        let region = Region {
            base,
            size,
            prot,
            name: name.to_string(),
            backend,
            touched: 0,
        };
        let pos = self.regions.partition_point(|r| r.base < base);
        // Проверка пересечений (unmap уже срезал всё, что нужно).
        if let Some(next) = self.regions.get(pos) {
            if region.end() > next.base {
                return Err(MemError::Unmapped { addr: next.base, write: true });
            }
        }
        self.regions.insert(pos, region);
        self.hot = usize::MAX;
        Ok(())
    }

    /// Снятие отображения (частичное снятие разрезает регион).
    pub fn unmap(&mut self, base: u64, size: u64) -> Result<(), MemError> {
        let end = base.saturating_add(size);
        let mut i = 0;
        while i < self.regions.len() {
            let (rbase, rend) = (self.regions[i].base, self.regions[i].end());
            if rend <= base || rbase >= end {
                i += 1;
                continue;
            }
            // Полное покрытие — удаляем.
            if rbase >= base && rend <= end {
                self.regions.remove(i);
                continue;
            }
            // Разрезаем: нужен новый Vec для частей.
            let prot = self.regions[i].prot;
            let name = self.regions[i].name.clone();
            let clone_bytes = |r: &Region, from: u64, len: u64| -> Vec<u8> {
                let mut v = vec![0u8; len as usize];
                let _ = r.read_into(from - r.base, &mut v);
                v
            };
            let r = &self.regions[i];
            let (cut_start, cut_end) = (base.max(rbase), end.min(rend));
            let head_len = cut_start - rbase;
            let tail_len = rend - cut_end;
            let head = clone_bytes(r, rbase, head_len);
            let tail = clone_bytes(r, cut_end, tail_len);
            self.regions.remove(i);
            if head_len > 0 {
                self.regions.insert(
                    i,
                    Region {
                        base: rbase,
                        size: head_len,
                        prot,
                        name: name.clone(),
                        backend: Backend::Linear(head),
                        touched: 0,
                    },
                );
            }
            if tail_len > 0 {
                let at = self.regions.partition_point(|x| x.base < cut_end);
                self.regions.insert(
                    at,
                    Region {
                        base: cut_end,
                        size: tail_len,
                        prot,
                        name,
                        backend: Backend::Linear(tail),
                        touched: 0,
                    },
                );
            }
            i += 1;
        }
        self.hot = usize::MAX;
        Ok(())
    }

    pub fn protect(&mut self, base: u64, size: u64, prot: u32) -> Result<(), MemError> {
        let end = base.saturating_add(size);
        let mut any = false;
        for r in self.regions.iter_mut() {
            if r.base < end && r.end() > base {
                r.prot = prot;
                any = true;
            }
        }
        if any {
            Ok(())
        } else {
            Err(MemError::Unmapped { addr: base, write: true })
        }
    }

    /// Найти регион (с кэшем) и вернуть (индекс, смещение).
    #[inline(always)]
    fn locate(&mut self, addr: u64, write: bool) -> Result<(usize, u64), MemError> {
        if self.hot != usize::MAX {
            let r = &self.regions[self.hot];
            if addr >= r.base && addr < r.base + r.size {
                let need = if write { P_WRITE } else { P_READ };
                if r.prot & need == 0 {
                    self.stats.faults += 1;
                    return Err(MemError::Prot { addr, write, prot: r.prot });
                }
                return Ok((self.hot, addr - r.base));
            }
        }
        match self.regions.binary_search_by(|r| {
            if addr < r.base {
                std::cmp::Ordering::Greater
            } else if addr >= r.base + r.size {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        }) {
            Ok(i) => {
                let r = &self.regions[i];
                let need = if write { P_WRITE } else { P_READ };
                if r.prot & need == 0 {
                    self.stats.faults += 1;
                    return Err(MemError::Prot { addr, write, prot: r.prot });
                }
                self.hot = i;
                Ok((i, addr - r.base))
            }
            Err(_) => {
                self.stats.faults += 1;
                Err(MemError::Unmapped { addr, write })
            }
        }
    }

    #[inline]
    pub fn read_u8(&mut self, addr: u64) -> Result<u8, MemError> {
        self.stats.reads += 1;
        let (i, off) = self.locate(addr, false)?;
        Ok(self.regions[i].read8(off))
    }

    #[inline]
    pub fn write_u8(&mut self, addr: u64, v: u8) -> Result<(), MemError> {
        self.stats.writes += 1;
        self.note_write(addr);
        let (i, off) = self.locate(addr, true)?;
        self.regions[i].write8(off, v);
        if self.regions[i].prot & P_EXEC != 0 {
            self.code_gen = self.code_gen.wrapping_add(1);
        }
        Ok(())
    }

    #[inline]
    fn note_write(&mut self, addr: u64) {
        if self.watchpoints.is_empty() {
            return;
        }
        for w in self.watchpoints.iter_mut() {
            if addr >= w.addr && addr < w.addr + w.len {
                w.hits += 1;
            }
        }
    }

    pub fn read_u16(&mut self, a: u64) -> Result<u16, MemError> {
        let mut b = [0u8; 2];
        self.read_bytes(a, &mut b)?;
        Ok(u16::from_le_bytes(b))
    }
    pub fn read_u32(&mut self, a: u64) -> Result<u32, MemError> {
        let mut b = [0u8; 4];
        self.read_bytes(a, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    pub fn read_u64(&mut self, a: u64) -> Result<u64, MemError> {
        let mut b = [0u8; 8];
        self.read_bytes(a, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    pub fn read_i64(&mut self, a: u64) -> Result<i64, MemError> {
        self.read_u64(a).map(|v| v as i64)
    }
    pub fn read_i32(&mut self, a: u64) -> Result<i32, MemError> {
        self.read_u32(a).map(|v| v as i32)
    }
    pub fn write_i64(&mut self, a: u64, v: i64) -> Result<(), MemError> {
        self.write_u64(a, v as u64)
    }
    pub fn write_i32(&mut self, a: u64, v: i32) -> Result<(), MemError> {
        self.write_u32(a, v as u32)
    }
    pub fn write_u16(&mut self, a: u64, v: u16) -> Result<(), MemError> {
        self.write_bytes(a, &v.to_le_bytes())
    }
    pub fn write_u32(&mut self, a: u64, v: u32) -> Result<(), MemError> {
        self.write_bytes(a, &v.to_le_bytes())
    }
    pub fn write_u64(&mut self, a: u64, v: u64) -> Result<(), MemError> {
        self.write_bytes(a, &v.to_le_bytes())
    }

    pub fn read_bytes(&mut self, addr: u64, out: &mut [u8]) -> Result<(), MemError> {
        let mut done = 0usize;
        while done < out.len() {
            let a = addr + done as u64;
            let (i, off) = self.locate(a, false)?;
            let r = &self.regions[i];
            let avail = (r.size - off).min((out.len() - done) as u64) as usize;
            r.read_into(off, &mut out[done..done + avail]);
            done += avail;
        }
        self.stats.reads += 1;
        Ok(())
    }

    pub fn write_bytes(&mut self, addr: u64, data: &[u8]) -> Result<(), MemError> {
        let mut done = 0usize;
        let mut exec_touch = false;
        while done < data.len() {
            let a = addr + done as u64;
            self.note_write(a);
            let (i, off) = self.locate(a, true)?;
            let r = &mut self.regions[i];
            let avail = (r.size - off).min((data.len() - done) as u64) as usize;
            r.write_from(off, &data[done..done + avail]);
            if r.prot & P_EXEC != 0 {
                exec_touch = true;
            }
            done += avail;
        }
        if exec_touch {
            self.code_gen = self.code_gen.wrapping_add(1);
        }
        self.stats.writes += 1;
        Ok(())
    }

    /// Частичное чтение: возвращает, сколько байт удалось прочитать.
    pub fn read_partial(&mut self, addr: u64, out: &mut [u8]) -> usize {
        let mut done = 0usize;
        while done < out.len() {
            let a = addr + done as u64;
            let (i, off) = self.locate(a, false).unwrap_or((usize::MAX, 0));
            if i == usize::MAX {
                break;
            }
            let r = &self.regions[i];
            let avail = (r.size - off).min((out.len() - done) as u64) as usize;
            r.read_into(off, &mut out[done..done + avail]);
            done += avail;
        }
        done
    }

    /// Проверка, что диапазон отображён на запись.
    pub fn mapped(&self, addr: u64, len: u64) -> bool {
        if len == 0 {
            return true;
        }
        let mut cur = addr;
        let end = addr + len;
        while cur < end {
            match self.regions.iter().find(|r| r.contains(cur)) {
                Some(r) => cur = (r.end()).min(end),
                None => return false,
            }
        }
        true
    }

    /// Выделение памяти из арены (для структур HLE).
    pub fn arena_alloc(&mut self, size: u64, align: u64) -> Option<u64> {
        let align = align.max(1);
        let mut p = (self.arena_next + align - 1) & !(align - 1);
        if p + size > self.arena_end {
            return None;
        }
        self.arena_next = p + size;
        let _ = self.map(p & !PAGE_MASK, size, P_READ | P_WRITE, "hle-alloc");
        p = self.arena_next - size;
        Some(p)
    }

    pub fn map_arena(&mut self, base: u64, size: u64) -> Result<(), MemError> {
        self.map(base, size, P_READ | P_WRITE, "arena")?;
        self.arena_next = base;
        self.arena_end = base + ((size + PAGE_MASK) & !PAGE_MASK);
        Ok(())
    }

    /// Записать C-строку.
    pub fn write_cstr(&mut self, addr: u64, s: &str) -> Result<(), MemError> {
        self.write_bytes(addr, s.as_bytes())?;
        self.write_u8(addr + s.len() as u64, 0)
    }

    /// Прочитать C-строку (с ограничением длины).
    pub fn read_cstr(&mut self, addr: u64, max: usize) -> Result<String, MemError> {
        let mut out = Vec::new();
        for i in 0..max {
            let b = self.read_u8(addr + i as u64)?;
            if b == 0 {
                break;
            }
            out.push(b);
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Дамп региона для отладки.
    pub fn hexdump(&mut self, addr: u64, len: u64) -> String {
        let mut s = String::new();
        let mut a = addr;
        let end = addr + len;
        while a < end {
            let mut line = format!("{:016x}  ", a);
            for i in 0..16u64 {
                if a + i >= end {
                    line.push_str("   ");
                    continue;
                }
                match self.read_u8(a + i) {
                    Ok(b) => line.push_str(&format!("{:02x} ", b)),
                    Err(_) => line.push_str("?? "),
                }
            }
            line.push(' ');
            for i in 0..16u64 {
                if a + i >= end {
                    break;
                }
                let c = self.read_u8(a + i).unwrap_or(b'.');
                line.push(if (0x20..0x7f).contains(&c) { c as char } else { '.' });
            }
            s.push_str(&line);
            s.push('\n');
            a += 16;
        }
        s
    }
}

impl Default for Memory {
    fn default() -> Self {
        Memory::new()
    }
}

impl Region {
    #[inline(always)]
    fn read8(&self, off: u64) -> u8 {
        match &self.backend {
            Backend::Linear(v) => v[off as usize],
            Backend::Sparse(m) => {
                let page = off / PAGE;
                let idx = (off % PAGE) as usize;
                m.get(&page).map(|p| p[idx]).unwrap_or(0)
            }
        }
    }

    #[inline(always)]
    fn write8(&mut self, off: u64, val: u8) {
        match &mut self.backend {
            Backend::Linear(v) => v[off as usize] = val,
            Backend::Sparse(m) => {
                let page = off / PAGE;
                let idx = (off % PAGE) as usize;
                let entry = m.entry(page).or_insert_with(|| {
                    self.touched += 1;
                    Box::new([0u8; PAGE as usize])
                });
                entry[idx] = val;
            }
        }
    }

    fn read_into(&self, off: u64, out: &mut [u8]) {
        match &self.backend {
            Backend::Linear(v) => out.copy_from_slice(&v[off as usize..off as usize + out.len()]),
            Backend::Sparse(m) => {
                let mut done = 0usize;
                while done < out.len() {
                    let o = off + done as u64;
                    let page = o / PAGE;
                    let idx = (o % PAGE) as usize;
                    let n = ((PAGE as usize - idx).min(out.len() - done)) as usize;
                    match m.get(&page) {
                        Some(p) => out[done..done + n].copy_from_slice(&p[idx..idx + n]),
                        None => out[done..done + n].fill(0),
                    }
                    done += n;
                }
            }
        }
    }

    fn write_from(&mut self, off: u64, data: &[u8]) {
        match &mut self.backend {
            Backend::Linear(v) => v[off as usize..off as usize + data.len()].copy_from_slice(data),
            Backend::Sparse(m) => {
                let mut done = 0usize;
                while done < data.len() {
                    let o = off + done as u64;
                    let page = o / PAGE;
                    let idx = (o % PAGE) as usize;
                    let n = (PAGE as usize - idx).min(data.len() - done);
                    let entry = m.entry(page).or_insert_with(|| Box::new([0u8; PAGE as usize]));
                    entry[idx..idx + n].copy_from_slice(&data[done..done + n]);
                    done += n;
                }
            }
        }
    }
}
