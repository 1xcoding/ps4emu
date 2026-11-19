//! Трассировка выполнения: кольцевой буфер последних инструкций,
//! мини-дизассемблер и символизация адресов.

use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub struct TraceEntry {
    pub rip: u64,
    pub bytes: [u8; 8],
    pub rax: u64,
    pub rsp: u64,
}

pub struct Tracer {
    pub ring: VecDeque<TraceEntry>,
    pub capacity: usize,
    pub enabled: bool,
    /// Число инструкций между записями (1 = каждая).
    pub stride: u64,
    counter: u64,
}

impl Tracer {
    pub fn new(capacity: usize) -> Self {
        Tracer { ring: VecDeque::with_capacity(capacity.min(1 << 16)), capacity, enabled: false, stride: 1, counter: 0 }
    }

    pub fn record(&mut self, rip: u64, bytes: &[u8], rax: u64, rsp: u64) {
        if !self.enabled {
            return;
        }
        self.counter += 1;
        if self.counter % self.stride != 0 {
            return;
        }
        let mut b = [0u8; 8];
        let n = bytes.len().min(8);
        b[..n].copy_from_slice(&bytes[..n]);
        if self.ring.len() >= self.capacity {
            self.ring.pop_front();
        }
        self.ring.push_back(TraceEntry { rip, bytes: b, rax, rsp });
    }

    pub fn dump(&self, limit: usize) -> String {
        let mut s = String::new();
        let n = self.ring.len();
        for (i, e) in self.ring.iter().enumerate().rev() {
            if n - i > limit {
                break;
            }
            s.push_str(&format!(
                "{:016x}  {:<24} rax={:016x} rsp={:016x}\n",
                e.rip,
                disasm(&e.bytes, e.rip),
                e.rax,
                e.rsp
            ));
        }
        s
    }
}

impl Default for Tracer {
    fn default() -> Self {
        Tracer::new(64)
    }
}

// ---------------------------------------------------------------------------
// Дизассемблер
// ---------------------------------------------------------------------------

const R64: [&str; 16] = [
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];
const R32: [&str; 16] = [
    "eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi", "r8d", "r9d", "r10d", "r11d", "r12d",
    "r13d", "r14d", "r15d",
];
const R16: [&str; 16] = [
    "ax", "cx", "dx", "bx", "sp", "bp", "si", "di", "r8w", "r9w", "r10w", "r11w", "r12w", "r13w",
    "r14w", "r15w",
];
const R8: [&str; 16] = [
    "al", "cl", "dl", "bl", "spl", "bpl", "sil", "dil", "r8b", "r9b", "r10b", "r11b", "r12b", "r13b",
    "r14b", "r15b",
];

fn reg_name(idx: usize, size: u8) -> &'static str {
    match size {
        1 => R8[idx & 15],
        2 => R16[idx & 15],
        8 => R64[idx & 15],
        _ => R32[idx & 15],
    }
}

/// Префиксы и REX инструкции.
struct Px {
    rex: u8,
    size: u8,
    seg: Option<&'static str>,
    rep: Option<&'static str>,
    opsize: bool,
}

/// Результат разбора ModRM.
struct ModRm {
    /// Регистр из поля reg.
    reg: usize,
    /// Регистр r/m (если mod == 3).
    rm_reg: usize,
    /// Текстовое представление r/m.
    rm: String,
    /// Длина ModRM+SIB+disp.
    len: usize,
    /// Это память (mod != 3).
    is_mem: bool,
}

fn parse_modrm(b: &[u8], i: usize, px: &Px) -> ModRm {
    let m = *b.get(i).unwrap_or(&0);
    let modb = m >> 6;
    let reg = (((m >> 3) & 7) | ((px.rex & 4) << 1)) as usize;
    let rm_low = (m & 7) as usize;
    let rm_ext = ((px.rex & 1) << 3) as usize;
    let mut len = 1usize;
    if modb == 3 {
        return ModRm { reg, rm_reg: rm_low | rm_ext, rm: String::new(), len, is_mem: false };
    }
    let mut base = rm_low | rm_ext;
    let mut index: Option<usize> = None;
    let mut scale = 1u8;
    let mut disp: i64 = 0;
    let mut disp_size = 0usize;
    let mut rip_rel = false;
    if rm_low == 4 {
        // SIB
        let sib = *b.get(i + 1).unwrap_or(&0);
        len += 1;
        scale = 1 << (sib >> 6);
        index = Some((((sib >> 3) & 7) | ((px.rex & 2) << 2)) as usize);
        base = ((sib & 7) as usize) | rm_ext;
        if sib & 7 == 5 && modb == 0 {
            disp_size = 4;
            rip_rel = true;
        }
    } else if rm_low == 5 && modb == 0 {
        disp_size = 4;
        rip_rel = true;
    } else if modb == 1 {
        disp_size = 1;
    } else if modb == 2 {
        disp_size = 4;
    }
    if disp_size == 1 {
        disp = *b.get(i + len).unwrap_or(&0) as i8 as i64;
    } else if disp_size == 4 {
        let mut a = [0u8; 4];
        for k in 0..4 {
            a[k] = *b.get(i + len + k).unwrap_or(&0);
        }
        disp = i32::from_le_bytes(a) as i64;
    }
    len += disp_size;
    let mut txt = String::new();
    let dsz = if !px.opsize && px.rex & 8 == 0 { 4 } else { 8 };
    let _ = dsz;
    if rip_rel {
        txt.push_str(&format!("[rip{:+}]", disp));
    } else {
        txt.push('[');
        if let Some(idx) = index {
            if idx & 15 != 4 || px.rex & 2 != 0 {
                txt.push_str(&format!("{}*{}+", R64[idx & 15], scale));
            }
        }
        txt.push_str(&format!("{}", if base & 15 == 4 && index.is_none() && px.rex & 1 == 0 { "rsp" } else { R64[base & 15] }));
        if disp != 0 || disp_size > 0 {
            txt.push_str(&format!("{:+}", disp));
        }
        txt.push(']');
    }
    if let Some(seg) = px.seg {
        txt = format!("{}:{}", seg, txt);
    }
    ModRm { reg, rm_reg: base, rm: txt, len, is_mem: true }
}

/// Дизассемблировать одну инструкцию. Возвращает (текст, длина).
pub fn disasm_len(bytes: &[u8], rip: u64) -> (String, usize) {
    let mut i = 0usize;
    let mut px = Px { rex: 0, size: 4, seg: None, rep: None, opsize: false };
    while i < bytes.len() {
        match bytes[i] {
            0x66 => {
                px.opsize = true;
                px.size = 2;
            }
            0x67 => {}
            0xF2 => px.rep = Some("repne "),
            0xF3 => px.rep = Some("rep "),
            0x64 => px.seg = Some("fs"),
            0x65 => px.seg = Some("gs"),
            0x40..=0x4f => {
                px.rex = bytes[i];
                if px.rex & 8 != 0 {
                    px.size = 8;
                }
            }
            0x2e | 0x3e | 0x26 | 0x36 | 0xf0 => {}
            _ => break,
        }
        i += 1;
    }
    let sz = px.size;
    if i >= bytes.len() {
        return (hex(&bytes[..i]), i);
    }
    let op = bytes[i];
    i += 1;
    #[allow(unused_mut)]
    let mut text: String;
    match op {
        // --- арифметика и логика с ModRM -------------------------------
        0x00..=0x03 | 0x08..=0x0b | 0x18..=0x1b | 0x20..=0x23 | 0x28..=0x2b
        | 0x30..=0x33 | 0x38..=0x3b | 0x84 | 0x85 | 0x86 | 0x87 => {
            let names = ["add", "or", "adc", "sbb", "and", "sub", "xor", "cmp"];
            let mut nm = names[((op >> 3) & 7) as usize];
            if op == 0x84 || op == 0x85 {
                nm = "test";
            }
            if op == 0x86 || op == 0x87 {
                nm = "xchg";
            }
            let byte_ops = op & 1 == 0;
            let s = if byte_ops { 1 } else { sz };
            let m = parse_modrm(bytes, i, &px);
            let operand = if m.is_mem {
                m.rm.clone()
            } else {
                reg_name(m.rm_reg, s).to_string()
            };
            // Младший бит опкода: 0 — r/m получает источник, 1 — регистр.
            text = if op & 2 == 0 {
                format!("{} {}, {}", nm, operand, reg_name(m.reg, s))
            } else {
                format!("{} {}, {}", nm, reg_name(m.reg, s), operand)
            };
            i += m.len;
        }
        0x04 | 0x0c | 0x14 | 0x1c | 0x24 | 0x2c | 0x34 | 0x3c => {
            let names = ["add", "or", "adc", "sbb", "and", "sub", "xor", "cmp"];
            let nm = names[((op >> 3) & 7) as usize];
            text = format!("{} al, {:#x}", nm, *bytes.get(i).unwrap_or(&0));
            i += 1;
        }
        0x05 | 0x0d | 0x15 | 0x1d | 0x25 | 0x2d | 0x35 | 0x3d => {
            let names = ["add", "or", "adc", "sbb", "and", "sub", "xor", "cmp"];
            let nm = names[((op >> 3) & 7) as usize];
            let n = imm_len(sz);
            let imm = read_u64(bytes, i, n as u8);
            text = format!("{} {}, {:#x}", nm, reg_name(0, sz), imm);
            i += n;
        }
        0x80 | 0x81 | 0x82 | 0x83 => {
            let names = ["add", "or", "adc", "sbb", "and", "sub", "xor", "cmp"];
            let m = parse_modrm(bytes, i, &px);
            let nm = names[m.reg & 7];
            let s = if op == 0x80 { 1 } else { sz };
            let off = i + m.len;
            let n_imm = if op == 0x83 {
                1
            } else if s == 1 {
                1
            } else {
                imm_len(sz)
            };
            let imm = if n_imm == 1 {
                (*bytes.get(off).unwrap_or(&0) as i8 as i64) as u64
            } else {
                read_u64(bytes, off, n_imm as u8)
            };
            let dst = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, s).to_string() };
            let shown = if op == 0x83 { (imm as i64).to_string() } else { format!("{:#x}", imm) };
            text = format!("{} {}, {}", nm, dst, shown);
            i = off + n_imm;
        }
        0x68 => {
            let n = imm_len(sz);
            let imm = read_u64(bytes, i, n as u8);
            text = format!("push {:#x}", imm);
            i += n;
        }
        0x6a => {
            text = format!("push {}", *bytes.get(i).unwrap_or(&0) as i8);
            i += 1;
        }
        0x69 => {
            let m = parse_modrm(bytes, i, &px);
            let off = i + m.len;
            let n = imm_len(sz);
            let imm = read_u64(bytes, off, n as u8);
            let dst = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, sz).to_string() };
            text = format!("imul {}, {}, {:#x}", reg_name(m.reg, sz), dst, imm);
            i = off + n;
        }
        0x6b => {
            let m = parse_modrm(bytes, i, &px);
            let off = i + m.len;
            let imm = *bytes.get(off).unwrap_or(&0) as i8;
            let dst = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, sz).to_string() };
            text = format!("imul {}, {}, {}", reg_name(m.reg, sz), dst, imm);
            i = off + 1;
        }
        0x88..=0x8b => {
            let s = if op & 1 == 0 { 1 } else { sz };
            let m = parse_modrm(bytes, i, &px);
            let operand = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, s).to_string() };
            let (dst, src) = if op & 2 == 0 {
                (operand, reg_name(m.reg, s).to_string())
            } else {
                (reg_name(m.reg, s).to_string(), operand)
            };
            text = format!("mov {}, {}", dst, src);
            i += m.len;
        }
        0x8d => {
            let m = parse_modrm(bytes, i, &px);
            text = format!("lea {}, {}", reg_name(m.reg, sz), m.rm);
            i += m.len;
        }
        0x63 => {
            let m = parse_modrm(bytes, i, &px);
            let operand = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, 4).to_string() };
            text = format!("movsxd {}, {}", reg_name(m.reg, 8.max(sz)), operand);
            i += m.len;
        }
        0xc0 | 0xc1 | 0xd0 | 0xd1 | 0xd2 | 0xd3 => {
            let names = ["rol", "ror", "rcl", "rcr", "shl", "shr", "sal", "sar"];
            let m = parse_modrm(bytes, i, &px);
            let nm = names[m.reg & 7];
            let s = if op == 0xc0 || op & 1 == 0 { 1 } else { sz };
            let dst = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, s).to_string() };
            if op == 0xc0 || op == 0xc1 {
                text = format!("{} {}, {}", nm, dst, *bytes.get(i + 1).unwrap_or(&0));
                i += m.len + 1;
            } else if op == 0xd0 || op == 0xd1 {
                text = format!("{} {}, 1", nm, dst);
                i += m.len;
            } else {
                text = format!("{} {}, cl", nm, dst);
                i += m.len;
            }
        }
        0xc6 | 0xc7 => {
            let m = parse_modrm(bytes, i, &px);
            let s = if op == 0xc6 { 1 } else { sz };
            let off = i + m.len;
            let n = if op == 0xc6 { 1 } else { imm_len(sz) };
            let imm = if n == 1 {
                *bytes.get(off).unwrap_or(&0) as u64
            } else {
                read_u64(bytes, off, n as u8)
            };
            let dst = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, s).to_string() };
            text = format!("mov {}, {:#x}", dst, imm);
            i = off + n;
        }
        0xf6 | 0xf7 => {
            let names = ["test", "test", "not", "neg", "mul", "imul", "div", "idiv"];
            let m = parse_modrm(bytes, i, &px);
            let nm = names[m.reg & 7];
            let dst = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, sz).to_string() };
            if m.reg & 7 <= 1 {
                let n = if op == 0xf6 { 1 } else { imm_len(sz) };
                let imm = read_u64(bytes, i + m.len, n as u8);
                text = format!("{} {}, {:#x}", nm, dst, imm);
                i += m.len + n;
            } else {
                text = format!("{} {}", nm, if m.is_mem { dst } else { reg_name(m.rm_reg, sz).to_string() });
                i += m.len;
            }
        }
        0xfe | 0xff => {
            let names = ["inc", "dec", "call", "callf", "jmp", "jmpf", "push", "-"];
            let m = parse_modrm(bytes, i, &px);
            let nm = names[m.reg & 7];
            let dst = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, 8).to_string() };
            text = format!("{} {}", nm, dst);
            i += m.len;
        }
        // --- mov с непосредственным -----------------------------------
        0xb0..=0xb7 => {
            let r = ((op & 7) | ((px.rex & 1) << 3)) as usize;
            text = format!("mov {}, {:#x}", reg_name(r, 1), *bytes.get(i).unwrap_or(&0));
            i += 1;
        }
        0xb8..=0xbf => {
            let r = ((op & 7) | ((px.rex & 1) << 3)) as usize;
            let imm = read_u64(bytes, i, sz);
            text = format!("mov {}, {:#x}", reg_name(r, sz), imm);
            i += sz as usize;
        }
        // --- стек и переходы -------------------------------------------
        0x50..=0x57 => {
            let r = ((op & 7) | ((px.rex & 1) << 3)) as usize;
            text = format!("push {}", reg_name(r, 8));
        }
        0x58..=0x5f => {
            let r = ((op & 7) | ((px.rex & 1) << 3)) as usize;
            text = format!("pop {}", reg_name(r, 8));
        }
        0x90 => {
            text = if px.rex & 1 != 0 { "xchg r8, rax".into() } else { "nop".into() };
        }
        0x91..=0x97 => {
            let r = ((op & 7) | ((px.rex & 1) << 3)) as usize;
            text = format!("xchg {}, {}", reg_name(0, sz), reg_name(r, sz));
        }
        0x98 => text = if sz == 8 { "cdqe".into() } else { "cwde".into() },
        0x99 => text = if sz == 8 { "cqo".into() } else { "cdq".into() },
        0xc3 => text = "ret".into(),
        0xc2 => {
            text = format!("ret {:#x}", read_u64(bytes, i, 2));
            i += 2;
        }
        0xc9 => text = "leave".into(),
        0xcc => text = "int3".into(),
        0xcd => {
            text = format!("int {:#x}", *bytes.get(i).unwrap_or(&0));
            i += 1;
        }
        0xf4 => text = "hlt".into(),
        0xf8 => text = "clc".into(),
        0xf9 => text = "stc".into(),
        0xfa => text = "cli".into(),
        0xfb => text = "sti".into(),
        0xfc => text = "cld".into(),
        0xfd => text = "std".into(),
        0xe8 => {
            let rel = read_i32(bytes, i);
            let target = rip.wrapping_add(i as u64 + 4).wrapping_add(rel as u64);
            text = format!("call {:#x}", target);
            i += 4;
        }
        0xe9 => {
            let rel = read_i32(bytes, i);
            let target = rip.wrapping_add(i as u64 + 4).wrapping_add(rel as u64);
            text = format!("jmp {:#x}", target);
            i += 4;
        }
        0xeb => {
            let rel = *bytes.get(i).unwrap_or(&0) as i8 as i64;
            let target = rip.wrapping_add(i as u64 + 1).wrapping_add(rel as u64);
            text = format!("jmp {:#x}", target);
            i += 1;
        }
        0x70..=0x7f => {
            let cc = ["o", "no", "b", "ae", "e", "ne", "be", "a", "s", "ns", "p", "np", "l", "ge", "le", "g"];
            let rel = *bytes.get(i).unwrap_or(&0) as i8 as i64;
            let target = rip.wrapping_add(i as u64 + 1).wrapping_add(rel as u64);
            text = format!("j{} {:#x}", cc[(op & 15) as usize], target);
            i += 1;
        }
        0x0f => {
            let op2 = *bytes.get(i).unwrap_or(&0);
            i += 1;
            match op2 {
                0x05 => text = "syscall".into(),
                0x0b => text = "ud2".into(),
                0x31 => text = "rdtsc".into(),
                0x34 => text = "sysenter".into(),
                0x35 => text = "sysexit".into(),
                0xa2 => text = "cpuid".into(),
                0xa0 => text = "push fs".into(),
                0xa1 => text = "pop fs".into(),
                0xa8 => text = "push gs".into(),
                0xa9 => text = "pop gs".into(),
                0x1f => {
                    let m = parse_modrm(bytes, i, &px);
                    text = format!("nop {}", m.rm);
                    i += m.len;
                }
                0x1e => {
                    // endbr64 / endbr32 — 4-байтовая форма F3 0F 1E FA/FB
                    text = "endbr64".into();
                    i += 1;
                }
                0x80..=0x8f => {
                    let cc = ["o", "no", "b", "ae", "e", "ne", "be", "a", "s", "ns", "p", "np", "l", "ge", "le", "g"];
                    let rel = read_i32(bytes, i);
                    let target = rip.wrapping_add(i as u64 + 4).wrapping_add(rel as u64);
                    text = format!("j{} {:#x}", cc[(op2 & 15) as usize], target);
                    i += 4;
                }
                0x90..=0x9f => {
                    let cc = ["o", "no", "b", "ae", "e", "ne", "be", "a", "s", "ns", "p", "np", "l", "ge", "le", "g"];
                    let m = parse_modrm(bytes, i, &px);
                    let dst = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, 1).to_string() };
                    text = format!("set{} {}", cc[(op2 & 15) as usize], dst);
                    i += m.len;
                }
                0xaf => {
                    let m = parse_modrm(bytes, i, &px);
                    let src = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, sz).to_string() };
                    text = format!("imul {}, {}", reg_name(m.reg, sz), src);
                    i += m.len;
                }
                0xb6 | 0xb7 | 0xbe | 0xbf => {
                    let m = parse_modrm(bytes, i, &px);
                    let ssize = if op2 & 1 == 0 { 1 } else { 2 };
                    let src = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, ssize).to_string() };
                    let nm = if op2 & 0x08 != 0 { "movsx" } else { "movzx" };
                    text = format!("{} {}, {}", nm, reg_name(m.reg, sz), src);
                    i += m.len;
                }
                0x40..=0x4f => {
                    let m = parse_modrm(bytes, i, &px);
                    let src = if m.is_mem { m.rm.clone() } else { reg_name(m.rm_reg, sz).to_string() };
                    text = format!("cmov{} {}, {}", ["o", "no", "b", "ae", "e", "ne", "be", "a", "s", "ns", "p", "np", "l", "ge", "le", "g"][(op2 & 15) as usize], reg_name(m.reg, sz), src);
                    i += m.len;
                }
                0x6e | 0x7e => {
                    text = "movd/movq xmm".into();
                }
                0x10..=0x17 | 0x28..=0x2f | 0x51 | 0x54 | 0x55 | 0x56 | 0x57 | 0x58..=0x5f
                | 0x60..=0x6d | 0x6f | 0x70..=0x76 | 0xd6 | 0xd7 | 0xef => {
                    let m = parse_modrm(bytes, i, &px);
                    let src = if m.is_mem { m.rm.clone() } else { format!("xmm{}", m.rm_reg) };
                    text = format!("{}{} xmm{}, {}", px.rep.unwrap_or(""), "sse", m.reg, src);
                    i += m.len;
                }
                0x01 => {
                    let m = parse_modrm(bytes, i, &px);
                    text = format!("system {} (0f 01)", m.rm);
                    i += m.len;
                }
                _ => {
                    text = format!("db 0f {:02x}", op2);
                }
            }
        }
        _ => {
            text = hex(&bytes[..i.min(bytes.len())]);
        }
    }
    let len = i.max(1);
    (text, len)
}

/// Размер непосредственного операнда: в 64-битном режиме большинство форм
/// с REX.W всё равно кодируют imm32 (со знаком).
fn imm_len(sz: u8) -> usize {
    match sz {
        2 => 2,
        8 => 4,
        _ => 4,
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect::<Vec<_>>().join(" ")
}

fn read_u64(b: &[u8], off: usize, size: u8) -> u64 {
    let n = size as usize;
    let mut v = 0u64;
    for i in 0..n {
        v |= (*b.get(off + i).unwrap_or(&0) as u64) << (8 * i);
    }
    v
}

fn read_i32(b: &[u8], off: usize) -> i32 {
    let mut a = [0u8; 4];
    for i in 0..4 {
        a[i] = *b.get(off + i).unwrap_or(&0);
    }
    i32::from_le_bytes(a)
}

/// Строковая форма: текст + длина не нужна вызывающему.
pub fn disasm(bytes: &[u8], rip: u64) -> String {
    disasm_len(bytes, rip).0
}

/// Символизация адреса по загруженным модулям.
pub struct Symbols {
    /// (начало, конец, имя модуля, возможная таблица).
    pub modules: Vec<(u64, u64, String)>,
    pub map: Vec<(u64, u64, String)>,
}

impl Symbols {
    pub fn new() -> Self {
        Symbols { modules: Vec::new(), map: Vec::new() }
    }

    pub fn add_module(&mut self, start: u64, end: u64, name: &str) {
        self.modules.push((start, end, name.to_string()));
    }

    pub fn add_symbol(&mut self, start: u64, size: u64, name: &str) {
        self.map.push((start, size.max(1), name.to_string()));
    }

    pub fn resolve(&self, addr: u64) -> Option<String> {
        let mut best: Option<&(u64, u64, String)> = None;
        for s in &self.map {
            if addr >= s.0 && addr < s.0 + s.1 {
                if best.map(|b| s.0 >= b.0).unwrap_or(true) {
                    best = Some(s);
                }
            }
        }
        if let Some(b) = best {
            return Some(format!("{}+{:#x}", b.2, addr - b.0));
        }
        for m in &self.modules {
            if addr >= m.0 && addr < m.1 {
                return Some(format!("{}+{:#x}", m.2, addr - m.0));
            }
        }
        None
    }
}

impl Default for Symbols {
    fn default() -> Self {
        Self::new()
    }
}
