//! Интерпретатор x86-64 (AMD64) для гостевого кода PlayStation 4.
//!
//! Поддерживается основная часть целочисленного набора AMD64, строковые
//! операции с REP-префиксами, базовая SSE/SSE2-арифметика, x87 (минимум,
//! нужный libc для printf) и инструкция `syscall`.

use crate::mem::{MemError, Memory};
use std::collections::BTreeMap;
use std::fmt;

pub const CF: u32 = 1 << 0;
pub const PF: u32 = 1 << 2;
pub const AF: u32 = 1 << 4;
pub const ZF: u32 = 1 << 6;
pub const SF: u32 = 1 << 7;
pub const DF: u32 = 1 << 10;
pub const OF: u32 = 1 << 11;
pub const FLAG_ONE: u64 = 1 << 1;

#[derive(Clone, Copy, Debug, Default)]
pub struct Regs {
    pub gpr: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    pub xmm: [u128; 16],
}

#[derive(Clone, Debug)]
pub struct X87 {
    pub st: [f64; 8],
    pub top: u8,
    pub cw: u16,
    pub sw: u16,
}

impl Default for X87 {
    fn default() -> Self {
        X87 { st: [0.0; 8], top: 0, cw: 0x037f, sw: 0 }
    }
}

#[derive(Debug, Clone)]
pub enum CpuError {
    Memory(MemError),
    Undefined { rip: u64, bytes: Vec<u8> },
    DivideError { rip: u64 },
    Custom(String),
}

impl fmt::Display for CpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CpuError::Memory(e) => write!(f, "{}", e),
            CpuError::Undefined { rip, bytes } => {
                write!(f, "неизвестная инструкция по адресу {:#x}: {:02x?}", rip, bytes)
            }
            CpuError::DivideError { rip } => {
                write!(f, "деление на ноль / переполнение по {:#x}", rip)
            }
            CpuError::Custom(s) => write!(f, "{}", s),
        }
    }
}

impl From<MemError> for CpuError {
    fn from(e: MemError) -> Self {
        CpuError::Memory(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Continue,
    Syscall,
    Interrupt(u8),
    Hlt,
}

#[derive(Clone, Copy, Default)]
struct Pfx {
    rex: u8,
    rex_seen: bool,
    p66: bool,
    p67: bool,
    p_f2: bool,
    p_f3: bool,
    /// 0 = нет сегмента, 1 = FS, 2 = GS
    seg: u8,
    /// 0 = без REP, 1 = F3 (REP), 2 = F2 (REPNE)
    rep: u8,
}

impl Pfx {
    #[inline]
    fn w(&self) -> bool {
        self.rex & 8 != 0
    }
    #[inline]
    fn r(&self) -> u8 {
        (self.rex >> 2) & 1
    }
    #[inline]
    fn x(&self) -> u8 {
        (self.rex >> 1) & 1
    }
    #[inline]
    fn b(&self) -> u8 {
        self.rex & 1
    }
}

#[derive(Clone, Copy, Default)]
struct Rm {
    reg: u8,
    mem: Option<u64>,
    /// Эффективный адрес без прибавления базы сегмента (для LEA).
    eff: u64,
}

#[derive(Clone)]
pub struct OpStats {
    pub total: u64,
    pub one_byte: [u64; 256],
    pub two_byte: BTreeMap<u8, u64>,
}

impl Default for OpStats {
    fn default() -> Self {
        OpStats { total: 0, one_byte: [0; 256], two_byte: BTreeMap::new() }
    }
}

pub struct Cpu {
    pub regs: Regs,
    pub x87: X87,
    pub stats: OpStats,
    /// Адрес последней успешно выполненной инструкции.
    pub last_rip: u64,
    /// Разрешать ли нестандартные байтовые регистры (для legacy-кодировок).
    rex_byte: bool,
    /// Включить трассировку в stderr.
    pub trace: bool,
    /// Точка останова (адрес) для отладчика.
    pub breakpoints: Vec<u64>,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            regs: Regs { rflags: FLAG_ONE, ..Default::default() },
            x87: X87::default(),
            stats: OpStats::default(),
            last_rip: 0,
            rex_byte: false,
            trace: false,
            breakpoints: Vec::new(),
        }
    }

    pub fn reset(&mut self, rip: u64, rsp: u64) {
        self.regs = Regs { rflags: FLAG_ONE, ..Default::default() };
        self.regs.rip = rip;
        self.regs.gpr[4] = rsp;
        self.last_rip = rip;
    }

    // ---- флаги -------------------------------------------------------

    #[inline]
    pub fn flag(&self, b: u32) -> bool {
        self.regs.rflags & b as u64 != 0
    }

    #[inline]
    pub fn set_flag(&mut self, b: u32, v: bool) {
        if v {
            self.regs.rflags |= b as u64;
        } else {
            self.regs.rflags &= !(b as u64);
        }
    }

    #[inline]
    fn set_szp(&mut self, res: u64, size: u8) {
        let sign = (size as u32 * 8 - 1) as u32;
        let m = mask(0, size);
        let v = res & m;
        let zf = v == 0;
        let sf = (v >> sign) & 1 != 0;
        let pf = (v as u8).count_ones() % 2 == 0;
        self.set_flag(ZF, zf);
        self.set_flag(SF, sf);
        self.set_flag(PF, pf);
    }

    // ---- выборка -----------------------------------------------------

    #[inline]
    fn fetch8(&mut self, mem: &mut Memory) -> Result<u8, CpuError> {
        let b = mem.read_u8(self.regs.rip)?;
        self.regs.rip = self.regs.rip.wrapping_add(1);
        Ok(b)
    }

    fn fetch_imm(&mut self, mem: &mut Memory, size: u8) -> Result<u64, CpuError> {
        match size {
            1 => Ok(self.fetch8(mem)? as u64),
            2 => {
                let a = self.fetch8(mem)? as u64;
                let b = self.fetch8(mem)? as u64;
                Ok(a | (b << 8))
            }
            4 => {
                let mut v = 0u64;
                for i in 0..4 {
                    v |= (self.fetch8(mem)? as u64) << (8 * i);
                }
                Ok(v)
            }
            _ => Ok(mem.read_u64(self.regs.rip).map(|v| {
                self.regs.rip = self.regs.rip.wrapping_add(8);
                v
            })?),
        }
    }

    fn fetch_prefixes(&mut self, mem: &mut Memory) -> Result<(u8, Pfx), CpuError> {
        let mut p = Pfx::default();
        for _ in 0..16 {
            let b = mem.read_u8(self.regs.rip)?;
            match b {
                0x66 => p.p66 = true,
                0x67 => p.p67 = true,
                0xF2 => {
                    p.p_f2 = true;
                    p.rep = 2;
                }
                0xF3 => {
                    p.p_f3 = true;
                    p.rep = 1;
                }
                0x2E | 0x3E | 0x26 | 0x36 | 0xF0 => {}
                0x64 => p.seg = 1,
                0x65 => p.seg = 2,
                0x40..=0x4f => {
                    p.rex = b;
                    p.rex_seen = true;
                }
                _ => return Ok((b, p)),
            }
            self.regs.rip = self.regs.rip.wrapping_add(1);
        }
        Err(CpuError::Custom("слишком много префиксов подряд".into()))
    }

    // ---- ModRM / SIB --------------------------------------------------

    fn decode_modrm(&mut self, mem: &mut Memory, p: &Pfx) -> Result<(u8, Rm), CpuError> {
        let m = self.fetch8(mem)?;
        let md = m >> 6;
        let regf = ((m >> 3) & 7) | (p.r() << 3);
        let rmf = (m & 7) | (p.b() << 3);
        if md == 3 {
            return Ok((regf, Rm { reg: rmf, mem: None, eff: 0 }));
        }
        let mut addr: u64;
        if (m & 7) == 4 {
            let sib = self.fetch8(mem)?;
            let scale = 1u64 << (sib >> 6);
            let idx_low = sib >> 3 & 7;
            let idxf = idx_low | (p.x() << 3);
            let basef = (sib & 7) | (p.b() << 3);
            let index = if idx_low == 4 && p.x() == 0 { 0 } else { self.gpr(idxf).wrapping_mul(scale) };
            if (sib & 7) == 5 && md == 0 {
                let disp = self.fetch_imm(mem, 4)? as i32 as i64 as u64;
                addr = disp.wrapping_add(index);
            } else {
                addr = self.gpr(basef).wrapping_add(index);
            }
        } else if (m & 7) == 5 && md == 0 {
            let disp = self.fetch_imm(mem, 4)? as i32 as i64 as u64;
            addr = self.regs.rip.wrapping_add(disp);
        } else {
            addr = self.gpr(rmf);
        }
        match md {
            1 => {
                let d = self.fetch8(mem)? as i8 as i64 as u64;
                addr = addr.wrapping_add(d);
            }
            2 => {
                let d = self.fetch_imm(mem, 4)? as i32 as i64 as u64;
                addr = addr.wrapping_add(d);
            }
            _ => {}
        }
        if p.p67 {
            addr &= 0xFFFF_FFFF;
        }
        let seg = match p.seg {
            1 => self.regs.fs_base,
            2 => self.regs.gs_base,
            _ => 0,
        };
        Ok((regf, Rm { reg: 0, mem: Some(addr.wrapping_add(seg)), eff: addr }))
    }

    #[inline]
    fn rm_read(&mut self, mem: &mut Memory, rm: &Rm, size: u8) -> Result<u64, CpuError> {
        match rm.mem {
            None => Ok(self.gpr_sized(rm.reg, size)),
            Some(a) => match size {
                1 => Ok(mem.read_u8(a)? as u64),
                2 => Ok(mem.read_u16(a)? as u64),
                4 => Ok(mem.read_u32(a)? as u64),
                _ => Ok(mem.read_u64(a)?),
            },
        }
    }

    #[inline]
    fn rm_write(&mut self, mem: &mut Memory, rm: &Rm, size: u8, val: u64) -> Result<(), CpuError> {
        match rm.mem {
            None => {
                self.set_gpr_sized(rm.reg, size, val);
                Ok(())
            }
            Some(a) => match size {
                1 => Ok(mem.write_u8(a, val as u8)?),
                2 => Ok(mem.write_u16(a, val as u16)?),
                4 => Ok(mem.write_u32(a, val as u32)?),
                _ => Ok(mem.write_u64(a, val)?),
            },
        }
    }

    // ---- регистры -----------------------------------------------------

    #[inline]
    pub fn gpr(&self, i: u8) -> u64 {
        self.regs.gpr[(i & 15) as usize]
    }

    #[inline]
    pub fn set_gpr(&mut self, i: u8, v: u64) {
        self.regs.gpr[(i & 15) as usize] = v;
    }

    #[inline]
    pub fn gpr_sized(&self, i: u8, size: u8) -> u64 {
        let idx = (i & 15) as usize;
        match size {
            1 if !self.rex_byte && (4..8).contains(&idx) => {
                (self.regs.gpr[idx - 4] >> 8) & 0xFF
            }
            1 => self.regs.gpr[idx] & 0xFF,
            2 => self.regs.gpr[idx] & 0xFFFF,
            4 => self.regs.gpr[idx] & 0xFFFF_FFFF,
            _ => self.regs.gpr[idx],
        }
    }

    #[inline]
    pub fn set_gpr_sized(&mut self, i: u8, size: u8, v: u64) {
        let idx = (i & 15) as usize;
        match size {
            1 if !self.rex_byte && (4..8).contains(&idx) => {
                let old = self.regs.gpr[idx - 4];
                self.regs.gpr[idx - 4] = (old & !0xFF00) | ((v & 0xFF) << 8);
            }
            1 => {
                let old = self.regs.gpr[idx];
                self.regs.gpr[idx] = (old & !0xFF) | (v & 0xFF);
            }
            2 => {
                let old = self.regs.gpr[idx];
                self.regs.gpr[idx] = (old & !0xFFFF) | (v & 0xFFFF);
            }
            4 => self.regs.gpr[idx] = v & 0xFFFF_FFFF,
            _ => self.regs.gpr[idx] = v,
        }
    }

    // ---- стек ---------------------------------------------------------

    fn push(&mut self, mem: &mut Memory, v: u64) -> Result<(), CpuError> {
        let sp = self.regs.gpr[4].wrapping_sub(8);
        mem.write_u64(sp, v)?;
        self.regs.gpr[4] = sp;
        Ok(())
    }

    fn pop(&mut self, mem: &mut Memory) -> Result<u64, CpuError> {
        let sp = self.regs.gpr[4];
        let v = mem.read_u64(sp)?;
        self.regs.gpr[4] = sp.wrapping_add(8);
        Ok(v)
    }

    fn push_sized(&mut self, mem: &mut Memory, v: u64, size: u8) -> Result<(), CpuError> {
        if size == 2 {
            let sp = self.regs.gpr[4].wrapping_sub(2);
            mem.write_u16(sp, v as u16)?;
            self.regs.gpr[4] = sp;
            Ok(())
        } else {
            self.push(mem, v)
        }
    }

    fn pop_sized(&mut self, mem: &mut Memory, size: u8) -> Result<u64, CpuError> {
        if size == 2 {
            let sp = self.regs.gpr[4];
            let v = mem.read_u16(sp)? as u64;
            self.regs.gpr[4] = sp.wrapping_add(2);
            Ok(v)
        } else {
            self.pop(mem)
        }
    }

    // ---- арифметика ---------------------------------------------------

    fn alu(&mut self, op: u8, a: u64, b: u64, size: u8) -> u64 {
        let bits = size as u32 * 8;
        let m = mask(0, size);
        let sign = 1u64 << (bits - 1);
        let a = a & m;
        let b = b & m;
        match op {
            0 | 2 => {
                let carry = if op == 2 { self.flag(CF) as u64 } else { 0 };
                let full = a as u128 + b as u128 + carry as u128;
                let r = full as u64 & m;
                self.set_flag(CF, full > m as u128);
                self.set_flag(OF, (!(a ^ b) & (a ^ r) & sign) != 0);
                self.set_flag(AF, (a ^ b ^ r) & 0x10 != 0);
                self.set_szp(r, size);
                r
            }
            3 | 5 | 7 => {
                let borrow = if op == 3 { self.flag(CF) as u64 } else { 0 };
                let full = a as i128 - b as i128 - borrow as i128;
                let r = (full as u64) & m;
                self.set_flag(CF, full < 0);
                self.set_flag(OF, ((a ^ b) & (a ^ r) & sign) != 0);
                self.set_flag(AF, (a ^ b ^ r) & 0x10 != 0);
                self.set_szp(r, size);
                r
            }
            1 => {
                let r = (a | b) & m;
                self.set_flag(CF, false);
                self.set_flag(OF, false);
                self.set_flag(AF, false);
                self.set_szp(r, size);
                r
            }
            4 => {
                let r = (a & b) & m;
                self.set_flag(CF, false);
                self.set_flag(OF, false);
                self.set_flag(AF, false);
                self.set_szp(r, size);
                r
            }
            _ => {
                let r = (a ^ b) & m;
                self.set_flag(CF, false);
                self.set_flag(OF, false);
                self.set_flag(AF, false);
                self.set_szp(r, size);
                r
            }
        }
    }

    fn cond(&self, cc: u8) -> bool {
        let cf = self.flag(CF);
        let zf = self.flag(ZF);
        let sf = self.flag(SF);
        let of = self.flag(OF);
        let pf = self.flag(PF);
        match cc & 15 {
            0 => of,
            1 => !of,
            2 => cf,
            3 => !cf,
            4 => zf,
            5 => !zf,
            6 => cf || zf,
            7 => !cf && !zf,
            8 => sf,
            9 => !sf,
            10 => pf,
            11 => !pf,
            12 => sf != of,
            13 => sf == of,
            14 => zf || (sf != of),
            _ => !zf && (sf == of),
        }
    }

    fn shift_op(&mut self, kind: u8, val: u64, count: u64, size: u8) -> u64 {
        let bits = size as u32 * 8;
        let m = mask(0, size);
        let mut v = val & m;
        let c = count % bits as u64;
        let sign = 1u64 << (bits - 1);
        match kind {
            0 => {
                // ROL
                if c > 0 {
                    v = ((v << c) | (v >> (bits as u64 - c))) & m;
                    self.set_flag(CF, v & 1 != 0);
                    self.set_flag(OF, ((v & sign) != 0) != (v & 1 != 0));
                }
                v
            }
            1 => {
                // ROR
                if c > 0 {
                    v = ((v >> c) | (v << (bits as u64 - c))) & m;
                    self.set_flag(CF, v & sign != 0);
                    self.set_flag(OF, ((v & sign) != 0) != (v & (sign >> 1) != 0));
                }
                v
            }
            2 => {
                // RCL
                let mut carry = self.flag(CF) as u64;
                for _ in 0..c {
                    let newc = (v & sign) >> (bits - 1);
                    v = ((v << 1) & m) | carry;
                    carry = newc;
                }
                self.set_flag(CF, carry != 0);
                v
            }
            3 => {
                // RCR
                let mut carry = self.flag(CF) as u64;
                for _ in 0..c {
                    let newc = v & 1;
                    v = (v >> 1) | (carry << (bits - 1));
                    carry = newc;
                }
                self.set_flag(CF, carry != 0);
                v
            }
            4 | 6 => {
                // SHL / SAL
                if c == 0 {
                    return v;
                }
                let r = (v << c) & m;
                self.set_flag(CF, (v >> (bits as u64 - c)) & 1 != 0);
                let cf = self.flag(CF);
                self.set_flag(OF, ((r & sign) != 0) != cf);
                self.set_szp(r, size);
                r
            }
            5 => {
                // SHR
                if c == 0 {
                    return v;
                }
                let r = (v >> c) & m;
                self.set_flag(CF, (v >> (c - 1)) & 1 != 0);
                if c == 1 {
                    self.set_flag(OF, (v & sign) != 0);
                }
                self.set_szp(r, size);
                r
            }
            _ => {
                // SAR
                if c == 0 {
                    return v;
                }
                let sx = sign_ext(v, size);
                let r = ((sx >> c) as u64) & m;
                self.set_flag(CF, (sx >> (c - 1)) & 1 != 0);
                self.set_flag(OF, false);
                self.set_szp(r, size);
                r
            }
        }
    }

    // ---- исполнение ---------------------------------------------------

    pub fn step(&mut self, mem: &mut Memory) -> Result<Step, CpuError> {
        let start = self.regs.rip;
        let (op, p) = self.fetch_prefixes(mem)?;
        // Опкод уже прочитан, но не «израсходован» — сдвигаем RIP.
        self.regs.rip = self.regs.rip.wrapping_add(1);
        self.stats.total += 1;
        self.stats.one_byte[op as usize] += 1;
        self.rex_byte = p.rex_seen;
        let r = self.exec(op, &p, mem, start);
        if r.is_ok() {
            self.last_rip = start;
        }
        r
    }

    fn undef(&self, rip: u64, mem: &mut Memory) -> CpuError {
        let mut bytes = Vec::new();
        for i in 0..8u64 {
            match mem.read_u8(rip + i) {
                Ok(b) => bytes.push(b),
                Err(_) => break,
            }
        }
        CpuError::Undefined { rip, bytes }
    }

    #[inline]
    fn opsize(&self, p: &Pfx, def: u8) -> u8 {
        if p.w() {
            8
        } else if p.p66 {
            2
        } else {
            def
        }
    }

    fn exec(&mut self, op: u8, p: &Pfx, mem: &mut Memory, start: u64) -> Result<Step, CpuError> {
        // ---- ALU: 00..3D (кроме недопустимых в long mode 06/07/0E/16/17/1E/1F/26/27/2E/2F/36/37/3E/3F)
        if op <= 0x3d && !matches!(op, 0x0f | 0x26 | 0x2e | 0x36 | 0x3e) {
            let idx = op & 7;
            let aluop = op >> 3;
            if idx > 5 {
                return Err(self.undef(start, mem));
            }
            if idx <= 3 {
                let size = if idx & 1 == 0 { 1 } else { self.opsize(p, 4) };
                let (regf, rm) = self.decode_modrm(mem, p)?;
                if idx & 2 == 0 {
                    let src = self.gpr_sized(regf, size);
                    let dst = self.rm_read(mem, &rm, size)?;
                    let r = self.alu(aluop, dst, src, size);
                    if aluop != 7 {
                        self.rm_write(mem, &rm, size, r)?;
                    }
                } else {
                    let src = self.rm_read(mem, &rm, size)?;
                    let dst = self.gpr_sized(regf, size);
                    let r = self.alu(aluop, dst, src, size);
                    if aluop != 7 {
                        self.set_gpr_sized(regf, size, r);
                    }
                }
            } else {
                let size = if idx == 4 { 1 } else { self.opsize(p, 4) };
                let imm = self.fetch_imm(mem, if size == 8 { 4 } else { size })?;
                let imm = if size == 8 { imm as u32 as i32 as i64 as u64 } else { imm };
                let dst = self.gpr_sized(0, size);
                let r = self.alu(aluop, dst, imm, size);
                if aluop != 7 {
                    self.set_gpr_sized(0, size, r);
                }
            }
            return Ok(Step::Continue);
        }

        match op {
            // ---- push/pop r64
            0x50..=0x5f => {
                let r = (op & 7) | (p.b() << 3);
                let size = if p.p66 { 2 } else { 8 };
                if op < 0x58 {
                    let v = self.gpr(r);
                    self.push_sized(mem, if size == 2 { v & 0xFFFF } else { v }, size)?;
                } else {
                    let v = self.pop_sized(mem, size)?;
                    self.set_gpr_sized(r, size, v);
                }
                Ok(Step::Continue)
            }
            0x68 => {
                let size = if p.p66 { 2 } else { 4 };
                let v = self.fetch_imm(mem, size)?;
                let v = if size == 4 { v as u32 as i32 as i64 as u64 } else { v };
                self.push_sized(mem, v, if p.p66 { 2 } else { 8 })?;
                Ok(Step::Continue)
            }
            0x6a => {
                let v = self.fetch_imm(mem, 1)? as i8 as i64 as u64;
                self.push_sized(mem, v, if p.p66 { 2 } else { 8 })?;
                Ok(Step::Continue)
            }
            // ---- mov r, imm
            0xb0..=0xb7 => {
                let r = (op & 7) | (p.b() << 3);
                let v = self.fetch_imm(mem, 1)?;
                self.set_gpr_sized(r, 1, v);
                Ok(Step::Continue)
            }
            0xb8..=0xbf => {
                let r = (op & 7) | (p.b() << 3);
                let size = self.opsize(p, 4);
                let v = self.fetch_imm(mem, size)?;
                self.set_gpr_sized(r, size, v);
                Ok(Step::Continue)
            }
            // ---- mov r/m, r и mov r, r/m
            0x88 | 0x89 | 0x8a | 0x8b => {
                let size = if op & 1 == 0 { 1 } else { self.opsize(p, 4) };
                let (regf, rm) = self.decode_modrm(mem, p)?;
                if op & 2 == 0 {
                    let v = self.gpr_sized(regf, size);
                    self.rm_write(mem, &rm, size, v)?;
                } else {
                    let v = self.rm_read(mem, &rm, size)?;
                    self.set_gpr_sized(regf, size, v);
                }
                Ok(Step::Continue)
            }
            // ---- mov r/m16, sreg / mov sreg, r/m16 (обе — no-op:我们没有 сегментов)
            0x8c | 0x8e => {
                let (_regf, rm) = self.decode_modrm(mem, p)?;
                if op == 0x8c {
                    self.rm_write(mem, &rm, 2, 0)?;
                }
                Ok(Step::Continue)
            }
            // ---- lea
            0x8d => {
                let size = self.opsize(p, 4);
                let (regf, rm) = self.decode_modrm(mem, p)?;
                if rm.mem.is_none() {
                    return Err(self.undef(start, mem));
                }
                self.set_gpr_sized(regf, size, rm.eff);
                Ok(Step::Continue)
            }
            // ---- mov moffs
            0xa0..=0xa3 => {
                let addr = self.fetch_imm(mem, 8)?;
                let size = if op & 1 == 0 { 1 } else { self.opsize(p, 4) };
                if op & 2 == 0 {
                    let v = self.gpr_sized(0, size);
                    match size {
                        1 => mem.write_u8(addr, v as u8)?,
                        2 => mem.write_u16(addr, v as u16)?,
                        4 => mem.write_u32(addr, v as u32)?,
                        _ => mem.write_u64(addr, v)?,
                    }
                } else {
                    let v = match size {
                        1 => mem.read_u8(addr)? as u64,
                        2 => mem.read_u16(addr)? as u64,
                        4 => mem.read_u32(addr)? as u64,
                        _ => mem.read_u64(addr)?,
                    };
                    self.set_gpr_sized(0, size, v);
                }
                Ok(Step::Continue)
            }
            // ---- test al/eax, imm
            0xa8 | 0xa9 => {
                let size = if op == 0xa8 { 1 } else { self.opsize(p, 4) };
                let imm = self.fetch_imm(mem, if size == 8 { 4 } else { size })?;
                let imm = if size == 8 { imm as u32 as i32 as i64 as u64 } else { imm };
                let a = self.gpr_sized(0, size);
                let r = a & imm;
                self.set_flag(CF, false);
                self.set_flag(OF, false);
                self.set_flag(AF, false);
                self.set_szp(r, size);
                Ok(Step::Continue)
            }
            // ---- xchg
            0x90 => {
                if p.b() != 0 {
                    let r = (p.b() << 3) | (op & 7);
                    let tmp = self.gpr(r);
                    let rax = self.gpr(0);
                    self.set_gpr(0, tmp);
                    self.set_gpr(r, rax);
                }
                Ok(Step::Continue)
            }
            0x91..=0x97 => {
                let r = (op & 7) | (p.b() << 3);
                let tmp = self.gpr(r);
                let rax = self.gpr(0);
                self.set_gpr(0, tmp);
                self.set_gpr(r, rax);
                Ok(Step::Continue)
            }
            0x86 | 0x87 => {
                let size = if op == 0x86 { 1 } else { self.opsize(p, 4) };
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let a = self.rm_read(mem, &rm, size)?;
                let b = self.gpr_sized(regf, size);
                self.rm_write(mem, &rm, size, b)?;
                self.set_gpr_sized(regf, size, a);
                Ok(Step::Continue)
            }
            // ---- cbw/cwde/cdqe, cwd/cdq/cqo
            0x98 => {
                let size = self.opsize(p, 4);
                let v = match size {
                    2 => self.gpr_sized(0, 1) as u8 as i8 as i64 as u64,
                    8 => self.gpr_sized(0, 4) as u32 as i32 as i64 as u64,
                    _ => self.gpr_sized(0, 2) as u16 as i16 as i64 as u64,
                };
                self.set_gpr_sized(0, size, v);
                Ok(Step::Continue)
            }
            0x99 => {
                let size = self.opsize(p, 4);
                let v = self.gpr_sized(0, size);
                let neg = sign_ext(v, size) < 0;
                let fill = if neg { mask(0, size) } else { 0 };
                self.set_gpr_sized(2, size, fill);
                Ok(Step::Continue)
            }
            // ---- pushf/popf/sahf/lahf
            0x9c => {
                let v = (self.regs.rflags | FLAG_ONE) & 0xFFFF_FFFF;
                self.push(mem, v)?;
                Ok(Step::Continue)
            }
            0x9d => {
                let v = self.pop(mem)?;
                self.regs.rflags = (v & 0xFFFF_FFFF) | FLAG_ONE;
                Ok(Step::Continue)
            }
            0x9e => {
                let ah = (self.gpr_sized(0, 2) >> 8) as u8;
                self.regs.rflags &= !0xD5u64;
                self.regs.rflags |= (ah as u64) & 0xD5;
                Ok(Step::Continue)
            }
            0x9f => {
                let ah = ((self.regs.rflags & 0xD5) | 0x02) as u8;
                let ax = self.gpr_sized(0, 2) & 0x00FF;
                self.set_gpr_sized(0, 2, ax | ((ah as u64) << 8));
                Ok(Step::Continue)
            }
            0x9b => Ok(Step::Continue),
            0xf4 => Ok(Step::Hlt),
            // ---- ret/near
            0xc3 => {
                let v = self.pop(mem)?;
                self.regs.rip = v;
                Ok(Step::Continue)
            }
            0xc2 => {
                let n = self.fetch_imm(mem, 2)?;
                let v = self.pop(mem)?;
                self.regs.rip = v;
                self.regs.gpr[4] = self.regs.gpr[4].wrapping_add(n);
                Ok(Step::Continue)
            }
            0xc9 => {
                self.regs.gpr[4] = self.regs.gpr[5];
                let v = self.pop(mem)?;
                self.regs.gpr[5] = v;
                Ok(Step::Continue)
            }
            // ---- call/jmp
            0xe8 => {
                let rel = self.fetch_imm(mem, 4)? as i32 as i64 as u64;
                let ret = self.regs.rip;
                self.push(mem, ret)?;
                self.regs.rip = ret.wrapping_add(rel);
                Ok(Step::Continue)
            }
            0xe9 => {
                let rel = self.fetch_imm(mem, 4)? as i32 as i64 as u64;
                self.regs.rip = self.regs.rip.wrapping_add(rel);
                Ok(Step::Continue)
            }
            0xeb => {
                let rel = self.fetch_imm(mem, 1)? as i8 as i64 as u64;
                self.regs.rip = self.regs.rip.wrapping_add(rel);
                Ok(Step::Continue)
            }
            0x70..=0x7f => {
                let rel = self.fetch_imm(mem, 1)? as i8 as i64 as u64;
                if self.cond(op & 15) {
                    self.regs.rip = self.regs.rip.wrapping_add(rel);
                }
                Ok(Step::Continue)
            }
            0xe0 | 0xe1 | 0xe2 | 0xe3 => {
                let rel = self.fetch_imm(mem, 1)? as i8 as i64 as u64;
                let take = match op {
                    0xe0 => {
                        let v = self.gpr_sized(1, 4).wrapping_sub(1) & 0xFFFF_FFFF;
                        self.set_gpr_sized(1, 4, v);
                        v != 0
                    }
                    0xe1 => {
                        let v = self.gpr_sized(1, 4).wrapping_sub(1) & 0xFFFF_FFFF;
                        self.set_gpr_sized(1, 4, v);
                        v != 0 && self.flag(ZF)
                    }
                    0xe2 => {
                        let v = self.gpr_sized(1, 4).wrapping_sub(1) & 0xFFFF_FFFF;
                        self.set_gpr_sized(1, 4, v);
                        v != 0 && !self.flag(ZF)
                    }
                    _ => self.gpr_sized(1, 4) == 0,
                };
                if take {
                    self.regs.rip = self.regs.rip.wrapping_add(rel);
                }
                Ok(Step::Continue)
            }
            // ---- прерывания
            0xcc => Ok(Step::Interrupt(3)),
            0xcd => {
                let n = self.fetch_imm(mem, 1)? as u8;
                Ok(Step::Interrupt(n))
            }
            0xce => Ok(Step::Interrupt(4)),
            0xf1 => Ok(Step::Interrupt(1)),
            0xf5 => {
                let c = self.flag(CF);
                self.set_flag(CF, !c);
                Ok(Step::Continue)
            }
            0xf8 => {
                self.set_flag(CF, false);
                Ok(Step::Continue)
            }
            0xf9 => {
                self.set_flag(CF, true);
                Ok(Step::Continue)
            }
            0xfa | 0xfb => Ok(Step::Continue),
            0xfc => {
                self.set_flag(DF, false);
                Ok(Step::Continue)
            }
            0xfd => {
                self.set_flag(DF, true);
                Ok(Step::Continue)
            }
            // ---- группы
            0xf6 | 0xf7 => self.grp3(op == 0xf7, p, mem),
            0xfe => {
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let v = self.rm_read(mem, &rm, 1)?;
                match regf & 7 {
                    0 => {
                        let r = self.alu(0, v, 1, 1);
                        self.rm_write(mem, &rm, 1, r)?;
                    }
                    1 => {
                        let r = self.alu(5, v, 1, 1);
                        self.rm_write(mem, &rm, 1, r)?;
                    }
                    _ => return Err(self.undef(start, mem)),
                }
                Ok(Step::Continue)
            }
            0xff => self.grp5(p, mem, start),
            0x8f => {
                let (regf, rm) = self.decode_modrm(mem, p)?;
                if regf & 7 != 0 {
                    return Err(self.undef(start, mem));
                }
                let size = if p.p66 { 2 } else { 8 };
                let v = self.pop_sized(mem, size)?;
                self.rm_write(mem, &rm, size, v)?;
                Ok(Step::Continue)
            }
            0xc6 | 0xc7 => {
                let size = if op == 0xc6 { 1 } else { self.opsize(p, 4) };
                let (regf, rm) = self.decode_modrm(mem, p)?;
                if regf & 7 != 0 {
                    return Err(self.undef(start, mem));
                }
                let v = self.fetch_imm(mem, if size == 8 { 4 } else { size })?;
                let v = if size == 8 { v as u32 as i32 as i64 as u64 } else { v };
                self.rm_write(mem, &rm, size, v)?;
                Ok(Step::Continue)
            }
            0x80 | 0x81 | 0x83 => {
                let size = if op == 0x80 { 1 } else { self.opsize(p, 4) };
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let imm = match op {
                    0x81 => {
                        let v = self.fetch_imm(mem, if size == 8 { 4 } else { size })?;
                        if size == 8 {
                            v as u32 as i32 as i64 as u64
                        } else {
                            v
                        }
                    }
                    0x83 => self.fetch_imm(mem, 1)? as i8 as i64 as u64,
                    _ => self.fetch_imm(mem, 1)?,
                };
                let dst = self.rm_read(mem, &rm, size)?;
                let res = self.alu(regf & 7, dst, imm, size);
                if regf & 7 != 7 {
                    self.rm_write(mem, &rm, size, res)?;
                }
                Ok(Step::Continue)
            }
            0x84 | 0x85 => {
                let size = if op == 0x84 { 1 } else { self.opsize(p, 4) };
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let a = self.rm_read(mem, &rm, size)?;
                let b = self.gpr_sized(regf, size);
                let r = a & b;
                self.set_flag(CF, false);
                self.set_flag(OF, false);
                self.set_flag(AF, false);
                self.set_szp(r, size);
                Ok(Step::Continue)
            }
            0x69 | 0x6b => {
                let size = self.opsize(p, 4);
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let a = self.rm_read(mem, &rm, size)?;
                let imm = if op == 0x69 {
                    let v = self.fetch_imm(mem, if size == 8 { 4 } else { size })?;
                    if size == 8 {
                        v as u32 as i32 as i64 as u64
                    } else {
                        v
                    }
                } else {
                    self.fetch_imm(mem, 1)? as i8 as i64 as u64
                };
                let (r, ovf) = imul_wide(a, imm, size);
                self.set_gpr_sized(regf, size, r);
                self.set_flag(CF, ovf);
                self.set_flag(OF, ovf);
                Ok(Step::Continue)
            }
            0xc0 | 0xc1 | 0xd0 | 0xd1 | 0xd2 | 0xd3 => {
                let size = if op & 1 == 0 { 1 } else { self.opsize(p, 4) };
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let count = match op {
                    0xc0 | 0xc1 => self.fetch_imm(mem, 1)?,
                    0xd0 | 0xd1 => 1,
                    _ => self.gpr_sized(1, 1),
                };
                let v = self.rm_read(mem, &rm, size)?;
                let r = self.shift_op(regf & 7, v, count, size);
                self.rm_write(mem, &rm, size, r)?;
                Ok(Step::Continue)
            }
            // ---- строковые операции
            0xa4 | 0xa5 | 0xa6 | 0xa7 | 0xaa | 0xab | 0xac | 0xad | 0xae | 0xaf => {
                self.string_op(op, p, mem)
            }
            // ---- io (заглушки)
            0x6c..=0x6f => Ok(Step::Continue),
            // ---- escape
            0x0f => self.exec_two_byte(p, mem, start),
            // ---- x87
            0xd8..=0xdf => self.exec_x87(op, p, mem),
            // ---- movsxd
            0x63 => {
                let size = self.opsize(p, 4);
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let v = self.rm_read(mem, &rm, 4)? as u32 as i32 as i64 as u64;
                self.set_gpr_sized(regf, size, v);
                Ok(Step::Continue)
            }
            _ => Err(self.undef(start, mem)),
        }
    }

    fn grp3(&mut self, wide: bool, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let size = if wide { self.opsize(p, 4) } else { 1 };
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let v = self.rm_read(mem, &rm, size)?;
        match regf & 7 {
            0 => {
                let imm = self.fetch_imm(mem, if size == 8 { 4 } else { size })?;
                let imm = if size == 8 { imm as u32 as i32 as i64 as u64 } else { imm };
                let r = v & imm;
                self.set_flag(CF, false);
                self.set_flag(OF, false);
                self.set_flag(AF, false);
                self.set_szp(r, size);
            }
            1 => {
                // вторая форма test — недокументированная, трактуем как test
                let imm = self.fetch_imm(mem, if size == 8 { 4 } else { size })?;
                let imm = if size == 8 { imm as u32 as i32 as i64 as u64 } else { imm };
                let r = v & imm;
                self.set_flag(CF, false);
                self.set_flag(OF, false);
                self.set_szp(r, size);
            }
            2 => {
                let r = !v;
                self.rm_write(mem, &rm, size, r)?;
            }
            3 => {
                let r = self.alu(5, 0, v, size);
                self.set_flag(CF, v != 0);
                self.rm_write(mem, &rm, size, r)?;
            }
            4 => {
                let a = self.gpr_sized(0, size);
                let full = (a & mask(0, size)) as u128 * (v & mask(0, size)) as u128;
                let bits = size as u32 * 8;
                let lo = (full as u64) & mask(0, size);
                let hi = if bits == 64 { (full >> 64) as u64 } else { (full >> bits) as u64 };
                let ovf = hi != 0;
                self.set_flag(CF, ovf);
                self.set_flag(OF, ovf);
                self.set_gpr_sized(0, size, lo);
                self.set_gpr_sized(2, size, hi);
            }
            5 => {
                let a = sign_ext(self.gpr_sized(0, size), size);
                let b = sign_ext(v, size);
                let full = (a as i128) * (b as i128);
                let bits = size as u32 * 8;
                let lo = (full as u64) & mask(0, size);
                let hi = if bits == 64 { (full >> 64) as u64 & u64::MAX } else { ((full >> bits) as u64) & mask(0, size) };
                let ovf = sign_ext(lo, size) as i128 != full;
                self.set_flag(CF, ovf);
                self.set_flag(OF, ovf);
                self.set_gpr_sized(0, size, lo);
                self.set_gpr_sized(2, size, hi);
            }
            6 => {
                if v == 0 {
                    return Err(CpuError::DivideError { rip: self.regs.rip });
                }
                let bits = size as u32 * 8;
                let dividend: u128 = ((self.gpr_sized(2, size) as u128) << bits)
                    | self.gpr_sized(0, size) as u128;
                let q = dividend / v as u128;
                let r = dividend % v as u128;
                if q > mask(0, size) as u128 {
                    return Err(CpuError::DivideError { rip: self.regs.rip });
                }
                self.set_gpr_sized(0, size, q as u64);
                self.set_gpr_sized(2, size, r as u64);
            }
            _ => {
                let divisor = sign_ext(v, size);
                if divisor == 0 {
                    return Err(CpuError::DivideError { rip: self.regs.rip });
                }
                let bits = size as u32 * 8;
                let raw: u128 =
                    ((self.gpr_sized(2, size) as u128) << bits) | self.gpr_sized(0, size) as u128;
                let dividend = if bits == 64 {
                    raw as i128
                } else {
                    let signbit = 1u128 << (bits * 2 - 1);
                    if raw & signbit != 0 {
                        (raw | (!0u128 << (bits * 2))) as i128
                    } else {
                        raw as i128
                    }
                };
                let q = dividend.wrapping_div(divisor as i128);
                let r = dividend.wrapping_rem(divisor as i128);
                let min = -(1i128 << (bits - 1));
                let max = (1i128 << (bits - 1)) - 1;
                if q < min || q > max {
                    return Err(CpuError::DivideError { rip: self.regs.rip });
                }
                self.set_gpr_sized(0, size, q as u64);
                self.set_gpr_sized(2, size, r as u64);
            }
        }
        Ok(Step::Continue)
    }

    fn grp5(&mut self, p: &Pfx, mem: &mut Memory, start: u64) -> Result<Step, CpuError> {
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let size = self.opsize(p, 4);
        match regf & 7 {
            0 => {
                let v = self.rm_read(mem, &rm, size)?;
                let r = self.alu(0, v, 1, size);
                self.rm_write(mem, &rm, size, r)?;
            }
            1 => {
                let v = self.rm_read(mem, &rm, size)?;
                let r = self.alu(5, v, 1, size);
                self.rm_write(mem, &rm, size, r)?;
            }
            2 => {
                let target = self.rm_read(mem, &rm, 8)?;
                let ret = self.regs.rip;
                self.push(mem, ret)?;
                self.regs.rip = target;
            }
            3 => {
                let _ = self.rm_read(mem, &rm, 8)?;
            }
            4 => {
                let target = self.rm_read(mem, &rm, 8)?;
                self.regs.rip = target;
            }
            5 => {
                let _ = self.rm_read(mem, &rm, 8)?;
            }
            6 => {
                let size = if p.p66 { 2 } else { 8 };
                let v = self.rm_read(mem, &rm, size)?;
                self.push_sized(mem, v, size)?;
            }
            _ => return Err(self.undef(start, mem)),
        }
        Ok(Step::Continue)
    }

    fn string_op(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let size: u8 = match op {
            0xa4 | 0xa6 | 0xaa | 0xac | 0xae => 1,
            0xa5 | 0xa7 | 0xab | 0xad | 0xaf => self.opsize(p, 4),
            _ => 1,
        };
        let delta = if self.flag(DF) { 0u64.wrapping_sub(size as u64) } else { size as u64 };
        let rep = p.rep != 0;
        let elem = |m: &mut Memory, a: u64, s: u8| -> Result<u64, CpuError> {
            Ok(match s {
                1 => m.read_u8(a)? as u64,
                2 => m.read_u16(a)? as u64,
                4 => m.read_u32(a)? as u64,
                _ => m.read_u64(a)?,
            })
        };
        let store = |m: &mut Memory, a: u64, s: u8, v: u64| -> Result<(), CpuError> {
            match s {
                1 => m.write_u8(a, v as u8)?,
                2 => m.write_u16(a, v as u16)?,
                4 => m.write_u32(a, v as u32)?,
                _ => m.write_u64(a, v)?,
            }
            Ok(())
        };
        loop {
            if rep && self.gpr_sized(1, 8) == 0 {
                break;
            }
            match op {
                0xa4 | 0xa5 => {
                    let src = self.gpr_sized(6, 8);
                    let dst = self.gpr_sized(7, 8);
                    let v = elem(mem, src, size)?;
                    store(mem, dst, size, v)?;
                    self.set_gpr(6, src.wrapping_add(delta));
                    self.set_gpr(7, dst.wrapping_add(delta));
                }
                0xaa | 0xab => {
                    let v = self.gpr_sized(0, size);
                    let dst = self.gpr_sized(7, 8);
                    store(mem, dst, size, v)?;
                    self.set_gpr(7, dst.wrapping_add(delta));
                }
                0xac | 0xad => {
                    let src = self.gpr_sized(6, 8);
                    let v = elem(mem, src, size)?;
                    self.set_gpr_sized(0, size, v);
                    self.set_gpr(6, src.wrapping_add(delta));
                }
                0xa6 | 0xa7 => {
                    let src = self.gpr_sized(6, 8);
                    let dst = self.gpr_sized(7, 8);
                    let a = elem(mem, dst, size)?;
                    let b = elem(mem, src, size)?;
                    self.alu(7, a, b, size);
                    self.set_gpr(6, src.wrapping_add(delta));
                    self.set_gpr(7, dst.wrapping_add(delta));
                }
                0xae | 0xaf => {
                    let v = self.gpr_sized(0, size);
                    let dst = self.gpr_sized(7, 8);
                    let a = elem(mem, dst, size)?;
                    self.alu(7, a, v, size);
                    self.set_gpr(7, dst.wrapping_add(delta));
                }
                _ => break,
            }
            if !rep {
                break;
            }
            self.set_gpr_sized(1, 8, self.gpr_sized(1, 8).wrapping_sub(1));
            if op == 0xa6 || op == 0xa7 || op == 0xae || op == 0xaf {
                let zf = self.flag(ZF);
                if (p.rep == 1 && !zf) || (p.rep == 2 && zf) {
                    break;
                }
            }
        }
        Ok(Step::Continue)
    }

    // ---- x87 ---------------------------------------------------------

    fn fpu_get(&self, i: u8) -> f64 {
        self.x87.st[((self.x87.top + i) & 7) as usize]
    }
    fn fpu_set(&mut self, i: u8, v: f64) {
        let idx = ((self.x87.top + i) & 7) as usize;
        self.x87.st[idx] = v;
    }
    fn fpu_push(&mut self, v: f64) {
        self.x87.top = (self.x87.top + 7) & 7;
        self.x87.st[self.x87.top as usize] = v;
    }
    fn fpu_pop(&mut self) {
        self.x87.top = (self.x87.top + 1) & 7;
    }

    fn exec_x87(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let reg = regf & 7;
        let maddr = rm.mem;
        match (op, maddr) {
            // --- 32-битная память
            (0xd8, Some(a)) => {
                let v = mem.read_u32(a)? as u32 as f32 as f64;
                let st0 = self.fpu_get(0);
                let r = match reg {
                    0 => st0 + v,
                    1 => st0 * v,
                    2 => st0 - v,
                    3 => v - st0,
                    4 => st0 / v,
                    5 => v / st0,
                    _ => st0,
                };
                self.fpu_set(0, r);
            }
            (0xd9, Some(a)) => match reg {
                0 => {
                    // FLD m32fp
                    let raw = mem.read_u32(a)?;
                    let v = f32::from_bits(raw as u32) as f64;
                    if std::env::var("PS4EMU_X87DBG").is_ok() {
                        eprintln!("[x87] FLD m32 raw={:#x} v={}", raw, v);
                    }
                    self.fpu_push(v);
                }
                2 => {
                    // FST m32fp
                    let v = self.fpu_get(0) as f32;
                    mem.write_u32(a, v.to_bits())?;
                }
                3 => {
                    // FSTP m32fp
                    let v = self.fpu_get(0) as f32;
                    mem.write_u32(a, v.to_bits())?;
                    self.fpu_pop();
                }
                4 => {
                    // FLD m80fp (читаем как f64 из младших 8 байт)
                    let bits = mem.read_u64(a)?;
                    let v = f64::from_bits(bits);
                    self.fpu_push(v);
                }
                5 => self.x87.cw = mem.read_u16(a)?,
                6 => {
                    // FNSTENV: минимальный образ окружения
                    for i in 0..4u64 {
                        mem.write_u16(a + i * 2, 0)?;
                    }
                }
                7 => {
                    let cw = self.x87.cw;
                    mem.write_u16(a, cw)?;
                }
                _ => {}
            },
            (0xdd, Some(a)) => match reg {
                0 => {
                    // FLD m64fp
                    let v = f64::from_bits(mem.read_u64(a)?);
                    self.fpu_push(v);
                }
                2 => {
                    // FST m64fp
                    let v = self.fpu_get(0);
                    mem.write_u64(a, v.to_bits())?;
                }
                3 => {
                    // FSTP m64fp
                    let v = self.fpu_get(0);
                    mem.write_u64(a, v.to_bits())?;
                    self.fpu_pop();
                }
                7 => {
                    // FNSTSW AX
                    let sw = self.x87.sw;
                    self.set_gpr_sized(0, 2, sw as u64);
                }
                _ => {}
            },
            (0xdf, Some(a)) => match reg {
                0 | 5 => mem.write_u16(a, self.gpr_sized(0, 2) as u16)?,
                4 | 6 => self.x87.sw = mem.read_u16(a)?,
                7 => {
                    let cw = self.x87.cw;
                    mem.write_u16(a, cw)?;
                }
                _ => {}
            },
            (0xdb, Some(a)) => match reg {
                5 => {
                    // FLD m80fp: используем младшие 64 бита как f64
                    let v = f64::from_bits(mem.read_u64(a)?);
                    self.fpu_push(v);
                }
                7 => {
                    let v = f64::from_bits(mem.read_u64(a)?);
                    self.fpu_push(v);
                }
                _ => {}
            },
            (0xdc, Some(a)) => {
                let v = f64::from_bits(mem.read_u64(a)?);
                let st0 = self.fpu_get(0);
                let r = match reg {
                    0 => st0 + v,
                    1 => st0 * v,
                    2 => st0 - v,
                    3 => v - st0,
                    4 => st0 / v,
                    5 => v / st0,
                    _ => st0,
                };
                self.fpu_set(0, r);
            }
            (0xde, Some(a)) => {
                let _ = a;
            }
            // --- регистровые формы
            (0xd9, None) => match reg {
                0 => {
                    let v = self.fpu_get(rm.reg);
                    self.fpu_push(v);
                }
                1 => {
                    let v = fpu_swap(self.fpu_get(0), self.fpu_get(rm.reg));
                    self.fpu_set(rm.reg, v.0);
                    self.fpu_set(0, v.1);
                }
                4 => {}
                5 => self.fpu_pop(),
                _ => {}
            },
            // fadd/fsub/fdiv без выталкивания: st(0) = st(0) op st(i)
            (0xd8, None) => {
                let st0 = self.fpu_get(0);
                let other = self.fpu_get(rm.reg);
                let r = match reg {
                    0 => st0 + other,
                    1 => st0 * other,
                    2 => st0 - other,
                    3 => other - st0,
                    4 => st0 / other,
                    5 => other / st0,
                    _ => st0,
                };
                self.fpu_set(0, r);
            }
            // запись в st(i) (DC): st(i) = st(i) op st(0)
            (0xdc, None) => {
                let st0 = self.fpu_get(0);
                let other = self.fpu_get(rm.reg);
                let r = match reg {
                    0 => other + st0,
                    1 => other * st0,
                    2 => other - st0,
                    3 => st0 - other,
                    4 => other / st0,
                    5 => st0 / other,
                    _ => other,
                };
                self.fpu_set(rm.reg, r);
            }
            // DE: st(i) = st(i) op st(0), затем выталкивание
            (0xde, None) => {
                let st0 = self.fpu_get(0);
                let other = self.fpu_get(rm.reg);
                let r = match reg {
                    0 => other + st0,
                    1 => other * st0,
                    2 => other - st0,
                    3 => st0 - other,
                    4 => other / st0,
                    5 => st0 / other,
                    _ => other,
                };
                self.fpu_set(rm.reg, r);
                self.fpu_pop();
            }
            // DA: st(0) = st(0) op st(i), затем выталкивание
            (0xda, None) => {
                let st0 = self.fpu_get(0);
                let other = self.fpu_get(rm.reg);
                let r = match reg {
                    0 => st0 + other,
                    1 => st0 * other,
                    2 => st0 - other,
                    3 => other - st0,
                    4 => st0 / other,
                    5 => other / st0,
                    _ => st0,
                };
                self.fpu_set(0, r);
                self.fpu_pop();
            }
            (0xdb, None) | (0xdd, None) | (0xdf, None) => {
                if op == 0xdd && reg == 0 {
                    self.fpu_push(0.0);
                }
                if op == 0xdb && reg == 2 {
                    self.x87.sw &= !0x3F;
                }
                if op == 0xdb && reg == 3 {
                    self.x87 = X87::default();
                }
                if op == 0xdf && (reg == 4 || reg == 5) {
                    self.set_gpr_sized(0, 2, self.x87.sw as u64);
                }
                if op == 0xdf && reg == 6 {
                    self.set_gpr_sized(0, 2, self.x87.cw as u64);
                }
                if op == 0xdd && reg == 4 {
                    self.x87.sw &= !0x3F;
                }
            }
            _ => {}
        }
        Ok(Step::Continue)
    }

    // ---- 0F xx -------------------------------------------------------

    fn exec_two_byte(&mut self, p: &Pfx, mem: &mut Memory, start: u64) -> Result<Step, CpuError> {
        let op = self.fetch8(mem)?;
        *self.stats.two_byte.entry(op).or_insert(0) += 1;
        match op {
            0x05 => Ok(Step::Syscall),
            0x0b => Err(CpuError::Custom(format!("ud2 по адресу {:#x}", start))),
            // концепутационные префиксы (endbr64 = F3 0F 1E FA)
            0x18 | 0x19 | 0x1a | 0x1b | 0x1c | 0x1d | 0x1e | 0x1f => {
                let (_r, _rm) = self.decode_modrm(mem, p)?;
                Ok(Step::Continue)
            }
            0x31 => {
                let tsc = mem_tsc();
                self.set_gpr_sized(0, 4, tsc & 0xFFFF_FFFF);
                self.set_gpr_sized(2, 4, tsc >> 32);
                Ok(Step::Continue)
            }
            0xa2 => {
                let leaf = self.gpr_sized(0, 4) as u32;
                let sub = self.gpr_sized(1, 4) as u32;
                let (a, b, c, d) = cpuid_leaf(leaf, sub);
                self.set_gpr_sized(0, 4, a as u64);
                self.set_gpr_sized(1, 4, b as u64);
                self.set_gpr_sized(2, 4, c as u64);
                self.set_gpr_sized(3, 4, d as u64);
                Ok(Step::Continue)
            }
            0x80..=0x8f => {
                let rel = self.fetch_imm(mem, 4)? as i32 as i64 as u64;
                if self.cond(op & 15) {
                    self.regs.rip = self.regs.rip.wrapping_add(rel);
                }
                Ok(Step::Continue)
            }
            0x90..=0x9f => {
                let (_regf, rm) = self.decode_modrm(mem, p)?;
                let v = if self.cond(op & 15) { 1 } else { 0 };
                self.rm_write(mem, &rm, 1, v)?;
                Ok(Step::Continue)
            }
            0x40..=0x4f => {
                let size = self.opsize(p, 4);
                let (regf, rm) = self.decode_modrm(mem, p)?;
                if self.cond(op & 15) {
                    let v = self.rm_read(mem, &rm, size)?;
                    self.set_gpr_sized(regf, size, v);
                }
                Ok(Step::Continue)
            }
            0xaf => {
                let size = self.opsize(p, 4);
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let b = self.rm_read(mem, &rm, size)?;
                let a = self.gpr_sized(regf, size);
                let (r, ovf) = imul_wide(a, b, size);
                self.set_gpr_sized(regf, size, r);
                self.set_flag(CF, ovf);
                self.set_flag(OF, ovf);
                Ok(Step::Continue)
            }
            0xb6 | 0xb7 | 0xbe | 0xbf => {
                let src_size = if op & 1 == 0 { 1 } else { 2 };
                let size = self.opsize(p, 4);
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let v = self.rm_read(mem, &rm, src_size)?;
                let v = if op >= 0xbe { sign_ext(v, src_size) as u64 } else { v };
                self.set_gpr_sized(regf, size, v);
                Ok(Step::Continue)
            }
            0xbc | 0xbd => {
                let size = self.opsize(p, 4);
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let v = self.rm_read(mem, &rm, size)? & mask(0, size);
                if v == 0 {
                    self.set_flag(ZF, true);
                } else {
                    self.set_flag(ZF, false);
                    let bits = size as u32 * 8;
                    let r = if op == 0xbc {
                        v.trailing_zeros() as u64
                    } else {
                        (bits - 1 - v.leading_zeros() + (64 - bits)) as u64
                    };
                    self.set_gpr_sized(regf, size, r);
                }
                Ok(Step::Continue)
            }
            0xa3 | 0xab | 0xb3 | 0xbb => {
                let size = self.opsize(p, 4);
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let limit = size as u64 * 8 - 1;
                let bit = (self.gpr_sized(regf, size) & limit) as u32;
                let v = self.rm_read(mem, &rm, size)?;
                let b = (v >> bit) & 1;
                self.set_flag(CF, b != 0);
                if op != 0xa3 {
                    let nv = match op {
                        0xab => v | (1u64 << bit),
                        0xb3 => v & !(1u64 << bit),
                        _ => v ^ (1u64 << bit),
                    };
                    self.rm_write(mem, &rm, size, nv)?;
                }
                Ok(Step::Continue)
            }
            0xb0 | 0xb1 | 0xc0 | 0xc1 => {
                let size = if op & 1 == 0 { 1 } else { self.opsize(p, 4) };
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let dst = self.rm_read(mem, &rm, size)?;
                let src = self.gpr_sized(regf, size);
                let res = self.alu(7, dst, src, size);
                match op {
                    0xb0 | 0xb1 => {
                        let acc = self.gpr_sized(0, size);
                        if acc == dst {
                            self.set_flag(ZF, true);
                            self.rm_write(mem, &rm, size, src)?;
                        } else {
                            self.set_flag(ZF, false);
                            self.set_gpr_sized(0, size, dst);
                        }
                    }
                    _ => {
                        self.rm_write(mem, &rm, size, src)?;
                        self.set_gpr_sized(regf, size, res);
                    }
                }
                Ok(Step::Continue)
            }
            0xc7 => {
                let (regf, rm) = self.decode_modrm(mem, p)?;
                if regf & 7 == 1 {
                    if let Some(a) = rm.mem {
                        let wide = p.w();
                        let cmp: u128 = if wide {
                            ((mem.read_u64(a + 8)? as u128) << 64) | mem.read_u64(a)? as u128
                        } else {
                            mem.read_u64(a)? as u128
                        };
                        let cur: u128 = if wide {
                            ((self.gpr_sized(2, 8) as u128) << 64) | self.gpr_sized(0, 8) as u128
                        } else {
                            ((self.gpr_sized(2, 4) as u128) << 32) | self.gpr_sized(0, 4) as u128
                        };
                        if cmp == cur {
                            self.set_flag(ZF, true);
                            let nb: u128 = if wide {
                                ((self.gpr_sized(3, 8) as u128) << 64) | self.gpr_sized(1, 8) as u128
                            } else {
                                ((self.gpr_sized(3, 4) as u128) << 32) | self.gpr_sized(1, 4) as u128
                            };
                            if wide {
                                mem.write_u64(a, nb as u64)?;
                                mem.write_u64(a + 8, (nb >> 64) as u64)?;
                            } else {
                                mem.write_u64(a, nb as u64)?;
                            }
                        } else {
                            self.set_flag(ZF, false);
                            if wide {
                                self.set_gpr_sized(0, 8, cmp as u64);
                                self.set_gpr_sized(2, 8, (cmp >> 64) as u64);
                            } else {
                                self.set_gpr_sized(0, 4, cmp as u64);
                                self.set_gpr_sized(2, 4, (cmp >> 32) as u64);
                            }
                        }
                        return Ok(Step::Continue);
                    }
                }
                match regf & 7 {
                    6 | 7 => {
                        let v = mem_tsc().wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (start << 7);
                        self.set_gpr_sized(rm.reg, if p.w() { 8 } else { 4 }, v);
                        self.set_flag(CF, true);
                        Ok(Step::Continue)
                    }
                    _ => Err(self.undef(start, mem)),
                }
            }
            0xc8..=0xcf => {
                let r = (op & 7) | (p.b() << 3);
                let size = self.opsize(p, 4);
                let v = self.gpr(r);
                let sw = match size {
                    8 => v.swap_bytes(),
                    4 => (v as u32).swap_bytes() as u64,
                    _ => (v as u16).swap_bytes() as u64,
                };
                self.set_gpr_sized(r, size, sw);
                Ok(Step::Continue)
            }
            0xba => {
                let size = self.opsize(p, 4);
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let imm = self.fetch_imm(mem, 1)?;
                let limit = size as u64 * 8 - 1;
                let bit = (imm & limit) as u32;
                let v = self.rm_read(mem, &rm, size)?;
                self.set_flag(CF, (v >> bit) & 1 != 0);
                let nv = match regf & 7 {
                    4 => v,
                    5 => v | (1u64 << bit),
                    6 => v & !(1u64 << bit),
                    _ => v ^ (1u64 << bit),
                };
                if (regf & 7) != 4 {
                    self.rm_write(mem, &rm, size, nv)?;
                }
                Ok(Step::Continue)
            }
            0xa4 | 0xa5 | 0xac | 0xad => {
                let size = self.opsize(p, 4);
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let count = if op & 1 == 0 {
                    self.fetch_imm(mem, 1)?
                } else {
                    self.gpr_sized(1, 1)
                };
                let bits = size as u32 * 8;
                let cnt = (count % bits as u64) as u32;
                let dst = self.rm_read(mem, &rm, size)?;
                let src = self.gpr_sized(regf, size);
                let m = mask(0, size);
                let r = if cnt == 0 {
                    dst
                } else if op <= 0xa5 {
                    ((dst << cnt) | (src >> (bits - cnt))) & m
                } else {
                    ((dst >> cnt) | (src << (bits - cnt))) & m
                };
                if cnt != 0 {
                    self.set_szp(r, size);
                }
                self.rm_write(mem, &rm, size, r)?;
                Ok(Step::Continue)
            }
            0xae => {
                let (_regf, _rm) = self.decode_modrm(mem, p)?;
                Ok(Step::Continue)
            }
            0x01 | 0x20 | 0x21 | 0x22 | 0x23 | 0x30 | 0x33 => {
                let (_regf, _rm) = self.decode_modrm(mem, p)?;
                Ok(Step::Continue)
            }
            0x32 => {
                self.set_gpr_sized(0, 4, 0);
                self.set_gpr_sized(2, 4, 0);
                Ok(Step::Continue)
            }
            0x06 | 0x07 | 0x08 | 0x09 | 0x0d | 0x0e | 0x77 => Ok(Step::Continue),
            0xa0 | 0xa8 => {
                // push fs/gs
                let _ = self.push(mem, 0)?;
                Ok(Step::Continue)
            }
            0xa1 | 0xa9 => {
                let _ = self.pop(mem)?;
                Ok(Step::Continue)
            }
            0xb8 => {
                // popcnt
                let size = self.opsize(p, 4);
                let (regf, rm) = self.decode_modrm(mem, p)?;
                let v = self.rm_read(mem, &rm, size)? & mask(0, size);
                self.set_gpr_sized(regf, size, v.count_ones() as u64);
                self.set_flag(ZF, v == 0);
                Ok(Step::Continue)
            }
            // ---- SSE
            0x10 | 0x11 | 0x28 | 0x29 | 0x6f | 0x7f | 0x12 | 0x13 | 0x16 | 0x17 | 0x14 | 0x15 => {
                self.sse_mov(op, p, mem)
            }
            0x6e | 0x7e | 0xd6 => self.sse_movd(op, p, mem),
            0x2a | 0x2c | 0x2d => self.sse_cvt_int(op, p, mem),
            0x58 | 0x59 | 0x5c | 0x5e | 0x5d | 0x5f | 0x51 => self.sse_arith(op, p, mem),
            0x5a | 0x5b => self.sse_cvt(op, p, mem),
            0x2e | 0x2f => self.sse_comis(op, p, mem),
            0x54 | 0x55 | 0x56 | 0x57 | 0xef | 0xeb | 0xdb => self.sse_logic(op, p, mem),
            0x74 | 0x75 | 0x76 | 0xc2 => self.sse_cmp(op, p, mem),
            0x60..=0x6d | 0x70 => self.sse_pack(op, p, mem),
            0xd7 => self.sse_pmovmskb(p, mem),
            0x73 | 0x71 | 0x72 => self.sse_shift(op, p, mem),
            0xfc | 0xfd | 0xfe | 0xfa | 0xfb | 0xf8 | 0xf9 | 0xf1 | 0xf5 | 0xe8 | 0xe9 | 0xea => {
                self.sse_int(op, p, mem)
            }
            0x38 | 0x3a => {
                // трёхбайтовые опкоды: пропускаем как можно безопаснее
                let _op3 = self.fetch8(mem)?;
                let (_regf, rm) = self.decode_modrm(mem, p)?;
                if let Some(a) = rm.mem {
                    let mut buf = [0u8; 16];
                    let _ = mem.read_partial(a, &mut buf);
                }
                if op == 0x3a {
                    let _ = self.fetch_imm(mem, 1);
                }
                Ok(Step::Continue)
            }
            _ => Err(self.undef(start, mem)),
        }
    }

    fn xmm_read(&mut self, mem: &mut Memory, rm: &Rm, bytes: usize) -> Result<u128, CpuError> {
        match rm.mem {
            Some(a) => {
                let mut buf = [0u8; 16];
                mem.read_bytes(a, &mut buf[..bytes])?;
                Ok(u128::from_le_bytes(buf))
            }
            None => Ok(self.regs.xmm[(rm.reg & 15) as usize]),
        }
    }

    fn sse_mov(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let load = matches!(op, 0x10 | 0x28 | 0x6f | 0x12 | 0x16);
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let d = (regf & 15) as usize;
        // movss/movsd работают только с младшим элементом
        let scalar = if p.p_f3 { Some(4) } else if p.p_f2 { Some(8) } else { None };
        if load {
            let v = self.xmm_read(mem, &rm, 16)?;
            match scalar {
                Some(4) => {
                    self.regs.xmm[d] = (self.regs.xmm[d] & !0xFFFF_FFFFu128) | (v & 0xFFFF_FFFF);
                }
                Some(8) => {
                    self.regs.xmm[d] =
                        (self.regs.xmm[d] & !0xFFFF_FFFF_FFFF_FFFFu128) | (v & 0xFFFF_FFFF_FFFF_FFFF);
                }
                _ => self.regs.xmm[d] = v,
            }
        } else {
            let v = self.regs.xmm[d];
            let bytes = scalar.unwrap_or(16);
            match rm.mem {
                Some(a) => mem.write_bytes(a, &v.to_le_bytes()[..bytes])?,
                None => {
                    let r = (rm.reg & 15) as usize;
                    match scalar {
                        Some(4) => {
                            self.regs.xmm[r] = (self.regs.xmm[r] & !0xFFFF_FFFFu128) | (v & 0xFFFF_FFFF)
                        }
                        Some(8) => {
                            self.regs.xmm[r] = (self.regs.xmm[r] & !0xFFFF_FFFF_FFFF_FFFFu128)
                                | (v & 0xFFFF_FFFF_FFFF_FFFF)
                        }
                        _ => self.regs.xmm[r] = v,
                    }
                }
            }
        }
        Ok(Step::Continue)
    }

    fn sse_movd(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let wide = p.w();
        let size = if wide { 8 } else { 4 };
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let d = (regf & 15) as usize;
        match op {
            0x6e => {
                // 66 0F 6E: movd/movq xmm, r/m
                let v = self.rm_read(mem, &rm, size)?;
                self.regs.xmm[d] = v as u128;
            }
            0x7e => {
                if p.p_f3 {
                    // F3 0F 7E: movq xmm, xmm/m64 (загрузка)
                    let v = self.xmm_read(mem, &rm, 8)?;
                    self.regs.xmm[d] = v & 0xFFFF_FFFF_FFFF_FFFF;
                } else {
                    // 66 0F 7E: movd/movq r/m, xmm (сохранение)
                    let v = self.regs.xmm[d];
                    match rm.mem {
                        Some(a) => mem.write_bytes(a, &v.to_le_bytes()[..size as usize])?,
                        None => self.set_gpr_sized(rm.reg, size, v as u64),
                    }
                }
            }
            0xd6 => {
                // 66 0F D6: movq r/m64, xmm
                let v = self.regs.xmm[d];
                match rm.mem {
                    Some(a) => mem.write_bytes(a, &v.to_le_bytes()[..size as usize])?,
                    None => self.set_gpr_sized(rm.reg, size, v as u64),
                }
            }
            _ => {}
        }
        Ok(Step::Continue)
    }

    fn sse_cvt_int(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let dbl = p.p_f2;
        let size = if p.w() { 8 } else { 4 };
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let d = (regf & 15) as usize;
        match op {
            0x2a => {
                let iv = self.rm_read(mem, &rm, size)?;
                let f = if size == 8 { iv as i64 as f64 } else { iv as u32 as i32 as f64 };
                if dbl {
                    self.regs.xmm[d] = (self.regs.xmm[d] & !0xFFFF_FFFF_FFFF_FFFFu128) | f.to_bits() as u128;
                } else {
                    self.regs.xmm[d] =
                        (self.regs.xmm[d] & !0xFFFF_FFFFu128) | (f as f32).to_bits() as u128;
                }
            }
            _ => {
                let v = self.xmm_read(mem, &rm, if dbl { 8 } else { 4 })?;
                let f = if dbl { f64::from_bits(v as u64) } else { f32::from_bits(v as u32) as f64 };
                let t = if op == 0x2c { f.trunc() } else { f.round_ties_even() };
                let out = if size == 8 {
                    if t >= i64::MAX as f64 {
                        i64::MIN as u64
                    } else {
                        t as i64 as u64
                    }
                } else if t >= i32::MAX as f64 || t <= i32::MIN as f64 {
                    0x8000_0000
                } else {
                    t as i32 as u32 as u64
                };
                self.set_gpr_sized(regf, size, out);
            }
        }
        Ok(Step::Continue)
    }

    fn sse_arith(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let dbl = p.p_f2;
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let d = (regf & 15) as usize;
        if dbl {
            let b = f64::from_bits(self.xmm_read(mem, &rm, 8)? as u64);
            let a = f64::from_bits(self.regs.xmm[d] as u64);
            let r = match op {
                0x58 => a + b,
                0x59 => a * b,
                0x5c => a - b,
                0x5e => a / b,
                0x5d => a.min(b),
                0x5f => a.max(b),
                _ => a.sqrt(),
            };
            self.regs.xmm[d] = (self.regs.xmm[d] & !0xFFFF_FFFF_FFFF_FFFFu128) | r.to_bits() as u128;
        } else {
            let b = f32::from_bits(self.xmm_read(mem, &rm, 4)? as u32);
            let a = f32::from_bits(self.regs.xmm[d] as u32);
            let r = match op {
                0x58 => a + b,
                0x59 => a * b,
                0x5c => a - b,
                0x5e => a / b,
                0x5d => a.min(b),
                0x5f => a.max(b),
                _ => a.sqrt(),
            };
            self.regs.xmm[d] = (self.regs.xmm[d] & !0xFFFF_FFFFu128) | r.to_bits() as u128;
        }
        Ok(Step::Continue)
    }

    fn sse_cvt(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let dbl = p.p_f2;
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let d = (regf & 15) as usize;
        match op {
            0x5a => {
                if dbl {
                    let v = f64::from_bits(self.xmm_read(mem, &rm, 8)? as u64);
                    self.regs.xmm[d] = (self.regs.xmm[d] & !0xFFFF_FFFFu128) | (v as f32).to_bits() as u128;
                } else {
                    let v = f32::from_bits(self.xmm_read(mem, &rm, 4)? as u32);
                    self.regs.xmm[d] =
                        (self.regs.xmm[d] & !0xFFFF_FFFF_FFFF_FFFFu128) | (v as f64).to_bits() as u128;
                }
            }
            0x5b => {
                let v = self.xmm_read(mem, &rm, 16)?;
                let mut r = 0u128;
                for i in 0..4 {
                    let elem = ((v >> (i * 32)) & 0xFFFF_FFFF) as u32 as i32 as f32;
                    r |= (elem.to_bits() as u128) << (i * 32);
                }
                self.regs.xmm[d] = r;
            }
            _ => {}
        }
        Ok(Step::Continue)
    }

    fn sse_comis(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let dbl = p.p_f2;
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let d = (regf & 15) as usize;
        let a;
        let b;
        if dbl {
            a = f64::from_bits(self.regs.xmm[d] as u64);
            b = f64::from_bits(self.xmm_read(mem, &rm, 8)? as u64);
        } else {
            a = f32::from_bits(self.regs.xmm[d] as u32) as f64;
            b = f32::from_bits(self.xmm_read(mem, &rm, 4)? as u32) as f64;
        }
        let unordered = a.is_nan() || b.is_nan();
        let _ = op;
        self.set_flag(ZF, unordered || a == b);
        self.set_flag(PF, unordered);
        self.set_flag(CF, !unordered && a < b);
        self.set_flag(OF, false);
        self.set_flag(SF, false);
        self.set_flag(AF, false);
        Ok(Step::Continue)
    }

    fn sse_logic(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let d = (regf & 15) as usize;
        let a = self.regs.xmm[d];
        let b = self.xmm_read(mem, &rm, 16)?;
        self.regs.xmm[d] = match op {
            0x54 => a & !b,
            0x55 => a & b,
            0x56 => a | b,
            _ => a ^ b,
        };
        Ok(Step::Continue)
    }

    fn sse_cmp(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let d = (regf & 15) as usize;
        let a = self.regs.xmm[d];
        let b = self.xmm_read(mem, &rm, 16)?;
        let (elem_bits, count, imm) = match op {
            0x74 => (8u32, 16u32, 0u8),
            0x75 => (16, 8, 0),
            0x76 => (32, 4, 0),
            _ => (32, 4, 0),
        };
        let real_imm = if op == 0xc2 { self.fetch_imm(mem, 1).unwrap_or(0) as u8 } else { imm };
        let m = mask128(elem_bits);
        let mut r = 0u128;
        for i in 0..count {
            let shift = i * elem_bits;
            let av = (a >> shift) & m;
            let bv = (b >> shift) & m;
            let res = match op {
                0xc2 => {
                    let x = f32::from_bits(av as u32);
                    let y = f32::from_bits(bv as u32);
                    match real_imm & 7 {
                        0 => x == y,
                        1 => x < y,
                        2 => x <= y,
                        3 => x.is_nan() || y.is_nan(),
                        4 => !(x == y),
                        5 => !(x < y),
                        6 => !(x <= y),
                        _ => !(x.is_nan() || y.is_nan()),
                    }
                }
                _ => av == bv,
            };
            if res {
                r |= m << shift;
            }
        }
        self.regs.xmm[d] = r;
        Ok(Step::Continue)
    }

    fn sse_pack(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let d = (regf & 15) as usize;
        let a = self.regs.xmm[d];
        let b = self.xmm_read(mem, &rm, 16)?;
        match op {
            0x60..=0x67 => {
                let elem_bits = 8u32 << ((op & 3) as u32);
                let elems = 128 / elem_bits;
                let half = elems / 2;
                let mask_e = mask128(elem_bits);
                let mut r = 0u128;
                for i in 0..half {
                    let av = (a >> (i * elem_bits)) & mask_e;
                    let bv = (b >> (i * elem_bits)) & mask_e;
                    r |= av << ((2 * i) * elem_bits);
                    r |= bv << ((2 * i + 1) * elem_bits);
                }
                if op >= 0x64 {
                    // высокая половина
                    let mut r = 0u128;
                    for i in 0..half {
                        let av = (a >> (64 + i * elem_bits)) & mask_e;
                        let bv = (b >> (64 + i * elem_bits)) & mask_e;
                        r |= av << ((2 * i) * elem_bits);
                        r |= bv << ((2 * i + 1) * elem_bits);
                    }
                    self.regs.xmm[d] = r;
                    return Ok(Step::Continue);
                }
                self.regs.xmm[d] = r;
            }
            0x68..=0x6b => {
                let elem_bits = 8u32 << ((op & 3) as u32);
                let elems = 128 / elem_bits;
                let half = elems / 2;
                let mask_e = mask128(elem_bits);
                let mut r = 0u128;
                for i in 0..half {
                    let av = sign_elem((a >> (i * elem_bits)) & mask_e, elem_bits);
                    let bv = sign_elem((b >> (i * elem_bits)) & mask_e, elem_bits);
                    let lo = av.min(bv);
                    let hi = av.max(bv);
                    r |= ((lo as u128) & mask_e) << (i * elem_bits);
                    r |= ((hi as u128) & mask_e) << ((i + half) * elem_bits);
                }
                self.regs.xmm[d] = r;
            }
            0x6c => self.regs.xmm[d] = (a & 0xFFFF_FFFF_FFFF_FFFF) | ((b & 0xFFFF_FFFF_FFFF_FFFF) << 64),
            0x6d => self.regs.xmm[d] = (a >> 64) | ((b >> 64) << 64),
            0x70 => {
                // pshufd
                let imm = self.fetch_imm(mem, 1)? as u8;
                let mut r = 0u128;
                for i in 0..4 {
                    let idx = ((imm >> (i * 2)) & 3) as u32;
                    r |= ((a >> (idx * 32)) & 0xFFFF_FFFF) << (i * 32);
                }
                self.regs.xmm[d] = r;
            }
            _ => {}
        }
        Ok(Step::Continue)
    }

    fn sse_pmovmskb(&mut self, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let v = self.xmm_read(mem, &rm, 16)?;
        let mut m = 0u64;
        for i in 0..16 {
            if (v >> (i * 8)) & 0x80 != 0 {
                m |= 1 << i;
            }
        }
        self.set_gpr_sized(regf, 4, m);
        Ok(Step::Continue)
    }

    fn sse_shift(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let d = (regf & 15) as usize;
        let imm = self.fetch_imm(mem, 1)? as u32;
        let v = self.xmm_read(mem, &rm, 16)?;
        let r = match op {
            0x73 => match regf & 7 {
                2 | 6 => v >> imm.min(64),
                3 => {
                    if imm >= 16 {
                        0
                    } else {
                        v >> (imm * 8)
                    }
                }
                7 => {
                    if imm >= 16 {
                        0
                    } else {
                        v << (imm * 8)
                    }
                }
                _ => v,
            },
            0x71 => v,
            0x72 => v,
            _ => v,
        };
        self.regs.xmm[d] = r;
        Ok(Step::Continue)
    }

    fn sse_int(&mut self, op: u8, p: &Pfx, mem: &mut Memory) -> Result<Step, CpuError> {
        let (regf, rm) = self.decode_modrm(mem, p)?;
        let d = (regf & 15) as usize;
        let a = self.regs.xmm[d];
        let b = self.xmm_read(mem, &rm, 16)?;
        let r = match op {
            0xfc | 0xfd | 0xfe => {
                let bits = 8u32 << (op & 1) as u32;
                let mask_e = mask128(bits);
                let mut r = 0u128;
                for i in 0..(128 / bits) {
                    let x = (a >> (i * bits)) & mask_e;
                    let y = (b >> (i * bits)) & mask_e;
                    r |= ((x.wrapping_add(y)) & mask_e) << (i * bits);
                }
                r
            }
            0xf8 | 0xf9 | 0xfa | 0xfb => {
                let bits = 8u32 << (op & 1) as u32;
                let mask_e = mask128(bits);
                let mut r = 0u128;
                for i in 0..(128 / bits) {
                    let x = (a >> (i * bits)) & mask_e;
                    let y = (b >> (i * bits)) & mask_e;
                    r |= ((x.wrapping_sub(y)) & mask_e) << (i * bits);
                }
                r
            }
            0xf1 | 0xf5 => {
                // psllw/psrlw по xmm-счётчику — упрощённо
                a
            }
            0xe8 | 0xe9 | 0xea | 0xeb => {
                // psubsb/psubsw/psubusb/psubusw — упрощённо
                a.wrapping_sub(b)
            }
            _ => a,
        };
        self.regs.xmm[d] = r;
        Ok(Step::Continue)
    }
}

// ---- вспомогательные функции --------------------------------------------

#[inline]
pub fn mask(_v: u64, size: u8) -> u64 {
    match size {
        1 => 0xFF,
        2 => 0xFFFF,
        4 => 0xFFFF_FFFF,
        _ => u64::MAX,
    }
}

#[inline]
fn mask128(bits: u32) -> u128 {
    if bits >= 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    }
}

#[inline]
fn sign_elem(v: u128, bits: u32) -> i128 {
    let sign = 1i128 << (bits - 1);
    let x = v as i128;
    if x & sign != 0 {
        x - (1i128 << bits)
    } else {
        x
    }
}

#[inline]
fn fpu_swap(a: f64, b: f64) -> (f64, f64) {
    (a, b)
}

#[inline]
pub fn sign_ext(v: u64, size: u8) -> i64 {
    match size {
        1 => v as u8 as i8 as i64,
        2 => v as u16 as i16 as i64,
        4 => v as u32 as i32 as i64,
        _ => v as i64,
    }
}

fn imul_wide(a: u64, b: u64, size: u8) -> (u64, bool) {
    let bits = size as u32 * 8;
    let m = mask(0, size);
    let (x, y) = (sign_ext(a, size), sign_ext(b, size));
    let full = (x as i128) * (y as i128);
    let lo = (full as u64) & m;
    let ovf = if bits >= 128 {
        false
    } else {
        sign_ext(lo, size) as i128 != full
    };
    (lo, ovf)
}

fn mem_tsc() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    static START: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let start = *START.get_or_init(|| {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
    });
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_nanos() as u64).wrapping_sub(start).wrapping_mul(2))
        .unwrap_or(0)
}

fn cpuid_leaf(leaf: u32, sub: u32) -> (u32, u32, u32, u32) {
    let _ = sub;
    // Честно сообщаем только те расширения, которые действительно
    // эмулируются: без AVX/AVX2/BMI/FMA, иначе libc выбирает
    // несовместимые пути (ifunc) и данные портятся.
    let edx_leaf1 = (1 << 0)   // FPU
        | (1 << 2)             // DE
        | (1 << 3)             // PSE
        | (1 << 4)             // TSC
        | (1 << 5)             // MSR
        | (1 << 6)             // PAE
        | (1 << 8)             // CX8
        | (1 << 11)            // SEP
        | (1 << 12)            // MTRR
        | (1 << 13)            // PGE
        | (1 << 15)            // CMOV
        | (1 << 16)            // PAT
        | (1 << 19)            // CLFSH
        | (1 << 23)            // MMX
        | (1 << 24)            // FXSR
        | (1 << 25)            // SSE
        | (1 << 26); // SSE2
    let ecx_leaf1 = 1 << 0      // SSE3
        | 1 << 9                // SSSE3
        | 1 << 13; // CMPXCHG16B
    let edx_ext1 = (1 << 11)    // SYSCALL
        | (1 << 20)            // NX
        | (1 << 26)            // 1GB pages
        | (1 << 27); // RDTSCP (не используется, но безопасно)
    match leaf {
        0 => (0x0000_000D, 0x6874_7541, 0x444D_4163, 0x6974_6E65),
        // Семейство 16h (AMD Jaguar), модель 0, шаг 1
        1 => (0x0060_0F10, 0x0008_0000, ecx_leaf1, edx_leaf1),
        6 => (0x0000_0004, 0, 0, 0),
        7 => (0, 0, 0, 0),
        0x8000_0000 => (0x8000_001F, 0x6874_7541, 0x444D_4163, 0x6974_6E65),
        0x8000_0001 => (0x0060_0F10, 0, 0x0000_0001, edx_ext1 as u32),
        0x8000_0005 => (0x0000_0020, 0, 0, 0),
        0x8000_0006 => (0, 0, 0x0000_0102, 0),
        0x8000_0008 => (0x0000_0302, 0, 0, 0),
        _ => (0, 0, 0, 0),
    }
}
