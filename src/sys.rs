//! Высокоуровневая эмуляция системных вызовов PS4 (Orbis OS / FreeBSD 9).
//!
//! Номера системных вызовов соответствуют ядру FreeBSD 9.0, собранному
//! Sony для Orbis. Часть вызовов эмулируется полноценно (файлы, память,
//! потоки, время), часть — заглушками, достаточными для работы
//! тестовых программ и реальных бинарей.

use crate::mem::Memory;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod nr {
    pub const EXIT: u64 = 1;
    pub const READ: u64 = 3;
    pub const WRITE: u64 = 4;
    pub const OPEN: u64 = 5;
    pub const CLOSE: u64 = 6;
    pub const GETPID: u64 = 20;
    pub const RECVMSG: u64 = 27;
    pub const SENDMSG: u64 = 28;
    pub const RECVFROM: u64 = 29;
    pub const ACCEPT: u64 = 30;
    pub const BIND: u64 = 104;
    pub const SETSOCKOPT: u64 = 105;
    pub const LISTEN: u64 = 106;
    pub const GETTIMEOFDAY: u64 = 116;
    pub const GETRUSAGE: u64 = 117;
    pub const READV: u64 = 120;
    pub const WRITEV: u64 = 121;
    pub const SETITIMER: u64 = 83;
    pub const KILL: u64 = 37;
    pub const READLINK: u64 = 58;
    pub const MUNMAP: u64 = 73;
    pub const FTRUNCATE: u64 = 480;
    pub const LSEEK: u64 = 478;
    pub const MMAP: u64 = 477;
    pub const MPROTECT: u64 = 74;
    pub const MADVISE: u64 = 75;
    pub const MINCORE: u64 = 78;
    pub const SOCKET: u64 = 97;
    pub const CONNECT: u64 = 98;
    pub const GETSOCKOPT: u64 = 118;
    pub const GETSOCKNAME: u64 = 32;
    pub const GETPEERNAME: u64 = 31;
    pub const FCNTL: u64 = 92;
    pub const IOCTL: u64 = 54;
    pub const EXIT_GROUP: u64 = 492;
    pub const EXECVE: u64 = 59;
    pub const GETUID: u64 = 24;
    pub const GETEUID: u64 = 25;
    pub const GETGID: u64 = 47;
    pub const GETEGID: u64 = 43;
    pub const SETUID: u64 = 23;
    pub const GETPPID: u64 = 39;
    pub const GETSID: u64 = 310;
    pub const SETSID: u64 = 151;
    pub const FORK: u64 = 2;
    pub const WAIT4: u64 = 7;
    pub const OPENBSD_SYSCTL: u64 = 202;
    pub const SYSCONF: u64 = 601;
    pub const SCHED_YIELD: u64 = 331;
    pub const NANOSLEEP: u64 = 240;
    pub const CLOCK_GETTIME: u64 = 232;
    pub const GETDIRENTRIES: u64 = 272;
    pub const STAT: u64 = 188;
    pub const LSTAT: u64 = 190;
    pub const FSTAT: u64 = 189;
    pub const ACCESS: u64 = 33;
    pub const CHDIR: u64 = 12;
    pub const MKDIR: u64 = 136;
    pub const UNLINK: u64 = 10;
    pub const RENAME: u64 = 128;
    pub const DUP: u64 = 41;
    pub const DUP2: u64 = 90;
    pub const PIPE: u64 = 42;
    pub const SIGACTION: u64 = 416;
    pub const SIGPROCMASK: u64 = 340;
    pub const SIGALTSTACK: u64 = 342;
    pub const SIGRETURN: u64 = 417;
    pub const KQUEUE: u64 = 362;
    pub const KEVMACRO: u64 = 378;
    pub const SELECT: u64 = 93;
    pub const POLL: u64 = 209;
    pub const GETTID: u64 = 631;
    pub const THR_SELF: u64 = 432;
    pub const THR_EXIT: u64 = 431;
    pub const THR_CREATE: u64 = 430;
    pub const THR_JOIN: u64 = 433;
    pub const UMTX_OP: u64 = 573;
    pub const CPUSET_GETAFFINITY: u64 = 487;
    pub const CPUSET_SETAFFINITY: u64 = 488;
    pub const RT_PRIO_THREAD: u64 = 484;
    pub const BREAK: u64 = 17;
    pub const SBRK: u64 = 69;
    pub const ISSETUGID: u64 = 608;
    pub const ISSETPROCTITLE: u64 = 609;
    pub const JAIL_ATTACH: u64 = 537;
    pub const CAP_ENTER: u64 = 516;
    pub const CAPSGET: u64 = 515;
    pub const GETOSLEVEL: u64 = 602;
    pub const SETLOGIN: u64 = 50;
    pub const REVOKE: u64 = 56;
    pub const GETSOCKOPT_EXT: u64 = 118;
    pub const NETSTATS: u64 = 546;
    pub const SENDTO: u64 = 133;
    pub const RECVFROM2: u64 = 29;
    pub const SCHED_SETAFFINITY: u64 = 484;
    pub const SYNC: u64 = 36;
    pub const MKDIRAT: u64 = 496;
    pub const OPENAT: u64 = 499;
    pub const FSTATAT: u64 = 523;
    pub const READLINKAT: u64 = 505;
    pub const UNLINKAT: u64 = 503;
    pub const RENAMEAT: u64 = 512;
    pub const LOOKUP_SYMLINK: u64 = 0x7FFF_0001;
    // расширения (нужны для Linux-совместимости и libc)
    pub const ARCH_PRCTL: u64 = 158;
    pub const SYSARCH: u64 = 165;
    pub const SET_TID_ADDRESS: u64 = 218;
    pub const SET_ROBUST_LIST: u64 = 273;
    pub const RSEQ: u64 = 334;
    pub const PRLIMIT64: u64 = 302;
    pub const GETRANDOM: u64 = 318;
    pub const UNAME: u64 = 164;
    pub const GETCWD: u64 = 326;
    pub const FUTEX: u64 = 202;
    pub const GETRLIMIT: u64 = 194;
    pub const CLOCK_GETRES: u64 = 234;
    pub const MREMAP: u64 = 72;
    pub const CLONE3: u64 = 435;
    pub const ACCEPT4: u64 = 30;
    pub const GETDENTS64: u64 = 272;
}

pub const ENOENT: i32 = 2;
pub const EINTR: i32 = 4;
pub const EIO: i32 = 5;
pub const EBADF: i32 = 9;
pub const EAGAIN: i32 = 35;
pub const ENOMEM: i32 = 12;
pub const EFAULT: i32 = 14;
pub const EINVAL: i32 = 22;
pub const ENOSYS: i32 = 78;
pub const ENOTTY: i32 = 25;
pub const EPIPE: i32 = 32;
pub const ENOEXEC: i32 = 8;

pub const AF_UNIX: u64 = 1;
pub const AF_INET: u64 = 2;

/// Открытый файл (виртуальный дескриптор).
#[derive(Debug)]
pub enum FileObj {
    Stdout,
    Stderr,
    Stdin,
    /// Файл из VFS хоста (виртуальный путь -> байты).
    Mem { path: String, data: Vec<u8>, pos: u64, writable: bool },
    /// Каталог: список записей.
    Dir { path: String, entries: Vec<String>, done: bool },
    /// Сокет (заглушка).
    Socket { domain: u64, ty: u64, connected: bool },
    /// Псевдо-устройство вроде /dev/urandom.
    Rng,
}

impl FileObj {
    pub fn is_dir(&self) -> bool {
        matches!(self, FileObj::Dir { .. })
    }
}

/// Какой таблице номеров следуют вызовы гостя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abi {
    /// Orbis OS (FreeBSD 9 + расширения Sony) — родной для PS4.
    Orbis,
    /// Linux x86-64 — режим совместимости, полезен для запуска
    /// обычных статических бинарников и тестов ядра.
    Linux,
}

#[derive(Debug, Clone)]
pub struct ThreadInfo {
    pub id: i32,
    pub name: String,
    pub detached: bool,
    pub exited: bool,
}

/// Полное состояние эмуляции, доступное системным вызовам.
pub struct Sys {
    pub open_files: BTreeMap<i32, FileObj>,
    pub next_fd: i32,
    /// Виртуальная файловая система: путь -> содержимое.
    pub vfs: BTreeMap<String, Vec<u8>>,
    /// Каталоги для getdirentries.
    pub vdirs: BTreeMap<String, Vec<String>>,
    pub errno: i32,
    pub heap_brk: u64,
    pub heap_start: u64,
    pub heap_end: u64,
    pub mmap_hint: u64,
    pub mmap_end: u64,
    pub pid: i32,
    pub ppid: i32,
    pub tid: i32,
    pub uid: u32,
    pub gid: u32,
    pub threads: Vec<ThreadInfo>,
    pub mutexes: BTreeMap<u64, u64>,
    /// Куча аллокаций эмулятора (для free).
    pub allocations: BTreeMap<u64, u64>,
    pub stdout: String,
    pub stderr_buf: String,
    pub start_time: SystemTime,
    pub app_param: Option<Vec<u8>>,
    /// Лог вызовов (если включён трассировщик).
    pub log: Vec<String>,
    pub log_enabled: bool,
    pub hlt_count: u64,
    pub rng_state: u64,
    pub env: Vec<(String, String)>,
    /// Каталог, куда пишутся созданные гостем файлы.
    pub work_dir: String,
    pub exit_code: i32,
    pub exited: bool,
    pub abi: Abi,
    /// Текущие базы сегментов FS/GS (обновляются при syscall).
    pub fs_base: u64,
    pub gs_base: u64,
    pub cwd: String,
    /// Адреса зарегистрированных кадровых буферов `sceVideoOutRegisterBuffers`
    /// (индекс буфера -> адрес в памяти гостя).
    pub framebuffers: BTreeMap<u32, u64>,
    /// Ширина/высота кадра, как их объявил гость.
    pub fb_size: (u32, u32),
    /// Адрес и размер последнего представленного (`SubmitFlip`) кадра.
    pub last_flip: Option<(u64, u32, u32)>,
    /// Состояние виртуального геймпада, которое читает HLE.
    pub pad: crate::gpu::pad::PadState,
}

impl Sys {
    pub fn new() -> Self {
        let mut open_files = BTreeMap::new();
        open_files.insert(0, FileObj::Stdin);
        open_files.insert(1, FileObj::Stdout);
        open_files.insert(2, FileObj::Stderr);
        Sys {
            open_files,
            next_fd: 3,
            vfs: BTreeMap::new(),
            vdirs: BTreeMap::new(),
            errno: 0,
            heap_brk: 0,
            heap_start: 0,
            heap_end: 0,
            mmap_hint: 0,
            mmap_end: 0,
            pid: 0x1001,
            ppid: 1,
            tid: 1,
            uid: 1000,
            gid: 1000,
            threads: vec![ThreadInfo { id: 1, name: "main".into(), detached: false, exited: false }],
            mutexes: BTreeMap::new(),
            allocations: BTreeMap::new(),
            stdout: String::new(),
            stderr_buf: String::new(),
            start_time: SystemTime::now(),
            app_param: None,
            log: Vec::new(),
            log_enabled: false,
            hlt_count: 0,
            rng_state: 0x853c_49e6_748f_ea9b,
            env: Vec::new(),
            work_dir: ".".into(),
            exit_code: 0,
            exited: false,
            abi: Abi::Orbis,
            fs_base: 0,
            gs_base: 0,
            cwd: "/app0".into(),
            framebuffers: BTreeMap::new(),
            fb_size: (1920, 1080),
            last_flip: None,
            pad: crate::gpu::pad::PadState { connected: true, ..Default::default() },
        }
    }

    pub fn log(&mut self, s: String) {
        if self.log_enabled {
            if self.log.len() > 100_000 {
                self.log.remove(0);
            }
            self.log.push(s);
        }
    }

    fn alloc_fd(&mut self, f: FileObj) -> i32 {
        let fd = self.next_fd;
        self.next_fd += 1;
        self.open_files.insert(fd, f);
        fd
    }

    fn next_rand(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.rng_state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng_state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

impl Default for Sys {
    fn default() -> Self {
        Self::new()
    }
}

/// Результат вызова: Ok(значение) либо ошибка (errno).
pub type SysResult = Result<u64, i32>;

impl From<crate::mem::MemError> for i32 {
    fn from(_e: crate::mem::MemError) -> i32 {
        EFAULT
    }
}

pub struct SyscallCtx<'a> {
    pub mem: &'a mut Memory,
    pub sys: &'a mut Sys,
    /// Адрес инструкции syscall (для отладки).
    pub rip: u64,
    /// Имя вызываемого вызова (для отладочного лога).
    pub name: &'static str,
    /// Состояние потока, установленное CRT (через syscall 601/608).
    pub thr_self: u64,
    pub last_syscall: u64,
    pub last_args: [u64; 6],
}

pub fn syscall_name(nr: u64) -> &'static str {
    match nr {
        nr::EXIT | nr::EXIT_GROUP => "exit",
        nr::READ => "read",
        nr::WRITE => "write",
        nr::OPEN => "open",
        nr::CLOSE => "close",
        nr::MMAP => "mmap",
        nr::MUNMAP => "munmap",
        nr::MPROTECT => "mprotect",
        nr::LSEEK => "lseek",
        nr::FTRUNCATE => "ftruncate",
        nr::FSTAT => "fstat",
        nr::STAT => "stat",
        nr::LSTAT => "lstat",
        nr::GETTIMEOFDAY => "gettimeofday",
        nr::NANOSLEEP => "nanosleep",
        nr::CLOCK_GETTIME => "clock_gettime",
        nr::SOCKET => "socket",
        nr::CONNECT => "connect",
        nr::BIND => "bind",
        nr::LISTEN => "listen",
        nr::ACCEPT => "accept", // == ACCEPT4
        nr::SENDMSG => "sendmsg",
        nr::RECVMSG => "recvmsg",
        nr::SENDTO => "sendto",
        nr::RECVFROM => "recvfrom",
        nr::SETSOCKOPT => "setsockopt",
        nr::GETSOCKOPT => "getsockopt",
        nr::FCNTL => "fcntl",
        nr::IOCTL => "ioctl",
        nr::GETPID => "getpid",
        nr::GETTID => "gettid",
        nr::THR_SELF => "thr_self",
        nr::THR_EXIT => "thr_exit",
        nr::THR_CREATE => "thr_new",
        nr::UMTX_OP => "_umtx_op",
        nr::SCHED_YIELD => "sched_yield",
        nr::GETRUSAGE => "getrusage",
        nr::READV => "readv",
        nr::WRITEV => "writev",
        nr::GETDIRENTRIES => "getdirentries", // == GETDENTS64
        nr::ACCESS => "access",
        nr::CHDIR => "chdir",
        nr::MKDIR => "mkdir",
        nr::UNLINK => "unlink",
        nr::RENAME => "rename",
        nr::DUP => "dup",
        nr::DUP2 => "dup2",
        nr::PIPE => "pipe",
        nr::SIGACTION => "sigaction",
        nr::SIGPROCMASK => "sigprocmask",
        nr::SIGALTSTACK => "sigaltstack",
        nr::SYSCONF => "sysconf",
        nr::GETOSLEVEL => "getoslevel",
        nr::ISSETUGID => "issetugid",
        nr::FORK => "fork",
        nr::WAIT4 => "wait4",
        nr::KILL => "kill",
        nr::OPENAT => "openat",
        nr::FSTATAT => "fstatat",
        nr::ARCH_PRCTL => "arch_prctl",
        nr::SYSARCH => "sysarch",
        nr::SET_TID_ADDRESS => "set_tid_address",
        nr::SET_ROBUST_LIST => "set_robust_list",
        nr::RSEQ => "rseq",
        nr::PRLIMIT64 => "prlimit64",
        nr::GETRANDOM => "getrandom",
        nr::UNAME => "uname",
        nr::GETCWD => "getcwd",
        nr::FUTEX => "futex",
        nr::GETRLIMIT => "getrlimit",
        nr::CLOCK_GETRES => "clock_getres",
        nr::MREMAP => "mremap",
        nr::CLONE3 => "clone3",
        nr::BREAK => "break",
        nr::SBRK => "sbrk",
        nr::MADVISE => "madvise",
        nr::SIGRETURN => "sigreturn",
        nr::GETSOCKNAME => "getsockname",
        nr::GETPEERNAME => "getpeername",
        nr::SETSID => "setsid",
        nr::GETSID => "getsid",
        nr::SETUID => "setuid",
        nr::SYNC => "sync",
        nr::KQUEUE => "kqueue",
        nr::SELECT => "select",
        nr::POLL => "poll",
        nr::CPUSET_SETAFFINITY => "cpuset_setaffinity",
        nr::CPUSET_GETAFFINITY => "cpuset_getaffinity",
        _ => "неизвестный",
    }
}

/// Перевод номера Linux x86-64 в внутренний номер Orbis.
pub fn linux_to_orbis(nr: u64) -> Option<u64> {
    Some(match nr {
        0 => nr::READ,
        1 => nr::WRITE,
        2 => nr::OPEN,
        3 => nr::CLOSE,
        9 => nr::MMAP,
        10 => nr::MPROTECT,
        11 => nr::MUNMAP,
        12 => nr::BREAK,
        13 => nr::SIGACTION,
        14 => nr::SIGPROCMASK,
        16 => nr::IOCTL,
        19 => nr::READV,
        20 => nr::WRITEV,
        21 => nr::ACCESS,
        22 => nr::PIPE,
        23 => nr::SELECT,
        24 => nr::SCHED_YIELD,
        28 => nr::MADVISE,
        32 => nr::DUP,
        33 => nr::DUP2,
        35 => nr::NANOSLEEP,
        39 => nr::GETPID,
        41 => nr::SOCKET,
        42 => nr::CONNECT,
        43 => nr::ACCEPT,
        49 => nr::BIND,
        50 => nr::LISTEN,
        51 => nr::GETSOCKNAME,
        52 => nr::GETPEERNAME,
        54 => nr::SETSOCKOPT,
        55 => nr::GETSOCKOPT,
        57 => nr::FORK,
        59 => nr::EXECVE,
        60 => nr::EXIT,
        61 => nr::WAIT4,
        62 => nr::KILL,
        72 => nr::FCNTL,
        78 => nr::GETDIRENTRIES,
        80 => nr::CHDIR,
        87 => nr::UNLINK,
        89 => nr::READLINK,
        96 => nr::GETTIMEOFDAY,
        97 => nr::GETRLIMIT,
        131 => nr::SIGALTSTACK,
        158 => nr::ARCH_PRCTL,
        186 => nr::GETTID,
        202 => nr::FUTEX,
        217 => nr::GETDENTS64,
        218 => nr::SET_TID_ADDRESS,
        228 => nr::CLOCK_GETTIME,
        229 => nr::CLOCK_GETRES,
        231 => nr::EXIT_GROUP,
        257 => nr::OPENAT,
        262 => nr::FSTATAT,
        263 => nr::UNLINKAT,
        273 => nr::SET_ROBUST_LIST,
        302 => nr::PRLIMIT64,
        318 => nr::GETRANDOM,
        334 => nr::RSEQ,
        435 => nr::CLONE3,
        _ => return None,
    })
}

pub fn dispatch(ctx: &mut SyscallCtx, nr: u64, a: [u64; 6]) -> SysResult {
    let original = nr;
    let nr = if ctx.sys.abi == Abi::Linux {
        match linux_to_orbis(nr) {
            Some(n) => n,
            None => {
                if ctx.sys.log_enabled {
                    ctx.sys.log(format!("Linux syscall {} пока не поддержан", nr));
                }
                return Err(ENOSYS);
            }
        }
    } else {
        nr
    };
    ctx.last_syscall = original;
    ctx.last_args = a;
    ctx.name = syscall_name(nr);
    if ctx.sys.log_enabled {
        let msg = format!(
            "{:#x}: {}({}) {:x?}",
            ctx.rip,
            ctx.name,
            if original == nr { String::new() } else { format!("linux {}", original) },
            a
        );
        ctx.sys.log(msg.clone());
        if std::env::var("PS4EMU_VERBOSE").is_ok() {
            eprintln!("[sys] {}", msg);
        }
    }
    match nr {
        // ---- процессы
        nr::EXIT | nr::EXIT_GROUP => {
            ctx.sys.exit_code = a[0] as i32;
            ctx.sys.exited = true;
            Ok(0)
        }
        nr::GETPID => Ok(ctx.sys.pid as u64),
        nr::GETPPID => Ok(ctx.sys.ppid as u64),
        nr::GETTID | nr::THR_SELF => Ok(ctx.sys.tid as u64),
        nr::GETUID => Ok(ctx.sys.uid as u64),
        nr::GETEUID => Ok(ctx.sys.uid as u64),
        nr::GETGID => Ok(ctx.sys.gid as u64),
        nr::GETEGID => Ok(ctx.sys.gid as u64),
        nr::FORK => Ok(0), // дочерний процесс «уже здесь»
        nr::WAIT4 => Err(ENOENT),
        nr::KILL => Ok(0),
        nr::THR_EXIT => {
            ctx.sys.exited = true;
            Ok(0)
        }
        nr::THR_CREATE => Ok(0),
        nr::THR_JOIN => Ok(0),
        nr::SCHED_YIELD => Ok(0),
        nr::GETSID | nr::SETSID => Ok(ctx.sys.pid as u64),

        // ---- время
        nr::GETTIMEOFDAY => {
            let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
            if a[0] != 0 {
                ctx.mem.write_u64(a[0], t.as_secs())?;
                ctx.mem.write_u64(a[0] + 8, t.subsec_micros() as u64)?;
            }
            if a[1] != 0 {
                ctx.mem.write_u32(a[1], 0)?;
                ctx.mem.write_u32(a[1] + 4, 0)?;
            }
            Ok(0)
        }
        nr::CLOCK_GETTIME => {
            let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
            if a[1] != 0 {
                ctx.mem.write_u64(a[1], t.as_secs())?;
                ctx.mem.write_u64(a[1] + 8, t.subsec_nanos() as u64)?;
            }
            Ok(0)
        }
        nr::NANOSLEEP => {
            if a[0] != 0 {
                let sec = ctx.mem.read_u64(a[0])?;
                let nsec = ctx.mem.read_u64(a[0] + 8)?;
                let ms = sec.saturating_mul(1000) + nsec / 1_000_000;
                if ms > 0 && ms < 10_000 {
                    std::thread::sleep(std::time::Duration::from_millis(ms.min(100)));
                }
            }
            if a[1] != 0 {
                ctx.mem.write_u64(a[1], 0)?;
                ctx.mem.write_u64(a[1] + 8, 0)?;
            }
            Ok(0)
        }
        nr::GETRUSAGE => {
            if a[1] != 0 {
                // struct rusage — 144 байта; время работы процесса
                let el = ctx.sys.start_time.elapsed().unwrap_or_default();
                let sec = el.as_secs() as i64;
                let usec = el.subsec_micros() as i64;
                for i in 0..18u64 {
                    ctx.mem.write_u64(a[1] + i * 8, 0)?;
                }
                ctx.mem.write_i64(a[1], sec)?;
                ctx.mem.write_i64(a[1] + 8, usec)?;
            }
            Ok(0)
        }
        nr::SETITIMER => Ok(0),

        // ---- память
        nr::MMAP => {
            let len = a[1];
            let prot = a[2] as u32;
            let mut flags = a[3];
            let fd = a[4] as i64;
            let off = a[5];
            let map_fixed = flags & 0x10 != 0;
            // MAP_ANON: FreeBSD 0x1000, Linux 0x20
            let anon = flags & 0x1000 != 0 || flags & 0x20 != 0 || fd < 0;
            flags &= !0x8000; // MAP_NOCORE
            let _ = flags;
            let p = prot_to_ps4(prot);
            let want = if a[0] != 0 { a[0] } else { ctx.sys.mmap_hint };
            let base = if map_fixed {
                want
            } else if want >= ctx.sys.mmap_hint && want + len <= ctx.sys.mmap_end {
                want
            } else {
                ctx.sys.mmap_hint
            };
            if base + len > ctx.sys.mmap_end.max(base + len) {
                return Err(ENOMEM);
            }
            ctx.mem.unmap(base, len).ok();
            ctx.mem.map(base, len, p, "mmap")?;
            if anon {
                // Отображение уже обнулено.
            } else if fd >= 0 {
                let f = fd as i32;
                let content: Vec<u8> = match ctx.sys.open_files.get(&f) {
                    Some(FileObj::Mem { data, pos, .. }) => {
                        let start = (*pos + off) as usize;
                        let end = (start + len as usize).min(data.len());
                        if start < data.len() {
                            data[start..end].to_vec()
                        } else {
                            Vec::new()
                        }
                    }
                    _ => Vec::new(),
                };
                if !content.is_empty() {
                    ctx.mem.write_bytes(base, &content)?;
                }
            }
            if a[0] == 0 {
                ctx.sys.mmap_hint = (base + len + 0xFFFF) & !0xFFFF;
            }
            Ok(base)
        }
        nr::MUNMAP => {
            ctx.mem.unmap(a[0], a[1]).ok();
            Ok(0)
        }
        nr::MPROTECT | nr::MADVISE | nr::MINCORE => {
            if nr == nr::MPROTECT {
                let p = prot_to_ps4(a[2] as u32);
                ctx.mem.protect(a[0], a[1], p).ok();
            }
            Ok(0)
        }
        nr::BREAK => {
            // Регион кучи отображён целиком, поэтому break() только
            // перемещает границу — так данные не теряются.
            if a[0] != 0 {
                if a[0] > ctx.sys.heap_end || a[0] < ctx.sys.heap_start {
                    return Err(ENOMEM);
                }
                ctx.sys.heap_brk = a[0];
                Ok(0)
            } else {
                Ok(ctx.sys.heap_brk)
            }
        }
        nr::SBRK => {
            let cur = ctx.sys.heap_brk;
            let delta = a[0] as i64;
            let new = cur as i64 + delta;
            if delta > 0 && (new as u64) > ctx.sys.heap_end {
                return Err(ENOMEM);
            }
            if new < ctx.sys.heap_start as i64 {
                return Err(ENOMEM);
            }
            ctx.sys.heap_brk = new as u64;
            Ok(cur)
        }

        // ---- файлы
        nr::OPEN | nr::OPENAT => {
            let (path_addr, flags) = if nr == nr::OPEN { (a[0], a[1]) } else { (a[1], a[2]) };
            let path = ctx.mem.read_cstr(path_addr, 1024)?;
            let create = flags & 0x0200 != 0 || flags & 0x0002 != 0;
            let wr = flags & 0x0003 != 0 || flags & 0x0008 != 0;
            let key = ctx.sys.vfs.get(&path).map(|_| path.clone());
            match key {
                Some(p) => {
                    let data = ctx.sys.vfs.get(&p).cloned().unwrap_or_default();
                    Ok(ctx.sys.alloc_fd(FileObj::Mem { path: p, data, pos: 0, writable: true }) as u64)
                }
                None => {
                    if ctx.sys.vdirs.contains_key(&path) {
                        let entries = ctx.sys.vdirs.get(&path).cloned().unwrap_or_default();
                        return Ok(ctx.sys.alloc_fd(FileObj::Dir { path, entries, done: false }) as u64);
                    }
                    if path == "/dev/urandom" || path == "/dev/random" {
                        return Ok(ctx.sys.alloc_fd(FileObj::Rng) as u64);
                    }
                    if path == "/dev/tty" || path == "/dev/null" {
                        return Ok(ctx.sys.alloc_fd(if wr { FileObj::Stderr } else { FileObj::Stdin }) as u64);
                    }
                    if create || wr {
                        ctx.sys.vfs.insert(path.clone(), Vec::new());
                        Ok(ctx.sys.alloc_fd(FileObj::Mem {
                            path,
                            data: Vec::new(),
                            pos: 0,
                            writable: true,
                        }) as u64)
                    } else {
                        Err(ENOENT)
                    }
                }
            }
        }
        nr::CLOSE => {
            let fd = a[0] as i32;
            if let Some(FileObj::Mem { path, data, .. }) = ctx.sys.open_files.get(&fd) {
                let path = path.clone();
                let data = data.clone();
                let _ = data;
                ctx.sys.vfs.insert(path, data);
            }
            if ctx.sys.open_files.remove(&fd).is_some() {
                Ok(0)
            } else {
                Err(EBADF)
            }
        }
        nr::READ => {
            let fd = a[0] as i32;
            let buf = a[1];
            let len = a[2] as usize;
            // Свежие данные из VFS (файл мог быть перезаписан)
            if let Some(FileObj::Mem { path, .. }) = ctx.sys.open_files.get(&fd) {
                if let Some(v) = ctx.sys.vfs.get(path) {
                    let v = v.clone();
                    if let Some(FileObj::Mem { data, .. }) = ctx.sys.open_files.get_mut(&fd) {
                        *data = v;
                    }
                }
            }
            match ctx.sys.open_files.get_mut(&fd) {
                Some(FileObj::Stdin) => Ok(0),
                Some(FileObj::Mem { data, pos, .. }) => {
                    let start = (*pos as usize).min(data.len());
                    let end = (start + len).min(data.len());
                    let chunk = data[start..end].to_vec();
                    *pos += chunk.len() as u64;
                    if !chunk.is_empty() {
                        ctx.mem.write_bytes(buf, &chunk)?;
                    }
                    Ok(chunk.len() as u64)
                }
                Some(FileObj::Rng) => {
                    let mut v = vec![0u8; len.min(1 << 20)];
                    for b in v.iter_mut() {
                        *b = ctx.sys.next_rand() as u8;
                    }
                    ctx.mem.write_bytes(buf, &v)?;
                    Ok(v.len() as u64)
                }
                Some(FileObj::Dir { .. }) => Err(EIO),
                Some(_) => Err(EINVAL),
                None => Err(EBADF),
            }
        }
        nr::WRITE => {
            let fd = a[0] as i32;
            let buf = a[1];
            let len = a[2] as usize;
            if len > 64 * 1024 * 1024 {
                return Err(EINVAL);
            }
            let mut data = vec![0u8; len];
            let got = ctx.mem.read_partial(buf, &mut data);
            data.truncate(got);
            write_fd(ctx.sys, fd, &data)
        }
        nr::READV | nr::WRITEV => {
            let fd = a[0] as i32;
            let iov = a[1];
            let cnt = a[2].min(1024);
            let mut total = 0u64;
            for i in 0..cnt {
                let base = ctx.mem.read_u64(iov + i * 16)?;
                let len = ctx.mem.read_u64(iov + i * 16 + 8)? as usize;
                if nr == nr::WRITEV {
                    let mut data = vec![0u8; len.min(1 << 24)];
                    let got = ctx.mem.read_partial(base, &mut data);
                    data.truncate(got);
                    let n = write_fd(ctx.sys, fd, &data)?;
                    total += n;
                } else {
                    let mut data = vec![0u8; len.min(1 << 24)];
                    let n = match ctx.sys.open_files.get_mut(&fd) {
                        Some(FileObj::Mem { data: d, pos, .. }) => {
                            let s = (*pos as usize).min(d.len());
                            let e = (s + data.len()).min(d.len());
                            let n = e - s;
                            data[..n].copy_from_slice(&d[s..e]);
                            *pos += n as u64;
                            n
                        }
                        _ => 0,
                    };
                    ctx.mem.write_bytes(base, &data[..n])?;
                    total += n as u64;
                }
            }
            Ok(total)
        }
        nr::LSEEK => {
            let fd = a[0] as i32;
            let off = a[1] as i64;
            let whence = a[2];
            match ctx.sys.open_files.get_mut(&fd) {
                Some(FileObj::Mem { data, pos, .. }) => {
                    let new = match whence {
                        0 => off,
                        1 => *pos as i64 + off,
                        2 => data.len() as i64 + off,
                        _ => return Err(EINVAL),
                    };
                    *pos = new.max(0) as u64;
                    Ok(*pos)
                }
                Some(FileObj::Dir { .. }) => Ok(0),
                Some(_) => Ok(0),
                None => Err(EBADF),
            }
        }
        nr::FTRUNCATE => {
            let fd = a[0] as i32;
            let len = a[1] as usize;
            match ctx.sys.open_files.get_mut(&fd) {
                Some(FileObj::Mem { data, .. }) => {
                    data.resize(len, 0);
                    Ok(0)
                }
                Some(_) => Ok(0),
                None => Err(EBADF),
            }
        }
        nr::FSTAT | nr::STAT | nr::LSTAT | nr::FSTATAT => {
            let (buf, mode) = match nr {
                nr::FSTAT => {
                    let fd = a[0] as i32;
                    let mode = match ctx.sys.open_files.get(&fd) {
                        Some(FileObj::Dir { .. }) => 0o040755u32,
                        Some(FileObj::Stdout) | Some(FileObj::Stderr) | Some(FileObj::Stdin) => {
                            0o020666
                        }
                        Some(FileObj::Socket { .. }) => 0o140777,
                        Some(_) => 0o100644,
                        None => return Err(EBADF),
                    };
                    (a[1], mode)
                }
                nr::FSTATAT => (a[2], 0o100644),
                _ => (a[1], 0o100644),
            };
            if buf != 0 {
                // Записываем ровно 144 байта (struct stat x86-64): иначе можно
                // повредить соседние данные гостя.
                let mut st = vec![0u8; 144];
                st[8..16].copy_from_slice(&2u64.to_le_bytes()); // st_ino
                st[16..24].copy_from_slice(&1u64.to_le_bytes()); // st_nlink
                st[24..28].copy_from_slice(&mode.to_le_bytes()); // st_mode
                st[28..32].copy_from_slice(&ctx.sys.uid.to_le_bytes()); // st_uid
                st[32..36].copy_from_slice(&ctx.sys.gid.to_le_bytes()); // st_gid
                st[48..56].copy_from_slice(&4096u64.to_le_bytes()); // st_size
                st[56..64].copy_from_slice(&4096u64.to_le_bytes()); // st_blksize
                st[64..72].copy_from_slice(&8u64.to_le_bytes()); // st_blocks
                ctx.mem.write_bytes(buf, &st)?;
            }
            Ok(0)
        }
        nr::GETDIRENTRIES => {
            let fd = a[0] as i32;
            let buf = a[1];
            let len = a[2] as usize;
            let basep = a[3];
            let entries = match ctx.sys.open_files.get(&fd) {
                Some(FileObj::Dir { entries, .. }) => entries.clone(),
                _ => return Err(EBADF),
            };
            let done = match ctx.sys.open_files.get(&fd) {
                Some(FileObj::Dir { done, .. }) => *done,
                _ => true,
            };
            if done {
                return Ok(0);
            }
            // Формат freebsd11_dirent: d_fileno(4) d_reclen(2) d_type(1)
            // d_namlen(1) d_name(namlen+1), выровнено по 8.
            let mut out = Vec::new();
            for (i, name) in entries.iter().enumerate() {
                let namelen = name.len().min(255);
                let reclen = ((8 + namelen + 1) + 7) & !7;
                if out.len() + reclen > len {
                    break;
                }
                let mut rec = vec![0u8; reclen];
                rec[0..4].copy_from_slice(&((i + 1) as u32).to_le_bytes());
                rec[4..6].copy_from_slice(&(reclen as u16).to_le_bytes());
                rec[6] = 8; // DT_REG
                rec[7] = namelen as u8;
                rec[8..8 + namelen].copy_from_slice(&name.as_bytes()[..namelen]);
                out.extend_from_slice(&rec);
            }
            ctx.mem.write_bytes(buf, &out)?;
            if let Some(FileObj::Dir { done, .. }) = ctx.sys.open_files.get_mut(&fd) {
                *done = true;
            }
            if basep != 0 {
                ctx.mem.write_u64(basep, out.len() as u64)?;
            }
            Ok(out.len() as u64)
        }
        nr::ACCESS => Ok(0),
        nr::CHDIR => Ok(0),
        nr::MKDIR => {
            let p = ctx.mem.read_cstr(a[0], 1024)?;
            ctx.sys.vdirs.entry(p).or_default();
            Ok(0)
        }
        nr::UNLINK | nr::UNLINKAT => {
            let addr = if nr == nr::UNLINK { a[0] } else { a[1] };
            let p = ctx.mem.read_cstr(addr, 1024)?;
            ctx.sys.vfs.remove(&p);
            Ok(0)
        }
        nr::RENAME | nr::RENAMEAT => {
            let (fa, ta) = if nr == nr::RENAME { (a[0], a[1]) } else { (a[1], a[3]) };
            let from = ctx.mem.read_cstr(fa, 1024)?;
            let to = ctx.mem.read_cstr(ta, 1024)?;
            if let Some(d) = ctx.sys.vfs.remove(&from) {
                ctx.sys.vfs.insert(to, d);
            }
            Ok(0)
        }
        nr::READLINK | nr::READLINKAT => {
            let (pa, ba, len) = if nr == nr::READLINK { (a[0], a[1], a[2]) } else { (a[1], a[2], a[3]) };
            let p = ctx.mem.read_cstr(pa, 1024)?;
            let s = if p == "/proc/self/exe" {
                "/app0/eboot.bin".to_string()
            } else {
                p
            };
            let n = (s.len() as u64).min(len.saturating_sub(1));
            ctx.mem.write_bytes(ba, &s.as_bytes()[..n as usize])?;
            ctx.mem.write_u8(ba + n, 0)?;
            Ok(n)
        }
        nr::DUP => {
            let fd = a[0] as i32;
            match ctx.sys.open_files.get(&fd).map(|f| clone_file(f)) {
                Some(Some(f)) => Ok(ctx.sys.alloc_fd(f) as u64),
                Some(None) => Ok(ctx.sys.alloc_fd(FileObj::Stdin) as u64),
                None => Err(EBADF),
            }
        }
        nr::DUP2 => {
            let from = a[0] as i32;
            let to = a[1] as i32;
            let f = ctx.sys.open_files.get(&from).map(clone_file);
            match f {
                Some(Some(f)) => {
                    ctx.sys.open_files.insert(to, f);
                    Ok(to as u64)
                }
                _ => Err(EBADF),
            }
        }
        nr::PIPE => {
            let fd = a[0];
            for i in 0..2 {
                ctx.mem.write_u32(fd + i * 4, (ctx.sys.next_fd + i as i32) as u32)?;
            }
            ctx.sys.next_fd += 2;
            Ok(0)
        }
        nr::FCNTL => Ok(0),
        nr::IOCTL => Err(ENOTTY),

        // ---- сокеты
        nr::SOCKET => {
            let fd = ctx.sys.alloc_fd(FileObj::Socket { domain: a[0], ty: a[1], connected: false });
            Ok(fd as u64)
        }
        nr::CONNECT => {
            if let Some(FileObj::Socket { connected, .. }) = ctx.sys.open_files.get_mut(&(a[0] as i32)) {
                *connected = true;
            }
            Ok(0)
        }
        nr::BIND | nr::LISTEN | nr::SETSOCKOPT => Ok(0),
        nr::ACCEPT => Ok(ctx.sys.alloc_fd(FileObj::Socket {
            domain: AF_INET,
            ty: 1,
            connected: true,
        }) as u64),
        nr::GETSOCKNAME | nr::GETPEERNAME => Ok(0),
        nr::GETSOCKOPT => {
            if a[4] != 0 {
                ctx.mem.write_u32(a[4], 0)?;
            }
            Ok(0)
        }
        nr::SENDMSG | nr::RECVMSG => {
            let fd = a[0] as i32;
            let msg = a[1];
            if nr == nr::SENDMSG {
                let iov = ctx.mem.read_u64(msg + 8)?;
                let iovlen = ctx.mem.read_u64(msg + 16)?;
                let mut total = 0;
                for i in 0..iovlen.min(16) {
                    let base = ctx.mem.read_u64(iov + i * 16)?;
                    let len = ctx.mem.read_u64(iov + i * 16 + 8)? as usize;
                    let mut data = vec![0u8; len.min(1 << 20)];
                    let got = ctx.mem.read_partial(base, &mut data);
                    data.truncate(got);
                    total += write_fd(ctx.sys, fd, &data)?;
                }
                Ok(total)
            } else {
                if a[2] != 0 {
                    ctx.mem.write_u32(a[2] + 4, 0)?;
                }
                Ok(0)
            }
        }
        nr::SENDTO => {
            let fd = a[0] as i32;
            let mut data = vec![0u8; (a[2] as usize).min(1 << 20)];
            let got = ctx.mem.read_partial(a[1], &mut data);
            data.truncate(got);
            write_fd(ctx.sys, fd, &data)
        }
        nr::RECVFROM => Ok(0),

        // ---- расширения (TLS, рантайм glibc и т.п.)
        nr::ARCH_PRCTL | nr::SYSARCH => {
            // Linux: arch_prctl(code, addr); FreeBSD: sysarch(op, parms)
            if nr == nr::ARCH_PRCTL {
                match a[0] {
                    0x1001 => {
                        ctx.sys.gs_base = a[1];
                        Ok(0)
                    }
                    0x1002 => {
                        ctx.sys.fs_base = a[1];
                        Ok(0)
                    }
                    0x1003 => {
                        ctx.mem.write_u64(a[1], ctx.sys.fs_base)?;
                        Ok(0)
                    }
                    0x1004 => {
                        ctx.mem.write_u64(a[1], ctx.sys.gs_base)?;
                        Ok(0)
                    }
                    _ => Err(EINVAL),
                }
            } else {
                // AMD64_GET_FSBASE=130, SET_FSBASE=131, GET_GSBASE=128, SET_GSBASE=129
                match a[0] {
                    128 => {
                        ctx.mem.write_u64(a[1], ctx.sys.gs_base)?;
                        Ok(0)
                    }
                    129 => {
                        ctx.sys.gs_base = ctx.mem.read_u64(a[1])?;
                        Ok(0)
                    }
                    130 => {
                        ctx.mem.write_u64(a[1], ctx.sys.fs_base)?;
                        Ok(0)
                    }
                    131 => {
                        ctx.sys.fs_base = ctx.mem.read_u64(a[1])?;
                        Ok(0)
                    }
                    _ => Err(EINVAL),
                }
            }
        }
        nr::SET_TID_ADDRESS => Ok(ctx.sys.tid as u64),
        nr::SET_ROBUST_LIST | nr::RSEQ => Ok(0),
        nr::PRLIMIT64 => {
            if a[2] != 0 {
                // struct rlimit64 { cur, max }
                ctx.mem.write_u64(a[2], 1 << 20)?;
                ctx.mem.write_u64(a[2] + 8, 1 << 20)?;
            }
            Ok(0)
        }
        nr::GETRLIMIT => {
            if a[1] != 0 {
                ctx.mem.write_u64(a[1], 1 << 20)?;
                ctx.mem.write_u64(a[1] + 8, 1 << 20)?;
            }
            Ok(0)
        }
        nr::GETRANDOM => {
            let len = a[1] as usize;
            let mut v = vec![0u8; len.min(1 << 20)];
            for b in v.iter_mut() {
                *b = ctx.sys.next_rand() as u8;
            }
            ctx.mem.write_bytes(a[0], &v)?;
            Ok(v.len() as u64)
        }
        nr::UNAME => {
            // struct utsname: 6 полей по 65 байт
            let fields = ["Orbis", "ps4emu", "1.0", "Orbis OS 5.05", "x86_64", "ps4emu"];
            let mut buf = vec![0u8; 65 * 6];
            for (i, f) in fields.iter().enumerate() {
                let b = f.as_bytes();
                buf[i * 65..i * 65 + b.len()].copy_from_slice(b);
            }
            ctx.mem.write_bytes(a[0], &buf)?;
            Ok(0)
        }
        nr::GETCWD => {
            let cwd = ctx.sys.cwd.clone();
            let n = (cwd.len() as u64 + 1).min(a[1]);
            let bytes = &cwd.as_bytes()[..(n as usize).saturating_sub(1).min(cwd.len())];
            ctx.mem.write_bytes(a[0], bytes)?;
            ctx.mem.write_u8(a[0] + bytes.len() as u64, 0)?;
            Ok(a[0])
        }
        nr::FUTEX => {
            // FUTEX_WAIT (0) — считаем, что значение уже изменилось;
            // FUTEX_WAKE и остальные — успех.
            match a[1] & 0x7F {
                0 => Err(EAGAIN),
                _ => Ok(0),
            }
        }
        nr::CLOCK_GETRES => {
            if a[1] != 0 {
                ctx.mem.write_u64(a[1], 0)?;
                ctx.mem.write_u64(a[1] + 8, 1)?;
            }
            Ok(0)
        }
        nr::MREMAP => {
            // Расширение отображения на месте.
            let old = a[0];
            let _old_size = a[1];
            let new_size = a[2];
            let _ = ctx.mem.map(old, (new_size + 0xFFF) & !0xFFF, 7, "mremap");
            Ok(old)
        }
        nr::CLONE3 => Ok(0),

        // ---- прочее
        nr::SIGACTION | nr::SIGPROCMASK | nr::SIGALTSTACK | nr::SIGRETURN => Ok(0),
        nr::SYSCONF => {
            // _SC_NPROCESSORS_ONLN = 58, _SC_PAGESIZE = 29 и т.п.
            match a[0] {
                58 => Ok(8),
                29 => Ok(0x4000),
                0 => Ok(1),
                _ => Ok(1),
            }
        }
        nr::GETOSLEVEL => Ok(0x0500_0000),
        nr::ISSETUGID => Ok(0),
        nr::ISSETPROCTITLE => Ok(0),
        nr::CPUSET_GETAFFINITY => {
            if a[1] != 0 {
                ctx.mem.write_u64(a[1], 0xFF)?;
            }
            Ok(0)
        }
        nr::CPUSET_SETAFFINITY | nr::RT_PRIO_THREAD => Ok(0),
        nr::UMTX_OP => Ok(0),
        nr::KQUEUE => {
            let fd = ctx.sys.alloc_fd(FileObj::Stdin);
            Ok(fd as u64)
        }
        nr::KEVMACRO => Ok(0),
        nr::SELECT | nr::POLL => Ok(0),
        nr::SYNC => Ok(0),
        nr::EXECVE => Err(ENOEXEC),
        nr::NETSTATS => Ok(0),
        nr::LOOKUP_SYMLINK => Err(ENOENT),
        _ => Err(ENOSYS),
    }
}

fn clone_file(f: &FileObj) -> Option<FileObj> {
    Some(match f {
        FileObj::Stdin => FileObj::Stdin,
        FileObj::Stdout => FileObj::Stdout,
        FileObj::Stderr => FileObj::Stderr,
        FileObj::Rng => FileObj::Rng,
        FileObj::Socket { domain, ty, connected } => {
            FileObj::Socket { domain: *domain, ty: *ty, connected: *connected }
        }
        FileObj::Mem { .. } => return None,
        FileObj::Dir { .. } => return None,
    })
}

fn write_fd(sys: &mut Sys, fd: i32, data: &[u8]) -> SysResult {
    let mut sync: Option<(String, Vec<u8>)> = None;
    let written = match sys.open_files.get_mut(&fd) {
        Some(FileObj::Stdout) => {
            let s = String::from_utf8_lossy(data);
            print!("{}", s);
            sys.stdout.push_str(&s);
            Ok(data.len() as u64)
        }
        Some(FileObj::Stderr) => {
            let s = String::from_utf8_lossy(data);
            eprint!("{}", s);
            sys.stderr_buf.push_str(&s);
            Ok(data.len() as u64)
        }
        Some(FileObj::Stdin) => Ok(0),
        Some(FileObj::Mem { path, data: d, pos, writable }) => {
            if !*writable {
                return Err(EBADF);
            }
            let start = *pos as usize;
            if start + data.len() > d.len() {
                d.resize(start + data.len(), 0);
            }
            d[start..start + data.len()].copy_from_slice(data);
            *pos += data.len() as u64;
            sync = Some((path.clone(), d.clone()));
            // Синхронизация выполняется после выхода из match (нужен &mut Sys).
            Ok(data.len() as u64)
        }
        Some(FileObj::Dir { .. }) => Err(EIO),
        Some(FileObj::Rng) => Err(EINVAL),
        Some(FileObj::Socket { .. }) => Ok(data.len() as u64),
        None => Err(EBADF),
    };
    let n: u64 = written?;
    if let Some((p, content)) = sync {
        sys.vfs.insert(p, content);
    }
    Ok(n)
}

pub fn prot_to_ps4(prot: u32) -> u32 {
    let mut p = 0u32;
    if prot & 1 != 0 {
        p |= crate::mem::P_READ;
    }
    if prot & 2 != 0 {
        p |= crate::mem::P_WRITE;
    }
    if prot & 4 != 0 {
        p |= crate::mem::P_EXEC;
    }
    p
}

/// Сохранение файлов, созданных гостем, в каталог на хосте.
pub fn flush_outputs(sys: &Sys, dir: &str) {
    let base = std::path::Path::new(dir);
    for (path, data) in &sys.vfs {
        let rel = path.trim_start_matches('/');
        if rel.is_empty() || rel.contains("..") {
            continue;
        }
        let out = base.join(rel);
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::File::create(&out) {
            let _ = f.write_all(data);
        }
    }
}

/// Утилита для загрузки файла с хоста в VFS.
pub fn host_read(path: &str) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    let mut v = Vec::new();
    f.read_to_end(&mut v)?;
    Ok(v)
}

/// Записать буфер в файл хоста (для дампов).
pub fn host_write(path: &str, data: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(data)?;
    f.seek(SeekFrom::Start(0))?;
    Ok(())
}
