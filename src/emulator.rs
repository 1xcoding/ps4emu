//! Оркестратор эмуляции: загрузка образа, карта памяти, основной цикл,
//! диспетчеризация системных вызовов и HLE.

use crate::config::Config;
use crate::cpu::{Cpu, CpuError, Step};
use crate::elf::{self, Elf};
use crate::gpu::Gpu;
use crate::hle::{Args, Hle, HleError};
use crate::mem::{Memory, MemError, PAGE, P_EXEC, P_READ, P_WRITE};
use crate::oracle::NidTable;
use crate::sys::{self, Sys, SyscallCtx};
use crate::trace::{Symbols, Tracer};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

pub const STACK_TOP: u64 = 0x1_0000_0000;
pub const STACK_SIZE: u64 = 8 * 1024 * 1024;
pub const BRK_BASE: u64 = 0x2_0000_0000;
pub const BRK_SIZE: u64 = 0x1000_0000;
pub const MMAP_BASE: u64 = 0x4_0000_0000;
pub const MMAP_END: u64 = 0x6_0000_0000;
pub const APP_PARAM_BASE: u64 = 0x7_0000_0000;
pub const STUB_BASE: u64 = 0x8_0000_0000;
pub const STUB_STRIDE: u64 = 16;
pub const PIE_BASE: u64 = 0x10_0000_0000;
pub const HLE_HEAP: u64 = 0x8000_0000;
pub const HLE_HEAP_SIZE: u64 = 0x1000_0000;
pub const TLS_BASE: u64 = 0x9_0000_0000;

#[derive(Debug)]
pub enum EmuError {
    Io(String),
    Elf(String),
    Cpu(CpuError),
    Load(String),
    Stopped(String),
}

impl fmt::Display for EmuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmuError::Io(s) => write!(f, "ошибка ввода/вывода: {}", s),
            EmuError::Elf(s) => write!(f, "ошибка ELF: {}", s),
            EmuError::Cpu(e) => write!(f, "ошибка процессора: {}", e),
            EmuError::Load(s) => write!(f, "ошибка загрузки: {}", s),
            EmuError::Stopped(s) => write!(f, "остановлено: {}", s),
        }
    }
}

impl From<CpuError> for EmuError {
    fn from(e: CpuError) -> Self {
        EmuError::Cpu(e)
    }
}

impl From<MemError> for EmuError {
    fn from(e: MemError) -> Self {
        EmuError::Cpu(CpuError::Memory(e))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// Гостевая программа вызвала exit/exit_group.
    Exit(i32),
    Halt,
    /// Достигнут лимит инструкций.
    StepLimit,
    /// Ошибка исполнения.
    Fault(String),
    /// Вызвана HLE-функция, требующая остановки.
    HleStop(String),
    /// Отладчик.
    Breakpoint(u64),
}

#[derive(Clone, Debug)]
pub struct RunResult {
    pub reason: StopReason,
    pub steps: u64,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub name: String,
    pub base: u64,
    pub end: u64,
    pub entry: u64,
    pub path: String,
}

pub struct Emulator {
    pub cpu: Cpu,
    pub mem: Memory,
    pub sys: Sys,
    pub hle: Hle,
    pub tracer: Tracer,
    pub symbols: Symbols,
    pub modules: Vec<Module>,
    pub nids: NidTable,
    pub stub_slots: BTreeMap<u64, String>,
    pub config: Config,
    pub last_syscall: u64,
    pub last_hle: String,
    /// Сколько раз вызывали нереализованные функции.
    pub unsupported_calls: u64,
    pub pending_exit: Option<i32>,
    /// Состояние отладчика: адреса точек останова.
    pub breakpoints: Vec<u64>,
    pub startup_args: Vec<String>,
    /// Границы статического блока TLS и указатель потока.
    pub tls_block: u64,
    pub tls_memsz: u64,
    pub tls_tp: u64,
    /// Сведения о таблице program header для auxv.
    pub phdr_addr: u64,
    pub phnum: u64,
    pub phentsize: u64,
    pub load_bias: u64,
    /// Программный растеризатор и состояние вывода изображения.
    pub gpu: Gpu,
    /// Виртуальный геймпад, который читает гость через `scePadRead`.
    pub pad: crate::gpu::pad::PadState,
}

impl Emulator {
    pub fn new(config: Config) -> Result<Self, EmuError> {
        let mut mem = Memory::new();
        let mut sys = Sys::new();
        let mut hle = Hle::new();
        sys.work_dir = config.out_dir.to_string_lossy().to_string();
        let gpu = Gpu::new(config.width, config.height);
        let mut pad = crate::gpu::pad::PadState::default();
        pad.connected = true;

        // Стек
        mem.map(STACK_TOP - STACK_SIZE, STACK_SIZE, P_READ | P_WRITE, "stack")?;
        // Куча brk
        mem.map(BRK_BASE, BRK_SIZE, P_READ | P_WRITE, "brk")?;
        sys.heap_start = BRK_BASE;
        sys.heap_brk = BRK_BASE + 0x10000;
        sys.heap_end = BRK_BASE + BRK_SIZE;
        // Область mmap
        sys.mmap_hint = MMAP_BASE;
        sys.mmap_end = MMAP_END;
        // Арена HLE
        mem.map(HLE_HEAP, HLE_HEAP_SIZE, P_READ | P_WRITE, "hle-heap")
            .unwrap_or_default();
        hle.heap_next = HLE_HEAP + 0x10_0000;
        hle.heap_end = HLE_HEAP + HLE_HEAP_SIZE;
        mem.map_arena(APP_PARAM_BASE, 0x0010_0000).ok();
        // Регион под TLS (вариант II: блок ниже указателя потока).
        mem.map(TLS_BASE, 0x0002_0000, P_READ | P_WRITE, "tls")?;
        emu_tls_default(&mut mem);
        // Заглушки HLE: страница с точками входа, каждая по 16 байт
        mem.map(STUB_BASE, 0x2000_0000, P_READ | P_EXEC, "hle-stubs")?;

        let mut emu = Emulator {
            cpu: Cpu::new(),
            mem,
            sys,
            hle,
            tracer: Tracer::new(4096),
            symbols: Symbols::new(),
            modules: Vec::new(),
            nids: NidTable::new(),
            stub_slots: BTreeMap::new(),
            config: config.clone(),
            last_syscall: 0,
            last_hle: String::new(),
            unsupported_calls: 0,
            pending_exit: None,
            breakpoints: Vec::new(),
            startup_args: Vec::new(),
            tls_block: 0,
            tls_memsz: 0,
            tls_tp: 0,
            phdr_addr: 0,
            phnum: 0,
            phentsize: 56,
            load_bias: 0,
            gpu,
            pad,
        };
        emu.nids.populate_known();
        if let Some(p) = &config.nids {
            if let Ok(text) = std::fs::read_to_string(p) {
                let n = emu.nids.load_json(&text);
                emu.sys.log(format!("загружено NID из {:?}: {}", p, n));
            }
        }
        emu.cpu.trace = config.trace_cpu;
        emu.hle.trace = config.trace;
        emu.sys.log_enabled = config.trace;
        emu.tracer.enabled = config.trace_cpu;
        Ok(emu)
    }

    /// Выдать адрес заглушки для импортируемой функции.
    fn stub_for(&mut self, name: &str) -> u64 {
        if let Some(a) = self.hle.exports.get(name) {
            return *a;
        }
        let idx = self.stub_slots.len() as u64;
        let addr = STUB_BASE + idx * STUB_STRIDE;
        self.stub_slots.insert(addr, name.to_string());
        self.hle.exports.insert(name.to_string(), addr);
        addr
    }

    /// Загрузка основной программы.
    pub fn load(&mut self, path: &Path) -> Result<(), EmuError> {
        let raw = std::fs::read(path).map_err(|e| EmuError::Io(format!("{:?}: {}", path, e)))?;
        let data = unwrap_container(&raw)?;
        let interp = None;
        self.load_image(&data, path.to_string_lossy().as_ref(), interp)?;
        // Виртуальные пути
        let app0 = self.config.app0.clone();
        self.sys.vfs.insert(app0.clone(), data.clone());
        self.sys
            .vfs
            .insert(format!("/app0/{}", path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "eboot.bin".into())), data);
        Ok(())
    }

    /// Загрузка ELF-образа в память.
    pub fn load_image(&mut self, data: &[u8], name: &str, _interp: Option<&str>) -> Result<u64, EmuError> {
        let elf = elf::parse(data).map_err(|e| EmuError::Elf(e.to_string()))?;
        let bias = if elf.e_type == elf::ET_DYN { PIE_BASE } else { 0 };
        let mut start = u64::MAX;
        let mut end = 0u64;
        let mut perms: Vec<(u64, u64, u32)> = Vec::new();
        for ph in &elf.phdrs {
            if ph.p_type != elf::PT_LOAD {
                continue;
            }
            let base = (ph.vaddr.wrapping_add(bias)) & !0xFFF;
            let off = ph.vaddr & 0xFFF;
            let size = ph.memsz + off;
            let prot = {
                let mut p = 0;
                if ph.flags & elf::PF_R != 0 {
                    p |= P_READ;
                }
                if ph.flags & elf::PF_W != 0 {
                    p |= P_WRITE;
                }
                if ph.flags & elf::PF_X != 0 {
                    p |= P_EXEC;
                }
                p
            };
            // На время загрузки сегмент доступен на запись, затем права
            // выставляются в соответствии с PT_LOAD.flags.
            self.mem.map(base, size, prot | P_READ | P_WRITE, "elf")?;
            let src_start = ph.offset as usize;
            let src_end = (ph.offset + ph.filesz) as usize;
            if src_end > data.len() {
                return Err(EmuError::Elf(format!("сегмент за пределами файла ({} > {})", src_end, data.len())));
            }
            self.mem
                .write_bytes(ph.vaddr.wrapping_add(bias), &data[src_start..src_end])?;
            perms.push((base, size, prot));
            start = start.min(ph.vaddr.wrapping_add(bias));
            end = end.max(ph.vaddr.wrapping_add(bias) + ph.memsz);
        }
        if start == u64::MAX {
            return Err(EmuError::Elf("нет загружаемых сегментов".into()));
        }
        self.load_bias = bias;
        self.phnum = elf.phdrs.len() as u64;
        self.phentsize = 56;
        self.phdr_addr = elf
            .phdrs
            .iter()
            .find(|p| p.p_type == elf::PT_PHDR)
            .map(|p| p.vaddr.wrapping_add(bias))
            .unwrap_or(start.wrapping_add(64));
        // TLS нужен до релокаций: R_X86_64_TPOFF64 ссылается на tp.
        self.setup_tls(&elf, data, bias)?;
        // Релокации и импорты (до выставления прав: GOT может оказаться
        // внутри сегмента кода, как в ET_DYN с DT_TEXTREL).
        let imports = self.apply_relocations(&elf, data, bias, start, end)?;
        // Права доступа выставляем после загрузки всех сегментов: страницы на
        // стыке сегментов получают объединённые права.
        apply_page_perms(&mut self.mem, &perms)?;

        let entry = elf.entry.wrapping_add(bias);
        self.modules.push(Module {
            name: name.to_string(),
            base: start,
            end,
            entry,
            path: name.to_string(),
        });
        self.symbols.add_module(start, end, &short_name(name));
        for s in &elf.symbols {
            if s.is_func() && s.value != 0 {
                self.symbols.add_symbol(s.value + bias, s.size, elf.symbol_name(s));
            }
        }
        self.sys.log(format!(
            "загружен {} : entry {:#x}, сегментов {}, импортов {}",
            name,
            entry,
            elf.phdrs.iter().filter(|p| p.p_type == elf::PT_LOAD).count(),
            imports
        ));
        Ok(entry)
    }

    fn apply_relocations(
        &mut self,
        elf: &Elf,
        data: &[u8],
        bias: u64,
        start: u64,
        end: u64,
    ) -> Result<usize, EmuError> {
        // Находим .dynsym/.dynstr и таблицы релокаций.
        let mut dynsym_off = 0usize;
        let mut dynsym_size = 0usize;
        let mut dynstr_off = 0usize;
        let mut dynstr_size = 0usize;
        for s in &elf.sections {
            match s.sh_type {
                11 => {
                    dynsym_off = s.offset as usize;
                    dynsym_size = s.size as usize;
                    if let Some(link) = elf.sections.get(s.link as usize) {
                        dynstr_off = link.offset as usize;
                        dynstr_size = link.size as usize;
                    }
                }
                _ => {}
            }
        }
        let dynstr = if dynstr_size > 0 && dynstr_off + dynstr_size <= data.len() {
            &data[dynstr_off..dynstr_off + dynstr_size]
        } else {
            &data[0..0]
        };
        let sym_count = dynsym_size / 24;
        let dynsym = if dynsym_off + dynsym_size <= data.len() && dynsym_size > 0 {
            &data[dynsym_off..dynsym_off + dynsym_size]
        } else {
            &data[0..0]
        };
        let sym_name = |idx: usize| -> Option<String> {
            if idx == 0 || idx >= sym_count || dynsym.is_empty() {
                return None;
            }
            let o = idx * 24;
            let name_off = elf::read_u32(dynsym, o) as usize;
            if name_off >= dynstr.len() {
                return None;
            }
            let rest = &dynstr[name_off..];
            let e = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
            Some(String::from_utf8_lossy(&rest[..e]).into_owned())
        };

        let mut resolved = 0usize;
        for s in &elf.sections {
            // SHT_RELA = 4
            if s.sh_type != 4 || s.entsize == 0 {
                continue;
            }
            let ent = s.entsize as usize;
            let start_off = s.offset as usize;
            let stop = (s.offset + s.size) as usize;
            let stop = stop.min(data.len());
            let mut o = start_off;
            while o + 24 <= stop {
                // Для ET_DYN смещения релокаций отсчитываются от базы загрузки.
                let r_offset = elf::read_u64(data, o).wrapping_add(bias);
                let r_info = elf::read_u64(data, o + 8);
                let r_addend = elf::read_u64(data, o + 16) as i64;
                let sym = (r_info >> 32) as usize;
                let rtype = (r_info & 0xFFFF_FFFF) as u32;
                let where_ = r_offset;
                match rtype {
                    // R_X86_64_64
                    1 => {
                        let base = if let Some(name) = sym_name(sym) {
                            let v = self.resolve_import(&name);
                            if v != 0 {
                                resolved += 1;
                            }
                            v as i64
                        } else {
                            bias as i64
                        };
                        let v = base.wrapping_add(r_addend) as u64;
                        self.mem.write_u64(where_, v)?;
                    }
                    // R_X86_64_GLOB_DAT (6) / JUMP_SLOT (7)
                    6 | 7 => {
                        if let Some(name) = sym_name(sym) {
                            let mut v = self.resolve_import(&name);
                            if v == 0 {
                                v = self.missing_stub(&name);
                            } else {
                                resolved += 1;
                            }
                            self.mem.write_u64(where_, v)?;
                        } else if rtype == 6 {
                            self.mem.write_u64(where_, 0)?;
                        }
                    }
                    // R_X86_64_RELATIVE (8)
                    8 => {
                        let v = bias.wrapping_add(r_addend as u64);
                        self.mem.write_u64(where_, v)?;
                    }
                    // R_X86_64_PC32 / PLT32 (2/4)
                    2 | 4 => {
                        let v = if let Some(name) = sym_name(sym) {
                            let t = self.resolve_import(&name);
                            t as i64
                        } else {
                            bias as i64
                        }
                        .wrapping_add(r_addend);
                        let pc_rel = v - where_ as i64;
                        self.mem.write_u32(where_, pc_rel as u32)?;
                    }
                    // R_X86_64_32 / 32S (10/11)
                    10 | 11 => {
                        let v = bias.wrapping_add(r_addend as u64);
                        self.mem.write_u32(where_, v as u32)?;
                    }
                    // R_X86_64_DTPMOD64 (16) / DTPOFF64 (17) / TPOFF64 (18)
                    16 => {
                        self.mem.write_u64(where_, 1)?;
                    }
                    17 => {
                        let st = if sym < sym_count && !dynsym.is_empty() {
                            elf::read_u64(dynsym, sym * 24 + 8) as i64
                        } else {
                            0
                        };
                        self.mem.write_u64(where_, (st + r_addend) as u64)?;
                    }
                    18 => {
                        let st = if sym < sym_count && !dynsym.is_empty() {
                            elf::read_u64(dynsym, sym * 24 + 8) as i64
                        } else {
                            0
                        };
                        let v = st + r_addend - self.tls_memsz as i64;
                        self.mem.write_u64(where_, v as u64)?;
                    }
                    _ => {}
                }
                o += ent;
            }
        }
        // Символы из .dynsym регистрируем для символизации
        for i in 1..sym_count {
            let o = i * 24;
            let value = elf::read_u64(dynsym, o + 8);
            let info = dynsym[o + 4];
            let size = elf::read_u64(dynsym, o + 16);
            if info & 0xF == 2 && value != 0 && value >= start.saturating_sub(bias) && value < end {
                if let Some(n) = sym_name(i) {
                    self.symbols.add_symbol(value + bias, size, &n);
                }
            }
        }
        Ok(resolved)
    }

    /// Подставить адрес заглушки для имени импорта.
    fn resolve_import(&mut self, name: &str) -> u64 {
        // NID-стиль: 16 hex-символов
        let mut real = name.to_string();
        if name.len() == 16 && name.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(nid) = u32::from_str_radix(name, 16) {
                if let Some(n) = self.nids.lookup(nid) {
                    real = n.to_string();
                }
            }
        }
        if crate::hle::supported(&real) {
            let addr = self.stub_for(&real);
            if real != name {
                self.hle.exports.insert(name.to_string(), addr);
            }
            addr
        } else {
            self.hle.missing.insert(real.clone());
            0
        }
    }

    fn missing_stub(&mut self, name: &str) -> u64 {
        let key = format!("?{}", name);
        self.stub_for(&key)
    }

    fn setup_tls(&mut self, elf: &Elf, data: &[u8], _bias: u64) -> Result<(), EmuError> {
        let tp = TLS_BASE + 0x1_0000;
        self.tls_tp = tp;
        for ph in &elf.phdrs {
            if ph.p_type == elf::PT_TLS && ph.memsz > 0 {
                let memsz = ph.memsz;
                let block = tp - memsz;
                self.tls_block = block;
                self.tls_memsz = memsz;
                let src = (ph.offset as usize).min(data.len());
                let src_end = ((ph.offset + ph.filesz) as usize).min(data.len());
                if src_end > src {
                    let init = data[src..src_end].to_vec();
                    self.mem.write_bytes(block, &init)?;
                }
                break;
            }
        }
        // Блок управления потоком (TCB) сразу за указателем потока.
        self.mem.write_u64(tp, tp)?;          // self
        self.mem.write_u64(tp + 8, 0)?;       // dtv
        self.mem.write_u64(tp + 16, tp)?;     // tcb
        self.cpu.regs.fs_base = tp;
        Ok(())
    }

    /// Подготовить стек: argc/argv/envp/auxv.
    pub fn setup_stack(&mut self, args: &[String], env: &[(String, String)]) -> Result<(), EmuError> {
        let mut sp = STACK_TOP - 0x100;
        // Строки аргументов
        let mut arg_ptrs = Vec::new();
        let argv: Vec<String> = if args.is_empty() {
            vec![self.config.app0.clone()]
        } else {
            args.to_vec()
        };
        self.startup_args = argv.clone();
        for a in argv.iter().rev() {
            let bytes = a.as_bytes();
            sp -= bytes.len() as u64 + 1;
            self.mem.write_bytes(sp, bytes)?;
            self.mem.write_u8(sp + bytes.len() as u64, 0)?;
            arg_ptrs.push(sp);
        }
        arg_ptrs.reverse();
        let mut env_ptrs = Vec::new();
        for (k, v) in env.iter().rev() {
            let s = format!("{}={}", k, v);
            let bytes = s.as_bytes();
            sp -= bytes.len() as u64 + 1;
            self.mem.write_bytes(sp, bytes)?;
            self.mem.write_u8(sp + bytes.len() as u64, 0)?;
            env_ptrs.push(sp);
        }
        env_ptrs.reverse();
        // Выравнивание: после auxv указатель должен быть 16-байтовым
        let words = 1 + arg_ptrs.len() + 1 + env_ptrs.len() + 1 + 20 * 2;
        sp &= !0xF;
        sp -= (words * 8) as u64;
        sp &= !0xF;
        let mut p = sp;
        let argc = arg_ptrs.len() as u64;
        self.mem.write_u64(p, argc)?;
        p += 8;
        for a in &arg_ptrs {
            self.mem.write_u64(p, *a)?;
            p += 8;
        }
        self.mem.write_u64(p, 0)?;
        p += 8;
        for e in &env_ptrs {
            self.mem.write_u64(p, *e)?;
            p += 8;
        }
        self.mem.write_u64(p, 0)?;
        // AT_RANDOM: 16 случайных байт ниже строк
        sp -= 16;
        sp &= !0xF;
        let random_addr = sp;
        let random = [
            0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
            0xff, 0x01,
        ];
        self.mem.write_bytes(random_addr, &random)?;
        let entry = self.modules.last().map(|m| m.entry).unwrap_or(0);
        let execfn = arg_ptrs.first().copied().unwrap_or(0);
        let aux = [
            (3u64, self.phdr_addr),      // AT_PHDR
            (4, self.phentsize),         // AT_PHENT
            (5, self.phnum),             // AT_PHNUM
            (6, 0x4000),                 // AT_PAGESZ
            (7, 0),                      // AT_BASE
            (8, 0),                      // AT_FLAGS
            (9, entry),                  // AT_ENTRY
            (11, self.sys.uid as u64),   // AT_UID
            (12, self.sys.uid as u64),   // AT_EUID
            (13, self.sys.gid as u64),   // AT_GID
            (14, self.sys.gid as u64),   // AT_EGID
            (16, 0),                     // AT_HWCAP
            (17, 100),                   // AT_CLKTCK
            (23, 0),                     // AT_SECURE
            (25, random_addr),           // AT_RANDOM
            (31, execfn),                // AT_EXECFN
            (0, 0),
        ];
        // argc/argv/envp/auxv — единый непрерывный блок перед строками.
        let argv_bytes = (1 + arg_ptrs.len() + 1 + env_ptrs.len() + 1) * 8;
        let table_words = 1 + arg_ptrs.len() + 1 + env_ptrs.len() + 1 + aux.len() * 2;
        let aligned = if (sp as usize) % 16 == 0 { 0 } else { 8 };
        let table_bytes = (table_words * 8 + aligned) as u64;
        sp -= table_bytes;
        sp &= !0xF;
        let mut p = sp;
        self.mem.write_u64(p, arg_ptrs.len() as u64)?;
        p += 8;
        for a in &arg_ptrs {
            self.mem.write_u64(p, *a)?;
            p += 8;
        }
        self.mem.write_u64(p, 0)?;
        p += 8;
        for e in &env_ptrs {
            self.mem.write_u64(p, *e)?;
            p += 8;
        }
        self.mem.write_u64(p, 0)?;
        p += 8;
        let _ = argv_bytes;
        for (t, v) in aux {
            self.mem.write_u64(p, t)?;
            self.mem.write_u64(p + 8, v)?;
            p += 16;
        }
        self.cpu.regs.gpr[4] = sp;
        Ok(())
    }

    /// Основной цикл исполнения.
    pub fn run(&mut self) -> RunResult {
        let entry = self.modules.last().map(|m| m.entry).unwrap_or(0);
        if self.cpu.regs.rip == 0 {
            self.cpu.reset(entry, self.cpu.regs.gpr[4]);
        }
        let max = if self.config.max_steps == 0 { u64::MAX } else { self.config.max_steps };
        let mut steps = 0u64;
        #[allow(unused_assignments)]
        let mut reason = StopReason::Halt;
        loop {
            if steps >= max {
                reason = StopReason::StepLimit;
                break;
            }
            if let Some(code) = self.pending_exit.take() {
                reason = StopReason::Exit(code);
                break;
            }
            if self.sys.exited {
                reason = StopReason::Exit(self.sys.exit_code);
                break;
            }
            if self.sys.fs_base != 0 || self.sys.gs_base != 0 {
                self.apply_thread_bases();
            }
            let rip = self.cpu.regs.rip;
            if self.breakpoints.contains(&rip) {
                reason = StopReason::Breakpoint(rip);
                break;
            }
            // Вызов HLE-заглушки?
            if rip >= STUB_BASE && rip < STUB_BASE + 0x2000_0000 {
                if let Some(name) = self.stub_slots.get(&rip).cloned() {
                    match self.call_hle(&name) {
                        Ok(()) => {}
                        Err(e) => {
                            reason = match e {
                                HleError::Stop(s) => StopReason::HleStop(s),
                                HleError::Unsupported => StopReason::Fault(format!(
                                    "HLE-функция {} не реализована",
                                    name
                                )),
                            };
                            break;
                        }
                    }
                    steps += 1;
                    continue;
                }
            }
            let step_res = self.cpu.step(&mut self.mem);
            self.sanity_check(rip, self.cpu.regs.rip);
            match step_res {
                Ok(Step::Continue) => {
                    if self.tracer.enabled {
                        let mut b = [0u8; 8];
                        let _ = self.mem.read_partial(rip, &mut b);
                        self.tracer.record(rip, &b, self.cpu.regs.gpr[0], self.cpu.regs.gpr[4]);
                    }
                    steps += 1;
                }
                Ok(Step::Syscall) => {
                    self.do_syscall();
                    steps += 1;
                }
                Ok(Step::Hlt) => {
                    self.sys.hlt_count += 1;
                    if std::env::var("PS4EMU_BT").is_ok() {
                        eprintln!("--- hlt на {:#x}, стек вызовов гостя ---", rip);
                        let frames = self.guest_backtrace();
                        eprint!("{}", frames);
                    }
                    if self.sys.hlt_count > 1000 {
                        reason = StopReason::Halt;
                        break;
                    }
                    // HLT в гостевом коде: пропускаем как nop
                    steps += 1;
                }
                Ok(Step::Interrupt(n)) => {
                    if n == 3 {
                        reason = StopReason::Breakpoint(rip);
                        break;
                    }
                    steps += 1;
                }
                Err(e) => {
                    reason = StopReason::Fault(e.to_string());
                    break;
                }
            }
        }
        if self.tracer.enabled && std::env::var("PS4EMU_TRACE_TAIL").is_ok() {
            eprintln!("--- последние инструкции ---");
            eprint!("{}", self.tracer.dump(80));
        }
        RunResult {
            reason,
            steps,
            stdout: self.sys.stdout.clone(),
            stderr: self.sys.stderr_buf.clone(),
            exit_code: self.sys.exit_code,
        }
    }

    /// Выполнить один HLE-вызов (адрес возврата — на вершине стека).
    pub fn call_hle(&mut self, name: &str) -> Result<(), HleError> {
        let ret = self.mem.read_u64(self.cpu.regs.gpr[4]).unwrap_or(0);
        let stack_args = self.cpu.regs.gpr[4] + 8;
        self.cpu.regs.gpr[4] = self.cpu.regs.gpr[4] + 8;
        self.cpu.regs.rip = ret;
        self.last_hle = name.to_string();
        let regs = [
            self.cpu.regs.gpr[7],
            self.cpu.regs.gpr[6],
            self.cpu.regs.gpr[2],
            self.cpu.regs.gpr[1],
            self.cpu.regs.gpr[8],
            self.cpu.regs.gpr[9],
        ];
        let mut args = Args { regs, mem: &mut self.mem, stack: stack_args };
        let res = if let Some(real) = name.strip_prefix('?') {
            self.hle.missing.insert(real.to_string());
            self.unsupported_calls += 1;
            Ok(0)
        } else {
            crate::hle::call(name, &mut args, &mut self.sys, &mut self.hle)
        };
        match res {
            Ok(v) => {
                self.cpu.regs.gpr[0] = v;
                Ok(())
            }
            Err(HleError::Unsupported) => {
                self.unsupported_calls += 1;
                self.cpu.regs.gpr[0] = 0;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Обработать инструкцию syscall.
    pub fn do_syscall(&mut self) {
        let nr = self.cpu.regs.gpr[0];
        let a = [
            self.cpu.regs.gpr[7],
            self.cpu.regs.gpr[6],
            self.cpu.regs.gpr[2],
            self.cpu.regs.gpr[10],
            self.cpu.regs.gpr[8],
            self.cpu.regs.gpr[9],
        ];
        self.last_syscall = nr;
        if std::env::var("PS4EMU_BT_SYS").is_ok() && (nr == sys::nr::WRITE || nr == sys::nr::WRITEV || nr == 1 || nr == 20) && a[0] == 2 {
            eprintln!("--- syscall write(fd=2) на {:#x}, стек гостя ---", self.cpu.last_rip);
            let frames = self.guest_backtrace();
            eprint!("{}", frames);
        }
        let mut ctx = SyscallCtx {
            mem: &mut self.mem,
            sys: &mut self.sys,
            rip: self.cpu.last_rip,
            name: "",
            thr_self: 0,
            last_syscall: nr,
            last_args: a,
        };
        let res = sys::dispatch(&mut ctx, nr, a);
        let call_name = ctx.name;
        let _ = call_name;
        self.apply_thread_bases();
        let after = self.cpu.regs.rip;
        self.cpu.regs.gpr[1] = after; // rcx
        self.cpu.regs.gpr[11] = self.cpu.regs.rflags; // r11
        let linux = self.sys.abi == sys::Abi::Linux;
        match res {
            Ok(v) => {
                self.cpu.regs.gpr[0] = v;
                self.cpu.set_flag(crate::cpu::CF, false);
            }
            Err(errno) => {
                self.cpu.regs.gpr[0] = if linux {
                    (-(errno as i64)) as u64
                } else {
                    (errno as i64 as u64) & 0xFFFF_FFFF
                };
                self.cpu.set_flag(crate::cpu::CF, !linux);
            }
        }
        if self.sys.log_enabled {
            let name = sys::syscall_name(nr);
            let msg = format!(
                "  -> {} = {:#x} (errno={})",
                name,
                self.cpu.regs.gpr[0],
                errno_of(&res)
            );
            self.sys.log(msg.clone());
            if std::env::var("PS4EMU_VERBOSE").is_ok() {
                eprintln!("[sys] {}", msg.trim_start());
            }
        }
    }

    /// Трассировка стека гостя по цепочке rbp (если включены фреймы).
    pub fn guest_backtrace(&mut self) -> String {
        let mut s = String::new();
        let mut bp = self.cpu.regs.gpr[5];
        for _ in 0..32 {
            if bp < 0x1000 || bp > STACK_TOP {
                break;
            }
            let ret = match self.mem.read_u64(bp + 8) {
                Ok(v) => v,
                Err(_) => break,
            };
            let name = self.symbols.resolve(ret).unwrap_or_else(|| String::from("?"));
            s.push_str(&format!("  {:#018x} {}\n", ret, name));
            let next = match self.mem.read_u64(bp) {
                Ok(v) => v,
                Err(_) => break,
            };
            if next <= bp {
                break;
            }
            bp = next;
        }
        s
    }

    /// Проверка «подозрительных» скачков RIP и арифметических результатов.
    /// Включается переменной окружения PS4EMU_SANITY=1 и помогает находить
    /// ошибки интерпретатора (а не гостевой программы).
    pub fn sanity_check(&self, from: u64, to: u64) {
        if std::env::var("PS4EMU_SANITY").is_err() {
            return;
        }
        let mut m = Memory::new();
        std::mem::swap(&mut m, &mut Memory::new());
        let _ = m;
        if to > 0x0000_8000_0000_0000 {
            eprintln!(
                "[sanity] RIP улетел в {:#x} (было {:#x}), rax={:#x}",
                to, from, self.cpu.regs.gpr[0]
            );
        }
    }

    /// Применить базы сегментов FS/GS, изменённые гостем (arch_prctl и др.).
    pub fn apply_thread_bases(&mut self) {
        if self.sys.fs_base != 0 {
            self.cpu.regs.fs_base = self.sys.fs_base;
        }
        if self.sys.gs_base != 0 {
            self.cpu.regs.gs_base = self.sys.gs_base;
        }
    }

    /// Запуск с подготовкой стека и печатью отчёта.
    pub fn execute(&mut self, args: &[String], env: &[(String, String)]) -> Result<RunResult, EmuError> {
        self.setup_stack(args, env)?;
        if let Some(m) = self.modules.last() {
            self.cpu.reset(m.entry, self.cpu.regs.gpr[4]);
        }
        let r = self.run();
        self.capture_last_frame();
        Ok(r)
    }

    /// Снять последний представленный гостём кадр в `Gpu::last_frame`
    /// (используется командой `screenshot` и `--window`).
    pub fn capture_last_frame(&mut self) -> bool {
        let (addr, w, h) = match self.sys.last_flip {
            Some(v) => v,
            None => return false,
        };
        let before = self.gpu.last_frame.is_some();
        self.gpu.capture_guest(&mut self.mem, addr, w, h, w * 4);
        // Считаем кадр только один раз для одного и того же буфера.
        if self.gpu.last_frame.is_some() && !before {
            self.gpu.frames += 1;
            self.gpu.flips.push(addr);
        }
        true
    }

    /// Отчёт о падении с разбором стека вызовов.
    pub fn fault_report(&mut self, err: &CpuError) -> String {
        let mut s = String::new();
        s.push_str("=== СБОЙ ЭМУЛЯЦИИ ===\n");
        s.push_str(&format!("Причина: {}\n", err));
        s.push_str(&format!("RIP: {:#018x}", self.cpu.last_rip));
        if let Some(sym) = self.symbols.resolve(self.cpu.last_rip) {
            s.push_str(&format!("  <{}>", sym));
        }
        s.push('\n');
        let names = ["rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi",
                     "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"];
        for (i, n) in names.iter().enumerate() {
            s.push_str(&format!("{:<4}={:#018x} ", n, self.cpu.regs.gpr[i]));
            if i % 4 == 3 {
                s.push('\n');
            }
        }
        s.push_str(&format!("rflags={:#x}\n", self.cpu.regs.rflags));
        s.push_str(&format!(
            "Последний HLE: {}\nПоследний syscall: {} ({})\n",
            if self.last_hle.is_empty() { "-" } else { &self.last_hle },
            self.last_syscall,
            sys::syscall_name(self.last_syscall)
        ));
        s.push_str("--- стек вызовов ---\n");
        let mut sp = self.cpu.regs.gpr[4];
        let mut seen = 0;
        for _ in 0..2048 {
            let v = match self.mem.read_u64(sp) {
                Ok(v) => v,
                Err(_) => break,
            };
            sp += 8;
            if let Some(sym) = self.symbols.resolve(v) {
                s.push_str(&format!("  [{:#018x} -> {:#018x}] {}\n", sp - 8, v, sym));
                seen += 1;
                if seen >= 24 {
                    break;
                }
            }
        }
        if seen == 0 {
            s.push_str("  (не удалось восстановить стек)\n");
        }
        if self.tracer.enabled {
            s.push_str("--- последние инструкции ---\n");
            s.push_str(&self.tracer.dump(64));
        }
        s
    }

    /// Регионы памяти для команды `info mem`.
    pub fn memory_map(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "{:<18} {:<18} {:<6} {}\n",
            "начало", "конец", "права", "имя"
        ));
        for r in self.mem.regions() {
            s.push_str(&format!(
                "{:#018x} {:#018x} {:<6} {}\n",
                r.base,
                r.end(),
                crate::mem::prot_str(r.prot),
                r.name
            ));
        }
        s
    }

    pub fn stats_report(&self) -> String {
        let mut s = String::new();
        s.push_str("=== СТАТИСТИКА ===\n");
        s.push_str(&format!("Инструкций: {}\n", self.cpu.stats.total));
        s.push_str(&format!(
            "Чтений/записей памяти: {} / {}\n",
            self.mem.stats.reads, self.mem.stats.writes
        ));
        s.push_str(&format!("Ошибок доступа: {}\n", self.mem.stats.faults));
        s.push_str(&format!("Вызовов HLE: {}\n", self.hle.calls.values().sum::<u64>()));
        s.push_str(&format!("Нереализованных вызовов: {}\n", self.unsupported_calls));
        if !self.hle.missing.is_empty() {
            s.push_str("Не реализованы:\n");
            for m in self.hle.missing.iter().take(40) {
                s.push_str(&format!("  - {}\n", m));
            }
        }
        let mut top: Vec<(&String, &u64)> = self.hle.calls.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1));
        if !top.is_empty() {
            s.push_str("Часто вызываемые HLE-функции:\n");
            for (n, c) in top.iter().take(12) {
                s.push_str(&format!("  {:<40} {}\n", n, c));
            }
        }
        s
    }
}

/// Объединить и применить права доступа постранично.
fn apply_page_perms(mem: &mut Memory, perms: &[(u64, u64, u32)]) -> Result<(), EmuError> {
    let mut pages: BTreeMap<u64, (u32, u64)> = BTreeMap::new();
    for (base, size, prot) in perms {
        let mut p = *base;
        while p < base + size {
            let e = pages.entry(p).or_insert((0, PAGE));
            e.0 |= *prot;
            p += PAGE;
        }
    }
    // Склеиваем подряд идущие страницы с одинаковыми правами.
    let mut run_start = 0u64;
    let mut run_prot = 0u32;
    let mut run_len = 0u64;
    for (p, (prot, _)) in pages.iter() {
        if run_len > 0 && (*p == run_start + run_len) && *prot == run_prot {
            run_len += PAGE;
            continue;
        }
        if run_len > 0 {
            mem.protect(run_start, run_len, run_prot | P_READ)?;
        }
        run_start = *p;
        run_prot = *prot;
        run_len = PAGE;
    }
    if run_len > 0 {
        mem.protect(run_start, run_len, run_prot | P_READ)?;
    }
    Ok(())
}

/// Значения TLS по умолчанию (если у модуля нет PT_TLS).
fn emu_tls_default(_mem: &mut Memory) {}

fn errno_of(r: &sys::SysResult) -> i32 {
    match r {
        Ok(_) => 0,
        Err(e) => *e,
    }
}

fn short_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Распаковать SELF/ELF-контейнер в чистый ELF.
pub fn unwrap_container(raw: &[u8]) -> Result<Vec<u8>, EmuError> {
    if raw.len() >= 4 && &raw[0..4] == b"\x7fELF" {
        return Ok(raw.to_vec());
    }
    // SELF: 0x4F 0x15 0x3D 0x1D
    if raw.len() >= 32 && raw[0..4] == [0x4F, 0x15, 0x3D, 0x1D] {
        // Ищем встроенный ELF-заголовок.
        for i in 4..raw.len().saturating_sub(64).min(0x10000) {
            if &raw[i..i + 4] == b"\x7fELF" {
                let rest = &raw[i..];
                let end = find_last_load_end(rest).unwrap_or(rest.len());
                return Ok(rest[..end.min(rest.len())].to_vec());
            }
        }
        return Err(EmuError::Elf(
            "SELF-контейнер: не удалось найти встроенный ELF (сжатые сегменты не поддерживаются)".into(),
        ));
    }
    Err(EmuError::Elf("неизвестный формат файла (ожидается ELF или SELF)".into()))
}

fn find_last_load_end(data: &[u8]) -> Option<usize> {
    let e = elf::parse(data).ok()?;
    let mut end = 0usize;
    for ph in &e.phdrs {
        if ph.p_type == elf::PT_LOAD {
            end = end.max((ph.offset + ph.filesz) as usize);
        }
    }
    Some(end)
}
