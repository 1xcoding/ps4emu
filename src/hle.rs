//! HLE (High Level Emulation) — реализация функций системных библиотек
//! PS4 на стороне эмулятора: libkernel, libSceLibcInternal, libSceSysmodule,
//! libSceUserService и др.
//!
//! Вызов происходит, когда гость исполняет `call` на адрес, выданный
//! загрузчику вместо реальной библиотеки. Эмулятор забирает аргументы из
//! регистров/стека, выполняет действие и возвращает управление.

use crate::mem::Memory;
use crate::sys::{FileObj, Sys};
use std::collections::{BTreeMap, BTreeSet};

pub struct Hle {
    /// Имена импортируемых функций, для которых есть реализация.
    pub supported: BTreeSet<&'static str>,
    /// Счётчик вызовов по имени.
    pub calls: BTreeMap<String, u64>,
    /// Вызовы без реализации (для отчёта).
    pub missing: BTreeSet<String>,
    pub trace: bool,
    /// Потокобезопасная «куча» HLE.
    pub heap_next: u64,
    pub heap_end: u64,
    /// Таблица «открытых» псевдо-дескрипторов.
    pub ids: BTreeMap<String, u64>,
    pub next_id: u64,
    /// Адреса заглушек, зарегистрированных загрузчиком (имя -> адрес).
    pub exports: BTreeMap<String, u64>,
}

const HEAP_BASE: u64 = 0x8000_0000;
const HEAP_SIZE: u64 = 0x1000_0000;

impl Default for Hle {
    fn default() -> Self {
        Self::new()
    }
}

impl Hle {
    pub fn new() -> Self {
        let mut h = Hle {
            supported: BTreeSet::new(),
            calls: BTreeMap::new(),
            missing: BTreeSet::new(),
            trace: false,
            heap_next: HEAP_BASE,
            heap_end: HEAP_BASE + HEAP_SIZE,
            ids: BTreeMap::new(),
            next_id: 0x100,
            exports: BTreeMap::new(),
        };
        for n in SUPPORTED {
            h.supported.insert(n);
        }
        h
    }

    /// Добавить функцию в набор поддерживаемых (для тестов и расширений).
    #[allow(dead_code)]
    pub fn insert(&mut self, name: &'static str) {
        self.supported.insert(name);
    }

    fn id(&mut self, key: &str) -> u64 {
        if let Some(v) = self.ids.get(key) {
            return *v;
        }
        let v = self.next_id;
        self.next_id += 1;
        self.ids.insert(key.to_string(), v);
        v
    }

    fn alloc(&mut self, len: u64, align: u64) -> u64 {
        let align = align.max(16);
        let p = (self.heap_next + align - 1) & !(align - 1);
        if p + len > self.heap_end {
            return 0;
        }
        self.heap_next = p + len;
        p
    }
}

/// Аргументы HLE-вызова.
pub struct Args<'a> {
    pub regs: [u64; 6],
    pub mem: &'a mut Memory,
    /// Адрес, с которого начинаются аргументы, не влезшие в регистры
    /// (обычно rsp + 8 после возврата).
    pub stack: u64,
}

impl<'a> Args<'a> {
    pub fn get(&mut self, i: usize) -> u64 {
        if i < 6 {
            self.regs[i]
        } else {
            self.mem.read_u64(self.stack + (i as u64 - 6) * 8).unwrap_or(0)
        }
    }
    pub fn get_i32(&mut self, i: usize) -> i32 {
        self.get(i) as u32 as i32
    }
    pub fn ptr(&mut self, i: usize) -> u64 {
        self.get(i)
    }
    pub fn cstr(&mut self, i: usize) -> String {
        let p = self.get(i);
        if p == 0 {
            return String::new();
        }
        self.mem.read_cstr(p, 4096).unwrap_or_default()
    }
}

/// Ошибка выполнения HLE.
#[derive(Debug, Clone)]
pub enum HleError {
    /// Функция не реализована.
    Unsupported,
    /// Функция запросила остановку эмуляции.
    Stop(String),
}

pub type HleResult = Result<u64, HleError>;

impl From<crate::mem::MemError> for HleError {
    fn from(_e: crate::mem::MemError) -> Self {
        // Гостевая программа передала неверный указатель: возвращаем EFAULT.
        HleError::Stop("недопустимый указатель в HLE-вызове".into())
    }
}

fn ok(v: u64) -> HleResult {
    Ok(v)
}
fn errno(e: i32) -> HleResult {
    Ok((e as i64 as u64) & 0xFFFF_FFFF | 0xFFFF_FFFF_0000_0000)
}

/// Главная точка входа: выполнить HLE-функцию по имени.
pub fn call(name: &str, a: &mut Args, sys: &mut Sys, hle: &mut Hle) -> HleResult {
    let key = normalize(name);
    *hle.calls.entry(key.clone()).or_insert(0) += 1;
    if hle.trace {
        let args: Vec<u64> = (0..6).map(|i| a.regs[i]).collect();
        sys.log(format!("HLE {}({:x?})", name, args));
    }
    let r = dispatch(&key, a, sys, hle);
    if matches!(r, Ok(0) | Err(HleError::Unsupported)) {
        // ничего не делаем, счётчик уже обновлён
    }
    if let Err(HleError::Unsupported) = r {
        hle.missing.insert(name.to_string());
    }
    r
}

fn normalize(name: &str) -> String {
    let mut s = name.trim().to_string();
    for suffix in ["@libc.so", "@libkernel.so"] {
        if let Some(i) = s.find(suffix) {
            s.truncate(i);
        }
    }
    s
}

macro_rules! bail_unsupported {
    () => {
        return Err(HleError::Unsupported)
    };
}

fn dispatch(name: &str, a: &mut Args, sys: &mut Sys, hle: &mut Hle) -> HleResult {
    // ---------- память ----------
    match name {
        "malloc" | "calloc" | "realloc" | "memalign" | "posix_memalign" | "__sys_malloc"
        | "__libc_malloc" | "_Znwm" | "_Znam" | "operator new" | "operator new[]" => {
            let (size, align) = if name == "calloc" {
                let n = a.get(0);
                let sz = a.get(1);
                (n.saturating_mul(sz), 16)
            } else if name == "memalign" || name == "posix_memalign" {
                let al = a.get(0).max(16);
                let sz = a.get(1);
                if name == "posix_memalign" {
                    // int posix_memalign(void **memptr, size_t alignment, size_t size)
                    let ptrp = a.get(0);
                    let al = a.get(1).max(16);
                    let sz = a.get(2);
                    let p = hle.alloc(sz, al);
                    if p == 0 {
                        return errno(crate::sys::ENOMEM);
                    }
                    a.mem.write_u64(ptrp, p)?;
                    return ok(0);
                }
                (sz, al)
            } else if name == "realloc" {
                // возвращаем новый блок и копируем старый
                let old = a.get(0);
                let sz = a.get(1);
                let p = hle.alloc(sz, 16);
                if p != 0 && old != 0 {
                    let mut buf = vec![0u8; sz.min(1 << 20) as usize];
                    let got = a.mem.read_partial(old, &mut buf);
                    a.mem.write_bytes(p, &buf[..got]).ok();
                }
                return ok(p);
            } else {
                (a.get(0), 16)
            };
            if size == 0 {
                return ok(hle.alloc(16, 16));
            }
            let p = hle.alloc(size, align);
            if p != 0 {
                let _ = a.mem.map(p & !0xFFF, ((size + 0xFFF) & !0xFFF).max(0x1000), 3, "hle-heap");
                // обнуление
                let zero = vec![0u8; size.min(1 << 20) as usize];
                let _ = a.mem.write_bytes(p, &zero);
                sys.allocations.insert(p, size);
            }
            return ok(p);
        }
        "free" | "cfree" | "_ZdlPv" | "_ZdaPv" | "operator delete" => {
            let p = a.get(0);
            if let Some(sz) = sys.allocations.remove(&p) {
                let _ = a.mem.unmap(p & !0xFFF, ((sz + 0xFFF) & !0xFFF).max(0x1000));
            }
            return ok(0);
        }
        "posix_memalign_ret" => return ok(0),
        _ => {}
    }

    // ---------- строки и память libc ----------
    match name {
        "memcpy" | "memmove" | "__memcpy" => {
            let dst = a.get(0);
            let src = a.get(1);
            let n = a.get(2) as usize;
            let mut buf = vec![0u8; n.min(1 << 26)];
            let got = a.mem.read_partial(src, &mut buf);
            a.mem.write_bytes(dst, &buf[..got]).ok();
            return ok(dst);
        }
        "memset" | "bzero" | "explicit_bzero" => {
            let dst = a.get(0);
            let c = a.get(1) as u8;
            let n = a.get(2) as usize;
            let buf = vec![c; n.min(1 << 26)];
            a.mem.write_bytes(dst, &buf).ok();
            return ok(dst);
        }
        "memcmp" | "bcmp" => {
            let p1 = a.get(0);
            let p2 = a.get(1);
            let n = a.get(2);
            for i in 0..n {
                let x = a.mem.read_u8(p1 + i).unwrap_or(0);
                let y = a.mem.read_u8(p2 + i).unwrap_or(0);
                if x != y {
                    return ok((x as i64 - y as i64) as u64);
                }
            }
            return ok(0);
        }
        "strlen" | "__strlen" => {
            let p = a.get(0);
            let mut n = 0u64;
            while n < (1 << 24) {
                if a.mem.read_u8(p + n).unwrap_or(0) == 0 {
                    break;
                }
                n += 1;
            }
            return ok(n);
        }
        "strnlen" => {
            let p = a.get(0);
            let max = a.get(1);
            let mut n = 0u64;
            while n < max {
                if a.mem.read_u8(p + n).unwrap_or(0) == 0 {
                    break;
                }
                n += 1;
            }
            return ok(n);
        }
        "strcmp" | "strcoll" => {
            let p1 = a.get(0);
            let p2 = a.get(1);
            let mut i = 0u64;
            loop {
                let x = a.mem.read_u8(p1 + i).unwrap_or(0);
                let y = a.mem.read_u8(p2 + i).unwrap_or(0);
                if x != y || x == 0 || i > (1 << 20) {
                    return ok((x as i64 - y as i64) as u64);
                }
                i += 1;
            }
        }
        "strncmp" => {
            let p1 = a.get(0);
            let p2 = a.get(1);
            let n = a.get(2);
            for i in 0..n {
                let x = a.mem.read_u8(p1 + i).unwrap_or(0);
                let y = a.mem.read_u8(p2 + i).unwrap_or(0);
                if x != y || x == 0 {
                    return ok((x as i64 - y as i64) as u64);
                }
            }
            return ok(0);
        }
        "strcpy" | "stpcpy" => {
            let dst = a.get(0);
            let src = a.get(1);
            let s = a.mem.read_cstr(src, 1 << 20)?;
            a.mem.write_cstr(dst, &s).ok();
            return ok(dst + s.len() as u64);
        }
        "strncpy" => {
            let dst = a.get(0);
            let src = a.get(1);
            let n = a.get(2);
            let s = a.mem.read_cstr(src, 1 << 20)?;
            let bytes = s.as_bytes();
            for i in 0..n {
                let b = *bytes.get(i as usize).unwrap_or(&0);
                a.mem.write_u8(dst + i, b).ok();
            }
            return ok(dst);
        }
        "strcat" | "strncat" => {
            let dst = a.get(0);
            let src = a.get(1);
            let n = if name == "strncat" { a.get(2) } else { u64::MAX };
            let d = a.mem.read_cstr(dst, 1 << 20)?;
            let s = a.mem.read_cstr(src, 1 << 20)?;
            let add: String = s.chars().take(n as usize).collect();
            a.mem.write_cstr(dst + d.len() as u64, &add).ok();
            return ok(dst);
        }
        "strchr" | "index" => {
            let p = a.get(0);
            let c = a.get(1) as u8;
            let mut i = 0u64;
            loop {
                let b = a.mem.read_u8(p + i).unwrap_or(0);
                if b == c {
                    return ok(p + i);
                }
                if b == 0 || i > (1 << 22) {
                    return ok(0);
                }
                i += 1;
            }
        }
        "strrchr" | "rindex" => {
            let p = a.get(0);
            let c = a.get(1) as u8;
            let s = a.mem.read_cstr(p, 1 << 22)?;
            match s.as_bytes().iter().rposition(|&b| b == c) {
                Some(i) => return ok(p + i as u64),
                None => return ok(0),
            }
        }
        "strstr" => {
            let h = a.cstr(0);
            let n = a.cstr(1);
            match h.find(&n) {
                Some(i) => return ok(a.get(0) + i as u64),
                None => return ok(0),
            }
        }
        "strdup" => {
            let s = a.cstr(0);
            let p = hle.alloc(s.len() as u64 + 1, 16);
            a.mem.write_cstr(p, &s).ok();
            return ok(p);
        }
        "strtod" | "atof" => {
            let s = a.cstr(0);
            let v: f64 = s.trim().parse().unwrap_or(0.0);
            if name == "strtod" {
                let end = a.get(1);
                let base = a.get(0);
                if end != 0 {
                    a.mem.write_u64(end, base + s.len() as u64).ok();
                }
            }
            return ok(v.to_bits());
        }
        "atoi" | "atol" | "atoll" => {
            let s = a.cstr(0);
            return ok(s.trim().parse::<i64>().unwrap_or(0) as u64);
        }
        "strtol" | "strtoll" | "strtoul" | "strtoull" => {
            let s = a.cstr(0);
            let base = a.get(2) as u32;
            let endp = a.get(1);
            let trimmed = s.trim_start();
            let neg = trimmed.starts_with('-');
            let digits: String = trimmed
                .trim_start_matches(['-', '+', ' '])
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            let v = i128::from_str_radix(&digits, if base == 0 { 10 } else { base }).unwrap_or(0);
            let v = if neg { -v } else { v } as i64;
            if endp != 0 {
                let used = trimmed.len() - trimmed.trim_start_matches(['-', '+', ' ']).len()
                    + digits.len();
                let base_ptr = a.get(0);
                a.mem.write_u64(endp, base_ptr + used as u64).ok();
            }
            return ok(v as u64);
        }
        "abs" | "labs" | "llabs" => {
            let v = a.get(0) as i64;
            return ok(v.wrapping_abs() as u64);
        }
        _ => {}
    }

    // ---------- ввод/вывод ----------
    match name {
        "printf" | "puts" | "putchar" | "fputs" | "fwrite" | "write" => {
            let s = match name {
                "puts" => format!("{}\n", a.cstr(0)),
                "putchar" => {
                    let c = a.get(0) as u8 as char;
                    format!("{}", c)
                }
                "fputs" => a.cstr(0),
                "fwrite" => {
                    let p = a.get(0);
                    let size = a.get(1);
                    let n = a.get(2);
                    let mut buf = vec![0u8; (size * n).min(1 << 22) as usize];
                    let got = a.mem.read_partial(p, &mut buf);
                    buf.truncate(got);
                    String::from_utf8_lossy(&buf).into_owned()
                }
                "write" => {
                    let p = a.get(1);
                    let n = a.get(2);
                    let mut buf = vec![0u8; n.min(1 << 22) as usize];
                    let got = a.mem.read_partial(p, &mut buf);
                    buf.truncate(got);
                    String::from_utf8_lossy(&buf).into_owned()
                }
                _ => {
                    let fmt = a.cstr(0);
                    format_args(a, &fmt, 1)?
                }
            };
            print!("{}", s);
            sys.stdout.push_str(&s);
            if name == "printf" || name == "puts" {
                return ok(s.len() as u64);
            } else if name == "putchar" {
                return ok(a.get(0) & 0xFF);
            } else {
                return ok(s.len() as u64);
            }
        }
        "fprintf" | "sprintf" | "snprintf" | "vsnprintf" | "vsprintf" | "asprintf" => {
            let (dst, max, fmt, first) = match name {
                "fprintf" => {
                    let fmt = a.cstr(1);
                    let s = format_args(a, &fmt, 2)?;
                    let _ = eprint!("{}", s);
                    sys.stdout.push_str(&s);
                    return ok(s.len() as u64);
                }
                "sprintf" => (a.get(0), u64::MAX, a.cstr(1), 2),
                "snprintf" => (a.get(0), a.get(1), a.cstr(2), 3),
                "vsnprintf" => {
                    let dst = a.get(0);
                    let max = a.get(1);
                    let fmt = a.cstr(2);
                    let va = a.get(3);
                    let s = format_va(a, &fmt, va)?;
                    write_cstr_limited(a, dst, &s, max);
                    return ok(s.len() as u64);
                }
                "vsprintf" => {
                    let dst = a.get(0);
                    let fmt = a.cstr(1);
                    let va = a.get(2);
                    let s = format_va(a, &fmt, va)?;
                    a.mem.write_cstr(dst, &s).ok();
                    return ok(s.len() as u64);
                }
                _ => (a.get(0), u64::MAX, a.cstr(1), 2),
            };
            let s = format_args(a, &fmt, first)?;
            write_cstr_limited(a, dst, &s, max);
            return ok(s.len() as u64);
        }
        "scanf" | "sscanf" | "fscanf" => {
            // Возвращаем 0 прочитанных элементов: надежда, что игра это стерпит.
            return ok(0);
        }
        "setvbuf" | "setbuf" | "fflush" | "fclose" | "fileno" | "freopen" => {
            if name == "fileno" {
                return ok(a.get(0));
            }
            return ok(0);
        }
        "fopen" | "fopen64" => {
            let path = a.cstr(0);
            let mode = a.cstr(1);
            let wr = mode.contains('w') || mode.contains('a') || mode.contains('+');
            if wr {
                sys.vfs.insert(path.clone(), Vec::new());
            }
            let fd = match sys.vfs.get(&path).cloned() {
                Some(data) => {
                    let fd = sys.next_fd;
                    sys.next_fd += 1;
                    sys.open_files.insert(fd, FileObj::Mem { path, data, pos: 0, writable: wr });
                    fd
                }
                None => return ok(0),
            };
            // Возвращаем псевдо-FILE* — собственный указатель, пригодный для fwrite/fclose.
            let handle = 0x1000_0000u64 + fd as u64 * 0x100;
            hle.ids.insert(format!("FILE:{}", fd), handle);
            return ok(handle);
        }
        "fread" | "fgets" => {
            // Чтение из псевдо-FILE* не поддерживается (возвращаем 0).
            return ok(0);
        }
        "perror" | "strerror" => {
            let s = if name == "perror" {
                format!("{}: ошибка\n", a.cstr(0))
            } else {
                format!("ошибка {}", a.get(0) as i32)
            };
            sys.stdout.push_str(&s);
            if name == "strerror" {
                let p = hle.alloc(s.len() as u64 + 1, 8);
                a.mem.write_cstr(p, &s).ok();
                return ok(p);
            }
            return ok(0);
        }
        _ => {}
    }

    // ---------- libkernel: процессы, потоки, память ----------
    match name {
        "sceKernelGetProcessId" | "getpid" | "_getpid" => return ok(sys.pid as u64),
        "sceKernelGetThreadId" | "gettid" | "pthread_self" | "_pthread_self" => {
            return ok(sys.tid as u64);
        }
        "sceKernelGetCurrentCpu" => return ok(0),
        "sceKernelGetProcessTime" | "sceKernelGetProcessTimeLow" => {
            let el = sys.start_time.elapsed().unwrap_or_default();
            return ok(el.as_micros() as u64);
        }
        "sceKernelGetProcessTimeCounter" | "sceKernelGetProcessTimeCounterFrequency" => {
            return ok(1_000_000);
        }
        "sceKernelUsleep" | "usleep" | "sceKernelSleep" => {
            let us = a.get(0).min(100_000);
            std::thread::sleep(std::time::Duration::from_micros(us));
            return ok(0);
        }
        "sceKernelNanosecondsleep" => return ok(0),
        "sceKernelClockGettime" => {
            let clk = a.get(0);
            let ts = a.get(1);
            let el = if clk == 4 { sys.start_time.elapsed().unwrap_or_default() } else {
                use std::time::{SystemTime, UNIX_EPOCH};
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()
            };
            if ts != 0 {
                a.mem.write_u64(ts, el.as_secs())?;
                a.mem.write_u64(ts + 8, el.subsec_nanos() as u64)?;
            }
            return ok(0);
        }
        "sceKernelGettimeofday" | "gettimeofday" | "_gettimeofday" => {
            use std::time::{SystemTime, UNIX_EPOCH};
            let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
            let tp = a.get(0);
            if tp != 0 {
                a.mem.write_u64(tp, t.as_secs())?;
                a.mem.write_u64(tp + 8, t.subsec_micros() as u64)?;
            }
            return ok(0);
        }
        "sceKernelCreateEqueue" | "sceKernelDeleteEqueue" | "sceKernelWaitEqueue"
        | "sceKernelAddUserEvent" | "sceKernelAddUserEventEdge" | "sceKernelTriggerUserEvent"
        | "sceKernelDeleteUserEvent" => {
            if name == "sceKernelCreateEqueue" {
                let out = a.get(0);
                let id = hle.id("equeue");
                if out != 0 {
                    a.mem.write_u32(out, id as u32)?;
                }
                return ok(0);
            }
            if name == "sceKernelWaitEqueue" {
                // Возвращаем таймаут, чтобы цикл не завис.
                let out = a.get(2);
                if out != 0 {
                    a.mem.write_u64(out, 0)?;
                }
                return errno(60); // ETIMEDOUT
            }
            return ok(0);
        }
        "sceKernelCreateSema" | "sceKernelDeleteSema" | "sceKernelWaitSema" | "sceKernelSignalSema"
        | "sceKernelPollSema" => {
            match name {
                "sceKernelCreateSema" => {
                    let out = a.get(0);
                    let id = hle.id("sema");
                    if out != 0 {
                        a.mem.write_u32(out, id as u32)?;
                    }
                    return ok(0);
                }
                "sceKernelWaitSema" => return ok(0),
                "sceKernelPollSema" => return ok(0),
                _ => return ok(0),
            }
        }
        "sceKernelCreateMutex" | "sceKernelDeleteMutex" | "sceKernelLockMutex"
        | "sceKernelUnlockMutex" | "sceKernelTryLockMutex" => {
            if name == "sceKernelCreateMutex" {
                let out = a.get(0);
                let id = hle.id("mutex");
                if out != 0 {
                    a.mem.write_u32(out, id as u32)?;
                }
            }
            return ok(0);
        }
        "scePthreadMutexInit" | "scePthreadMutexDestroy" | "scePthreadMutexLock"
        | "scePthreadMutexTrylock" | "scePthreadMutexUnlock" => {
            if name == "scePthreadMutexInit" {
                let m = a.get(0);
                let id = hle.id("pmutex");
                if m != 0 {
                    a.mem.write_u32(m, id as u32)?;
                }
            }
            if name == "scePthreadMutexTrylock" {
                return ok(0);
            }
            return ok(0);
        }
        "scePthreadCondInit" | "scePthreadCondDestroy" | "scePthreadCondWait"
        | "scePthreadCondSignal" | "scePthreadCondBroadcast" | "scePthreadCondTimedwait" => {
            if name == "scePthreadCondTimedwait" {
                return errno(60);
            }
            return ok(0);
        }
        "scePthreadCreate" | "pthread_create" => {
            let tidp = a.get(0);
            let id = hle.id("thread");
            if tidp != 0 {
                a.mem.write_u32(tidp, id as u32)?;
            }
            sys.threads.push(crate::sys::ThreadInfo {
                id: id as i32,
                name: "worker".into(),
                detached: false,
                exited: false,
            });
            return ok(0);
        }
        "scePthreadJoin" | "scePthreadDetach" | "scePthreadExit" | "pthread_exit" => {
            if name == "scePthreadExit" {
                sys.exited = true;
                return Ok(0);
            }
            return ok(0);
        }
        "scePthreadSetprio" | "scePthreadGetschedparam" | "scePthreadSetschedparam"
        | "scePthreadRename" | "scePthreadAttrInit" | "scePthreadAttrDestroy"
        | "scePthreadAttrSetstacksize" | "scePthreadAttrGetstacksize"
        | "scePthreadAttrSetdetachstate" | "scePthreadAttrSetinheritsched"
        | "scePthreadAttrSetschedpolicy" | "scePthreadAttrSetschedparam" => return ok(0),
        "sceKernelGetThreadName" => {
            let buf = a.get(1);
            if buf != 0 {
                a.mem.write_cstr(buf, "main").ok();
            }
            return ok(0);
        }
        "sceKernelGetThreadInfo" => {
            let out = a.get(1);
            if out != 0 {
                for i in 0..10u64 {
                    a.mem.write_u64(out + i * 8, 0).ok();
                }
                a.mem.write_u64(out, sys.tid as u64).ok();
                a.mem.write_u32(out + 8 + 24, 700).ok();
            }
            return ok(0);
        }
        "sceKernelGetThreadCpuAffinityMask" => return ok(0xFF),
        "sceKernelSetThreadCpuAffinityMask" | "scePthreadSetaffinity" => return ok(0),
        "sceKernelYieldThread" | "sched_yield" | "sceKernelScheduleYield" => return ok(0),

        // Память ядра
        "sceKernelAllocateMainDirectMemory" | "sceKernelAllocateDirectMemory"
        | "sceKernelMapDirectMemory" | "sceKernelMapFlexibleMemory"
        | "sceKernelMapNamedFlexibleMemory" | "sceKernelMapNamedDirectMemory"
        | "sceKernelAllocateFlexibleMemory" => {
            let len = if name.contains("Allocate") { a.get(1) } else { a.get(1) };
            match name {
                "sceKernelAllocateMainDirectMemory" | "sceKernelAllocateDirectMemory"
                | "sceKernelAllocateFlexibleMemory" => {
                    let out = a.get(4);
                    let p = hle.alloc(len, 0x4000);
                    if out != 0 {
                        a.mem.write_u64(out, p)?;
                    }
                    let phys = 0x2000_0000u64 + (p & 0x0FFF_FFFF);
                    return ok(phys);
                }
                _ => {
                    let out = if name.contains("Named") { a.get(4) } else { a.get(3) };
                    let size = if name == "sceKernelMapDirectMemory" { a.get(1) } else { a.get(1) };
                    let p = hle.alloc(size, 0x4000);
                    a.mem.map(p, (size + 0xFFF) & !0xFFF, 3, "direct")?;
                    if out != 0 {
                        a.mem.write_u64(out, p)?;
                    }
                    return ok(0);
                }
            }
        }
        "sceKernelReleaseDirectMemory" | "sceKernelReleaseFlexibleMemory" => return ok(0),
        "sceKernelGetDirectMemorySize" => return ok(0x1800_0000),
        "sceKernelGetDirectMemoryType" | "sceKernelGetInternalCoredumpLevel" => return ok(0),
        "sceKernelMapNamedSystemFlexibleMemory" => return ok(0),
        "sceKernelMprotect" | "sceKernelMtypeprotect" | "sceKernelMunmap" => return ok(0),

        // Модули
        "sceSysmoduleLoadModule" | "sceSysmoduleLoadModuleInternal"
        | "sceSysmoduleLoadModuleByNameInternal" | "sceSysmoduleIsLoaded"
        | "sceSysmoduleUnloadModule" | "sceSysmoduleIsCalled" => {
            if name == "sceSysmoduleIsLoaded" {
                return ok(0);
            }
            return ok(0);
        }
        "sceKernelLoadStartModule" | "sceKernelLoadModule" | "sceKernelStartModule"
        | "sceKernelStopModule" | "sceKernelUnloadModule" => {
            if name == "sceKernelLoadStartModule" {
                let out = a.get(4);
                let h = hle.id("module");
                if out != 0 {
                    a.mem.write_u32(out, h as u32)?;
                }
            }
            return ok(0);
        }
        "sceKernelDlsym" => {
            let h = a.get(1);
            let sym = a.cstr(2);
            // Возвращаем адрес из таблицы экспорта загрузчика.
            let out = a.get(3);
            let addr = hle.exports.get(&sym).copied().unwrap_or(0);
            let _ = h;
            if out != 0 {
                a.mem.write_u64(out, addr)?;
            }
            if addr == 0 {
                return errno(crate::sys::ENOENT);
            }
            return ok(0);
        }

        // Пользователи/система
        "sceUserServiceInitialize" | "sceUserServiceTerminate"
        | "sceUserServiceGetInitialUser" | "sceUserServiceGetLoginUserIdList"
        | "sceUserServiceGetUserName" | "sceUserServiceGetEvent" => {
            if name == "sceUserServiceGetInitialUser" {
                let out = a.get(0);
                if out != 0 {
                    a.mem.write_i32(out, 0x1000_0001u32 as i32)?;
                }
            }
            if name == "sceUserServiceGetUserName" {
                let buf = a.get(1);
                let len = a.get(2) as usize;
                if buf != 0 {
                    let name = "User_Emu";
                    a.mem.write_bytes(buf, &name.as_bytes()[..name.len().min(len.saturating_sub(1))]).ok();
                }
            }
            return ok(0);
        }
        "sceSystemServiceGetStatus" | "sceSystemServiceParamGetInt"
        | "sceSystemServiceGetAppId" | "sceSystemServiceGetAppTitleId" => return ok(0),
        "sceKernelGetAppInfo" => {
            let out = a.get(3);
            if out != 0 {
                for i in 0..8u64 {
                    a.mem.write_u64(out + i * 8, 0).ok();
                }
            }
            return ok(0);
        }

        // Сохранения / ввод
        "sceSaveDataInitialize" | "sceSaveDataSetupSaveDataMemory"
        | "sceSaveDataGetSaveDataMemory" | "sceSaveDataCommitSaveData" => {
            if name == "sceSaveDataGetSaveDataMemory" {
                let buf = a.get(2);
                let len = a.get(3);
                let zero = vec![0u8; len.min(1 << 20) as usize];
                if buf != 0 {
                    a.mem.write_bytes(buf, &zero).ok();
                }
            }
            return ok(0);
        }
        "scePadInit" | "scePadOpen" | "scePadClose" | "scePadReadState" | "scePadRead"
        | "scePadGetHandle" => {
            if name == "scePadOpen" || name == "scePadGetHandle" {
                return ok(1);
            }
            if name == "scePadReadState" || name == "scePadRead" {
                let out = a.get(1);
                if out != 0 {
                    // Заполняем ScePadData из виртуального геймпада эмулятора.
                    let pad = sys.pad.clone();
                    pad.write_to(a.mem, out)?;
                }
            }
            return ok(0);
        }
        "sceAudioOutInit" | "sceAudioOutOpen" | "sceAudioOutOutput" | "sceAudioOutClose" => {
            if name == "sceAudioOutOpen" {
                return ok(1);
            }
            return ok(0);
        }
        "sceVideoOutOpen" | "sceVideoOutClose" | "sceVideoOutSetBufferAttribute"
        | "sceVideoOutRegisterBuffers" | "sceVideoOutSubmitFlip" | "sceVideoOutWaitVblank"
        | "sceVideoOutGetVblankStatus" => {
            match name {
                "sceVideoOutOpen" => return ok(1),
                "sceVideoOutSetBufferAttribute" => {
                    // sceVideoOutSetBufferAttribute(SceVideoOutBufferAttribute *attr,
                    //   uint32_t pixelFormat, uint32_t tilingMode,
                    //   uint32_t width, uint32_t height, uint32_t pitch)
                    let p = a.get(0);
                    let fmt = a.get(1);
                    let tiling = a.get(2);
                    let w = a.get(3);
                    let h = a.get(4);
                    let pitch = a.get(5);
                    if w > 0 && h > 0 {
                        sys.fb_size = (w as u32, h as u32);
                    }
                    if p != 0 {
                        a.mem.write_u32(p, fmt.max(4) as u32).ok();
                        a.mem.write_u32(p + 4, tiling as u32).ok();
                        a.mem.write_u32(p + 8, w as u32).ok();
                        a.mem.write_u32(p + 12, h as u32).ok();
                        a.mem.write_u32(p + 16, pitch as u32).ok();
                    }
                    return ok(0);
                }
                "sceVideoOutRegisterBuffers" => {
                    // sceVideoOutRegisterBuffers(handle, int startIndex,
                    //   void *const *addresses, int bufferNum, const attr *)
                    let start = a.get(1) as u32;
                    let ptrs = a.get(2);
                    let num = a.get(3) as u32;
                    for i in 0..num.min(16) {
                        if let Ok(addr) = a.mem.read_u64(ptrs + i as u64 * 8) {
                            sys.framebuffers.insert(start + i, addr);
                        }
                    }
                    return ok(0);
                }
                "sceVideoOutSubmitFlip" => {
                    // sceVideoOutSubmitFlip(handle, bufferIndex, flipMode, flipArg)
                    let idx = a.get(1) as u32;
                    if let Some(addr) = sys.framebuffers.get(&idx).copied() {
                        let (w, h) = sys.fb_size;
                        sys.last_flip = Some((addr, w, h));
                    }
                    return ok(0);
                }
                "sceVideoOutGetVblankStatus" => {
                    let out = a.get(1);
                    if out != 0 {
                        a.mem.write_u64(out, 0).ok();
                    }
                    return ok(0);
                }
                _ => return ok(0),
            }
        }
        "sceRtcGetCurrentTick" | "sceRtcGetTick" | "sceRtcGetCurrentClockLocalTime"
        | "sceRtcGetTickResolution" => {
            match name {
                "sceRtcGetCurrentTick" => {
                    let out = a.get(0);
                    let el = sys.start_time.elapsed().unwrap_or_default();
                    if out != 0 {
                        a.mem.write_u64(out, el.as_micros() as u64)?;
                    }
                    return ok(0);
                }
                "sceRtcGetTickResolution" => return ok(1_000_000),
                _ => {
                    let out = a.get(0);
                    if out != 0 {
                        for i in 0..8u64 {
                            a.mem.write_u16(out + i * 2, 0).ok();
                        }
                    }
                    return ok(0);
                }
            }
        }
        "sceNetInit" | "sceNetPoolCreate" | "sceNetCtlInit" | "sceNetGetMacAddress" => return ok(0),
        "sceNetSocket" | "sceNetConnect" | "sceNetSend" | "sceNetRecv" => return ok(0),
        "sceHttpGetStatusCode" | "sceHttpInit" | "sceHttpCreateTemplate" => return ok(0),
        "sceSysUtilSendSystemNotificationWithText" => return ok(0),
        "sceAppContentInitialize" | "sceAppContentGetAddcontInfo" => return ok(0),
        "sceCommonDialogInitialize" | "sceMsgDialogInitialize" | "sceMsgDialogOpen"
        | "sceMsgDialogUpdateStatus" | "sceMsgDialogClose" => {
            if name == "sceMsgDialogUpdateStatus" {
                return ok(3); // SCE_MSG_DIALOG_STATUS_FINISHED
            }
            return ok(0);
        }
        "sceImeDialogInit" | "sceImeDialogGetStatus" => {
            if name == "sceImeDialogGetStatus" {
                return ok(0);
            }
            return ok(0);
        }
        "sceNpScoreInit" | "sceNpManagerGetNpId" | "sceNpManagerGetAccountRegion" => return ok(0),

        _ => {}
    }

    // ---------- C++ рантайм / обработка исключений ----------
    match name {
        "__cxa_atexit" | "__cxa_finalize" | "__aeabi_atexit" | "atexit" | "on_exit" => return ok(0),
        "_Unwind_Resume" | "__gxx_personality_v0" | "_ZSt9terminatev" | "_ZSt10unexpectedv" => {
            Err(HleError::Stop(format!("вызвана C++-функция {}", name)))
        }
        "__stack_chk_fail" | "__stack_chk_guard_check" => {
            Err(HleError::Stop("сработала проверка переполнения стека (__stack_chk_fail)".into()))
        }
        "__assert_fail" | "__assert" => {
            let msg = a.cstr(2);
            Err(HleError::Stop(format!("сработал assert: {}", msg)))
        }
        "abort" | "_exit" | "exit" | "quick_exit" => {
            if name == "exit" {
                sys.exit_code = a.get_i32(0);
            }
            sys.exited = true;
            Ok(0)
        }
        "_ZnwmSt11align_val_t" | "_ZnamSt11align_val_t" => {
            let size = a.get(0);
            let align = a.get(1);
            let p = hle.alloc(size, align.max(16));
            a.mem.map(p & !0xFFF, ((size + 0xFFF) & !0xFFF).max(0x1000), 3, "cpp-heap")?;
            return ok(p);
        }
        "_ZdlPvSt11align_val_t" | "_ZdaPvSt11align_val_t" => return ok(0),
        _ => bail_unsupported!(),
    }
}

fn write_cstr_limited(a: &mut Args, dst: u64, s: &str, max: u64) {
    let bytes = s.as_bytes();
    let n = if max == u64::MAX { bytes.len() } else { (max as usize).saturating_sub(1).min(bytes.len()) };
    let _ = a.mem.write_bytes(dst, &bytes[..n]);
    let _ = a.mem.write_u8(dst + n as u64, 0);
}

/// Форматирование printf-строки; `first` — индекс первого вариативного
/// аргумента в регистрах/стеке.
pub fn format_args(a: &mut Args, fmt: &str, first: usize) -> Result<String, HleError> {
    let mut out = String::new();
    let mut idx = first;
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if i >= chars.len() {
            break;
        }
        if chars[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }
        // флаги
        let mut spec = String::new();
        while i < chars.len() && "-+ #0".contains(chars[i]) {
            spec.push(chars[i]);
            i += 1;
        }
        let mut width = String::new();
        while i < chars.len() && chars[i].is_ascii_digit() {
            width.push(chars[i]);
            i += 1;
        }
        if i < chars.len() && chars[i] == '*' {
            width = a.get(idx).to_string();
            idx += 1;
            i += 1;
        }
        let mut prec_str = String::new();
        if i < chars.len() && chars[i] == '.' {
            i += 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                prec_str.push(chars[i]);
                i += 1;
            }
        }
        // длина
        let mut len = String::new();
        while i < chars.len() && "hljztLq".contains(chars[i]) {
            len.push(chars[i]);
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let conv = chars[i];
        i += 1;
        let w: usize = width.parse().unwrap_or(0);
        let p: usize = prec_str.parse().unwrap_or(0);
        let pad = |s: &str, w: usize, left: bool| -> String {
            if s.len() >= w {
                s.to_string()
            } else if left {
                format!("{:<width$}", s, width = w)
            } else {
                format!("{:>width$}", s, width = w)
            }
        };
        let left = spec.contains('-');
        match conv {
            'd' | 'i' => {
                let v = if len.contains('l') || len.contains('z') || len.contains('t') || len.contains('j') {
                    a.get(idx) as i64
                } else {
                    a.get_i32(idx) as i64
                };
                idx += 1;
                out.push_str(&pad(&v.to_string(), w, left));
            }
            'u' => {
                let v = if len.contains('l') || len.contains('z') { a.get(idx) } else { a.get_i32(idx) as u32 as u64 };
                idx += 1;
                out.push_str(&pad(&v.to_string(), w, left));
            }
            'x' | 'X' | 'o' => {
                let v = if len.contains('l') || len.contains('z') { a.get(idx) } else { a.get_i32(idx) as u32 as u64 };
                idx += 1;
                let s = match conv {
                    'x' => format!("{:x}", v),
                    'X' => format!("{:X}", v),
                    _ => format!("{:o}", v),
                };
                let s = if spec.contains('#') && conv != 'o' && v != 0 {
                    format!("0{}{}", conv, s)
                } else {
                    s
                };
                out.push_str(&pad(&s, w, left));
            }
            'p' => {
                let v = a.get(idx);
                idx += 1;
                out.push_str(&format!("0x{:x}", v));
            }
            'c' => {
                let v = a.get(idx) as u8 as char;
                idx += 1;
                out.push(v);
            }
            's' => {
                let p_addr = a.get(idx);
                idx += 1;
                if p_addr == 0 {
                    out.push_str("(null)");
                } else {
                    let s = a.mem.read_cstr(p_addr, 1 << 20).unwrap_or_default();
                    let s = if prec_str.len() > 0 { s.chars().take(p).collect::<String>() } else { s };
                    out.push_str(&pad(&s, w, left));
                }
            }
            'f' | 'F' | 'g' | 'G' | 'e' | 'E' => {
                let v = f64::from_bits(a.get(idx));
                idx += 1;
                let s = match conv {
                    'f' | 'F' => format!("{:.*}", if prec_str.len() > 0 { p } else { 6 }, v),
                    'e' => format!("{:.*e}", if prec_str.len() > 0 { p } else { 6 }, v),
                    'E' => format!("{:.*E}", if prec_str.len() > 0 { p } else { 6 }, v),
                    _ => {
                        if v == 0.0 {
                            "0".to_string()
                        } else {
                            format!("{}", v)
                        }
                    }
                };
                out.push_str(&pad(&s, w, left));
            }
            'n' => {
                let p_addr = a.get(idx);
                idx += 1;
                let _ = a.mem.write_u32(p_addr, out.len() as u32);
            }
            _ => {
                out.push('%');
                out.push(conv);
            }
        }
    }
    Ok(out)
}

/// Форматирование через `va_list` (x86-64 SysV ABI).
pub fn format_va(a: &mut Args, fmt: &str, va: u64) -> Result<String, HleError> {
    if va == 0 {
        return Ok(fmt.to_string());
    }
    let gp_offset = a.mem.read_u32(va).unwrap_or(48);
    let fp_offset = a.mem.read_u32(va + 4).unwrap_or(304);
    let overflow = a.mem.read_u64(va + 8).unwrap_or(0);
    let reg_save = a.mem.read_u64(va + 16).unwrap_or(0);

    // Собираем «виртуальные» аргументы: 6 целочисленных слотов из reg_save_area,
    // затем — из overflow_arg_area.
    let mut gp = gp_offset as u64;
    let fp = fp_offset as u64;
    let mut ovf = overflow;
    let mut args = [0u64; 6];
    for (i, slot) in args.iter_mut().enumerate() {
        if gp < 48 && reg_save != 0 {
            *slot = a.mem.read_u64(reg_save + gp).unwrap_or(0);
            gp += 8;
        } else {
            *slot = a.mem.read_u64(ovf).unwrap_or(0);
            ovf += 8;
        }
        let _ = i;
    }
    let _ = fp;
    let mut sub = Args { regs: args, mem: a.mem, stack: overflow };
    // Для аргументов за пределами reg_save используем overflow-область.
    sub.stack = if gp >= 48 { ovf } else { overflow };
    format_args(&mut sub, fmt, 0)
}

/// Список поддерживаемых имён (для отчёта и докуменации).
pub const SUPPORTED: &[&str] = &[
    "malloc", "free", "calloc", "realloc", "memalign", "posix_memalign", "__sys_malloc",
    "memcpy", "memset", "memmove", "memcmp", "strlen", "strcmp", "strncmp", "strcpy",
    "strncpy", "strcat", "strchr", "strrchr", "strstr", "strdup", "strtod", "atoi",
    "strtol", "printf", "puts", "putchar", "fputs", "fwrite", "sprintf", "snprintf",
    "vsnprintf", "fprintf", "sceKernelGetProcessId", "sceKernelGetThreadId",
    "sceKernelGetProcessTime", "sceKernelUsleep", "sceKernelGettimeofday",
    "sceKernelCreateEqueue", "sceKernelWaitEqueue", "sceKernelCreateSema",
    "scePthreadMutexLock", "scePthreadCreate", "sceKernelAllocateDirectMemory",
    "sceKernelMapDirectMemory", "sceSysmoduleLoadModule", "sceUserServiceGetInitialUser",
    "scePadInit", "sceVideoOutOpen", "sceAudioOutInit", "sceRtcGetCurrentTick",
    "__cxa_atexit", "exit", "abort", "sceKernelDlsym",
];

/// Проверка, реализована ли функция HLE.
pub fn supported(name: &str) -> bool {
    let n = normalize(name);
    SUPPORTED.contains(&n.as_str()) || {
        // имя может быть реализовано, даже если не попало в список
        let probe: &[&str] = &[
            "sceKernel", "scePthread", "sceSysmodule", "sceUserService", "sceSystemService",
            "scePad", "sceVideoOut", "sceAudioOut", "sceRtc", "sceSaveData", "sceNet",
            "sceHttp", "sceMsgDialog", "sceImeDialog", "sceNp", "sceAppContent",
        ];
        probe.iter().any(|p| n.starts_with(p))
    }
}
