//! Минимальный сборщик ELF64-файлов для тестов (без binutils).

#![allow(dead_code)]

/// Собранный образ: код + необязательная таблица символов.
pub struct GuestBuilder {
    pub code: Vec<u8>,
    pub data: Vec<u8>,
    pub rodata: Vec<u8>,
    pub symbols: Vec<(String, u64)>,
    pub base: u64,
    pub entry: u64,
}

impl GuestBuilder {
    pub fn new() -> Self {
        GuestBuilder {
            code: Vec::new(),
            data: Vec::new(),
            rodata: Vec::new(),
            symbols: Vec::new(),
            base: 0x40_0000,
            entry: 0x40_1000,
        }
    }

    /// Разместить код по адресу (для расчёта переходов).
    pub fn here(&self) -> u64 {
        self.entry + self.code.len() as u64
    }

    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.code.extend_from_slice(b);
        self
    }

    pub fn label(&mut self, name: &str) -> &mut Self {
        let addr = self.here();
        self.symbols.push((name.to_string(), addr));
        self
    }

    /// mov r64, imm64
    pub fn mov_imm64(&mut self, reg: u8, v: u64) -> &mut Self {
        let rex = 0x48u8 | if reg >= 8 { 1 } else { 0 };
        let mut b = vec![rex, 0xB8 | (reg & 7)];
        b.extend_from_slice(&v.to_le_bytes());
        self.code.extend(b);
        self
    }

    /// mov r32, imm32 (не обнуляет старшую половину при reg>=8 — для тестов хватит)
    pub fn mov_imm32(&mut self, reg: u8, v: u32) -> &mut Self {
        let mut b = Vec::new();
        if reg >= 8 {
            b.push(0x41);
        }
        b.push(0xB8 | (reg & 7));
        b.extend_from_slice(&v.to_le_bytes());
        self.code.extend(b);
        self
    }

    /// syscall
    pub fn syscall(&mut self) -> &mut Self {
        self.code.extend_from_slice(&[0x0F, 0x05]);
        self
    }

    /// call rel32 на текущий адрес + смещение (заполняется позже)
    pub fn call_placeholder(&mut self) -> usize {
        self.code.extend_from_slice(&[0xE8, 0, 0, 0, 0]);
        self.code.len() - 4
    }

    pub fn patch_rel32(&mut self, at: usize, target: u64) -> &mut Self {
        let next = self.entry + at as u64 + 4;
        let rel = (target as i64 - next as i64) as i32;
        self.code[at..at + 4].copy_from_slice(&rel.to_le_bytes());
        self
    }

    /// exit(code) через Orbis syscall 1
    pub fn exit(&mut self, code: u32) -> &mut Self {
        self.mov_imm32(0, 1); // exit
        self.mov_imm32(7, code); // rdi
        self.syscall()
    }

    /// Собрать ELF64 (ET_EXEC, два PT_LOAD: код+rodata и данные).
    pub fn build(&self) -> Vec<u8> {
        let code_vaddr = self.entry;
        let ro_vaddr = 0x41_0000u64;
        let data_vaddr = 0x42_0000u64;

        let header_len = 64 + 56 * 3;
        let mut file = vec![0u8; header_len];
        // ELF-заголовок
        file[0..4].copy_from_slice(b"\x7fELF");
        file[4] = 2; // 64 бита
        file[5] = 1; // little-endian
        file[6] = 1; // версия
        file[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        file[18..20].copy_from_slice(&62u16.to_le_bytes()); // x86-64
        file[20..24].copy_from_slice(&1u32.to_le_bytes());
        file[24..32].copy_from_slice(&self.entry.to_le_bytes());
        let phoff = 64u64;
        file[32..40].copy_from_slice(&phoff.to_le_bytes());
        file[52..54].copy_from_slice(&64u16.to_le_bytes()); // ehsize
        file[54..56].copy_from_slice(&56u16.to_le_bytes()); // phentsize
        file[56..58].copy_from_slice(&3u16.to_le_bytes()); // phnum

        let mut off = header_len;
        let code_off = off;
        off += self.code.len();
        let ro_off = off;
        off += self.rodata.len();
        let data_off = off;
        let _ = off;

        let add_ph = |file: &mut Vec<u8>, idx: usize, flags: u32, offset: u64, vaddr: u64, size: u64| {
            let o = 64 + idx * 56;
            file[o..o + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
            file[o + 4..o + 8].copy_from_slice(&flags.to_le_bytes());
            file[o + 8..o + 16].copy_from_slice(&offset.to_le_bytes());
            file[o + 16..o + 24].copy_from_slice(&vaddr.to_le_bytes());
            file[o + 24..o + 32].copy_from_slice(&vaddr.to_le_bytes());
            file[o + 32..o + 40].copy_from_slice(&size.to_le_bytes());
            file[o + 40..o + 48].copy_from_slice(&size.to_le_bytes());
            file[o + 48..o + 56].copy_from_slice(&0x1000u64.to_le_bytes());
        };
        let n = self.code.len() as u64;
        let nr = self.rodata.len() as u64;
        let nd = self.data.len() as u64;
        add_ph(&mut file, 0, 5, code_off as u64, code_vaddr, n);
        add_ph(&mut file, 1, 4, ro_off as u64, ro_vaddr, nr);
        add_ph(&mut file, 2, 6, data_off as u64, data_vaddr, nd);

        let mut file = file;
        file.extend_from_slice(&self.code);
        file.extend_from_slice(&self.rodata);
        file.extend_from_slice(&self.data);
        file
    }
}

impl Default for GuestBuilder {
    fn default() -> Self {
        Self::new()
    }
}
