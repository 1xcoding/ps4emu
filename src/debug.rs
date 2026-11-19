//! Отладчик: точки останова, пошаговое исполнение, просмотр регистров
//! и памяти, обратные трассировки, «watchpoints».

use crate::emulator::Emulator;
use std::io::{self, BufRead, Write};

#[derive(Debug, Clone, PartialEq)]
pub enum DebugAction {
    Continue,
    Step(usize),
    Quit,
    Restart,
}

/// Результат работы отладчика.
pub struct DebugSession {
    pub steps_done: u64,
    pub commands: Vec<String>,
}

pub struct Debugger {
    pub emulator: Emulator,
    pub echo: bool,
    pub log: Vec<String>,
}

impl Debugger {
    pub fn new(emu: Emulator) -> Self {
        Debugger { emulator: emu, echo: true, log: Vec::new() }
    }

    /// Интерактивный сеанс (stdin/stdout).
    pub fn run_interactive(&mut self) -> Result<(), String> {
        println!("ps4emu debugger. Введите 'help' для списка команд.");
        let stdin = io::stdin();
        let mut lines = stdin.lock().lines();
        loop {
            print!("(emu) ");
            let _ = io::stdout().flush();
            let line = match lines.next() {
                Some(Ok(l)) => l,
                _ => break,
            };
            let out = self.command(&line);
            print!("{}", out);
            if line.trim() == "quit" || line.trim() == "q" {
                break;
            }
        }
        Ok(())
    }

    /// Выполнить одну команду отладчика, вернуть текстовый вывод.
    pub fn command(&mut self, line: &str) -> String {
        let parts: Vec<&str> = line.trim().split_whitespace().collect();
        if parts.is_empty() {
            return String::new();
        }
        self.log.push(line.to_string());
        match parts[0] {
            "help" | "h" | "?" => HELP.to_string(),
            "step" | "s" | "si" => {
                let n: usize = parts.get(1).and_then(|x| x.parse().ok()).unwrap_or(1);
                let mut out = String::new();
                for _ in 0..n {
                    match self.emulator.cpu.step(&mut self.emulator.mem) {
                        Ok(_) => {}
                        Err(e) => {
                            out.push_str(&format!("ошибка: {}\n", e));
                            break;
                        }
                    }
                }
                out.push_str(&self.regs());
                out
            }
            "next" | "n" => self.run_until_return(),
            "continue" | "c" | "run" => self.run_until_stop(),
            "regs" | "r" => self.regs(),
            "bt" | "backtrace" => {
                let mut out = format!("стек вызовов (RIP={:#x}):\n", self.emulator.cpu.regs.rip);
                out.push_str(&self.emulator.guest_backtrace());
                out
            }
            "x" | "mem" | "examine" => {
                let addr = parts.get(1).and_then(|s| parse_addr(s)).unwrap_or(0);
                let n: u64 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(64);
                self.emulator.mem.hexdump(addr, n)
            }
            "disas" | "d" | "u" => {
                let addr = parts.get(1).and_then(|s| parse_addr(s)).unwrap_or(self.emulator.cpu.regs.rip);
                self.disasm(addr, parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(8))
            }
            "break" | "b" => {
                let addr = parts.get(1).and_then(|s| parse_addr(s)).unwrap_or(0);
                self.emulator.breakpoints.push(addr);
                format!("точка останова добавлена: {:#x}\n", addr)
            }
            "delete" | "clear" => {
                if let Some(a) = parts.get(1).and_then(|s| parse_addr(s)) {
                    self.emulator.breakpoints.retain(|x| *x != a);
                    format!("точка останова удалена: {:#x}\n", a)
                } else {
                    self.emulator.breakpoints.clear();
                    "все точки останова удалены\n".into()
                }
            }
            "info" => match parts.get(1).copied() {
                Some("mem") => self.emulator.memory_map(),
                Some("break") | Some("b") => {
                    let mut s = String::from("точки останова:\n");
                    for b in &self.emulator.breakpoints {
                        s.push_str(&format!("  {:#x}\n", b));
                    }
                    s
                }
                Some("sym") => {
                    let mut s = String::new();
                    for m in &self.emulator.modules {
                        s.push_str(&format!(
                            "{:<32} {:#x}..{:#x} точка входа {:#x}\n",
                            m.name, m.base, m.end, m.entry
                        ));
                    }
                    for (addr, name) in self.emulator.stub_slots.iter().take(40) {
                        s.push_str(&format!("  заглушка {:#x} -> {}\n", addr, name));
                    }
                    s
                }
                _ => "info mem | info break | info sym\n".into(),
            },
            "watch" => {
                let addr = parts.get(1).and_then(|s| parse_addr(s)).unwrap_or(0);
                let len = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
                self.emulator.mem.watchpoints.push(crate::mem::Watchpoint { addr, len, hits: 0 });
                format!("наблюдение за {:#x} ({} байт)\n", addr, len)
            }
            "watches" => {
                let mut s = String::new();
                for w in &self.emulator.mem.watchpoints {
                    s.push_str(&format!("{:#x} +{}: {} обращений\n", w.addr, w.len, w.hits));
                }
                if s.is_empty() {
                    s.push_str("наблюдений нет\n");
                }
                s
            }
            "symbol" | "sym" => {
                let a = parts.get(1).and_then(|s| parse_addr(s)).unwrap_or(self.emulator.cpu.regs.rip);
                match self.emulator.symbols.resolve(a) {
                    Some(s) => format!("{:#x} = {}\n", a, s),
                    None => format!("{:#x}: символ не найден\n", a),
                }
            }
            "stats" => self.emulator.stats_report(),
            "reset" => {
                let entry = self.emulator.modules.last().map(|m| m.entry).unwrap_or(0);
                let sp = self.emulator.cpu.regs.gpr[4];
                self.emulator.cpu.reset(entry, sp);
                "процессор сброшен\n".into()
            }
            "quit" | "q" | "exit" => "выход\n".into(),
            other => format!("неизвестная команда: {}. 'help' — справка\n", other),
        }
    }

    fn regs(&self) -> String {
        let c = &self.emulator.cpu;
        let names = ["rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi"];
        let mut s = String::new();
        for (i, n) in names.iter().enumerate() {
            s.push_str(&format!("{:<4} {:#018x}  ", n, c.regs.gpr[i]));
            if i % 2 == 1 {
                s.push('\n');
            }
        }
        for i in 8..16 {
            s.push_str(&format!("r{:<3} {:#018x}  ", i, c.regs.gpr[i]));
            if i % 2 == 1 {
                s.push('\n');
            }
        }
        s.push_str(&format!(
            "rip  {:#018x}  rflags {:#x}  fs {:#x}\n",
            c.regs.rip, c.regs.rflags, c.regs.fs_base
        ));
        s.push_str(&format!(
            "xmm0 {:#034x}\nxmm1 {:#034x}\n",
            c.regs.xmm[0], c.regs.xmm[1]
        ));
        s
    }

    fn disasm(&mut self, addr: u64, n: usize) -> String {
        let mut s = String::new();
        let mut pc = addr;
        for _ in 0..n {
            let mut b = [0u8; 8];
            let got = self.emulator.mem.read_partial(pc, &mut b);
            if got == 0 {
                s.push_str(&format!("{:#x}: <не отображено>\n", pc));
                break;
            }
            let sym = self.emulator.symbols.resolve(pc).unwrap_or_default();
            s.push_str(&format!(
                "{:016x}  {:<32} {}\n",
                pc,
                crate::trace::disasm(&b, pc),
                sym
            ));
            pc += estimate_len(&b) as u64;
        }
        s
    }

    fn run_until_stop(&mut self) -> String {
        // Останавливаемся на первой точке останова или при завершении.
        let max = 50_000_000u64;
        for _ in 0..max {
            let rip = self.emulator.cpu.regs.rip;
            if self.emulator.breakpoints.contains(&rip) {
                return format!("остановка на точке {:#x}\n{}", rip, self.regs());
            }
            if rip >= crate::emulator::STUB_BASE
                && self.emulator.stub_slots.contains_key(&rip)
            {
                let name = self.emulator.stub_slots.get(&rip).cloned().unwrap_or_default();
                if let Err(e) = self.emulator.call_hle(&name) {
                    return format!("HLE {} остановлен: {:?}\n", name, std::mem::discriminant(&e));
                }
                continue;
            }
            if match self.emulator.cpu.step(&mut self.emulator.mem) {
                Ok(crate::cpu::Step::Syscall) => {
                    self.emulator.do_syscall();
                    false
                }
                Ok(_) => false,
                Err(_) => true,
            } {
                return "процесс остановлен с ошибкой\n".to_string();
            }
            if self.emulator.sys.exited {
                return format!("программа завершилась, код {}\n", self.emulator.sys.exit_code);
            }
        }
        "достигнут лимит исполнения\n".into()
    }

    fn run_until_return(&mut self) -> String {
        let sp = self.emulator.cpu.regs.gpr[4];
        let ret = self.emulator.mem.read_u64(sp).unwrap_or(0);
        for _ in 0..50_000_000u64 {
            if self.emulator.cpu.regs.rip == ret {
                return format!("возврат в {:#x}\n{}", ret, self.regs());
            }
            match self.emulator.cpu.step(&mut self.emulator.mem) {
                Ok(crate::cpu::Step::Syscall) => self.emulator.do_syscall(),
                Ok(_) => {}
                Err(e) => return format!("ошибка: {}\n", e),
            }
            if self.emulator.sys.exited {
                return "программа завершилась\n".into();
            }
        }
        "лимит исполнения\n".into()
    }
}

fn parse_addr(s: &str) -> Option<u64> {
    let t = s.trim();
    if let Some(h) = t.strip_prefix("0x") {
        return u64::from_str_radix(h, 16).ok();
    }
    // Только цифры 0-9 — десятичное; есть буквы a-f — шестнадцатеричное.
    if t.chars().all(|c| c.is_ascii_digit()) {
        t.parse().ok()
    } else if t.chars().all(|c| c.is_ascii_hexdigit()) {
        u64::from_str_radix(t, 16).ok()
    } else {
        None
    }
}

fn estimate_len(b: &[u8]) -> usize {
    for i in 0..b.len() {
        match b[i] {
            0x40..=0x4f | 0x66 | 0x67 | 0xf2 | 0xf3 | 0x64 | 0x65 => continue,
            0x0f => return i + 2,
            0xe8 | 0xe9 => return i + 5,
            0xeb | 0x70..=0x7f => return i + 2,
            0xb8..=0xbf | 0x68 => return i + 5,
            0x6a => return i + 2,
            0x50..=0x5f | 0x90 | 0xc3 | 0xc9 | 0xcc | 0xf4 => return i + 1,
            _ => {
                let m = b.get(i + 1).copied().unwrap_or(0);
                let mut len = i + 2;
                if m >> 6 != 3 {
                    if m & 7 == 4 {
                        len += 1;
                    }
                    if m >> 6 == 1 {
                        len += 1;
                    } else if m >> 6 == 2 || (m & 7) == 5 {
                        len += 4;
                    }
                }
                return len;
            }
        }
    }
    1
}

pub const HELP: &str = "\
команды отладчика:
  step [n]        выполнить n инструкций
  next            выполнить до возврата из текущей функции
  continue        продолжать до точки останова/остановки
  regs            показать регистры
  bt              стек вызовов гостя
  x <адрес> [n]   дамп памяти
  disas <адрес> [n] дизассемблирование
  break <адрес>   точка останова
  delete [адрес]  удалить точку останова
  watch <адрес> [n] наблюдение за записью (watchpoint)
  watches         список наблюдений
  info mem|break|sym
  symbol <адрес>  найти символ
  stats           статистика эмуляции
  reset           сбросить процессор
  quit            выход
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn debugger_commands_work() {
        let cfg = Config::default();
        let emu = Emulator::new(cfg).unwrap();
        let mut dbg = Debugger::new(emu);
        assert!(dbg.command("help").contains("команды отладчика"));
        assert!(dbg.command("regs").contains("rip"));
        let out = dbg.command("mem 0 16");
        assert!(out.is_empty() || out.contains("0000"));
        assert!(dbg.command("info mem").contains("stack"));
    }

    #[test]
    fn breakpoint_add_remove() {
        let emu = Emulator::new(Config::default()).unwrap();
        let mut dbg = Debugger::new(emu);
        dbg.command("break 0x401000");
        assert_eq!(dbg.emulator.breakpoints, vec![0x401000]);
        dbg.command("delete 0x401000");
        assert!(dbg.emulator.breakpoints.is_empty());
    }

    #[test]
    fn parse_addresses() {
        assert_eq!(parse_addr("0x10"), Some(16));
        assert_eq!(parse_addr("4096"), Some(4096));
        assert_eq!(parse_addr("401000"), Some(401000));
        assert_eq!(parse_addr("0x401000"), Some(0x401000));
    }
}
