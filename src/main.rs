//! Консольный интерфейс эмулятора ps4emu.

use ps4emu::config::Config;
use ps4emu::emulator::{Emulator, StopReason};
use ps4emu::oracle;
use std::path::PathBuf;

const HELP: &str = "\
ps4emu — экспериментальный эмулятор PlayStation 4

Использование:
  ps4emu run <файл.elf|eboot.bin> [параметры]
  ps4emu info <файл>            — сведения об ELF/SELF
  ps4emu symbols <файл>         — таблица символов и импортов
  ps4emu disas <файл> [n]       — дизассемблировать код по точке входа
  ps4emu nid <имя|nid>          — вычислить NID функции / найти имя по NID
  ps4emu mem <файл>             — карта памяти после загрузки
  ps4emu bench <файл> [n]       — измерить скорость интерпретации
  ps4emu modules                — список известных системных модулей
  ps4emu hle                    — список реализованных HLE-функций
  ps4emu debug <файл>           — интерактивный отладчик
  ps4emu screenshot <файл> <изображение.bmp|ppm>  — снять кадр гостя
  ps4emu patch <файл> <патч.txt> [--out F]        — применить патчи к файлу
  ps4emu pkg <файл.pkg>         — разобрать контейнер PKG/FPKG
  ps4emu snapshot save|load|info ...              — снимки состояния

Параметры запуска:
  --steps N            предел исполняемых инструкций
  --trace              трассировка syscalls и HLE
  --trace-cpu          трассировка инструкций
  --no-stats           не печатать статистику
  --out DIR            каталог для файлов гостя
  --nids FILE          таблица NID -> имя (JSON)
  --log FILE           записать лог в файл
  --width/--height     размер экрана
  --window             открыть окно вывода (если собрано с GUI)
  --fixed-time         детерминированное время
  --arg A              аргумент гостевой программы (можно несколько)
  --pad СЦЕНАРИЙ       нажатия геймпада, например cross+left
  --screenshot ФАЙЛ    сохранить кадр в конце запуска
  -q, --quiet          тихий режим
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        print!("{}", HELP);
        return;
    }
    let cmd = args[0].as_str();
    let rest = &args[1..];
    let code = match cmd {
        "run" => cmd_run(rest),
        "info" => cmd_info(rest),
        "symbols" => cmd_symbols(rest),
        "disas" => cmd_disas(rest),
        "nid" => cmd_nid(rest),
        "mem" => cmd_mem(rest),
        "bench" => cmd_bench(rest),
        "modules" => {
            for m in oracle::MODULES {
                println!("{}", m);
            }
            0
        }
        "hle" => {
            for f in ps4emu::hle::SUPPORTED {
                println!("{:<48} {}", f, oracle::module_of(f));
            }
            0
        }
        "patch" => cmd_patch(rest),
        "pkg" => cmd_pkg(rest),
        "snapshot" => cmd_snapshot(rest),
        "debug" => cmd_debug(rest),
        "screenshot" | "shot" => cmd_screenshot(rest),
        "self-test" => cmd_self_test(),
        other => {
            eprintln!("неизвестная команда: {}\n", other);
            print!("{}", HELP);
            2
        }
    };
    std::process::exit(code);
}

struct Opts {
    config: Config,
    quiet: bool,
    guest_args: Vec<String>,
    linux_abi: bool,
    /// Был ли явно задан `--out`.
    out_set: bool,
}

fn parse_opts(args: &[String]) -> (Opts, Vec<String>) {
    let mut o = Opts {
        config: Config::default(),
        quiet: false,
        guest_args: Vec::new(),
        linux_abi: false,
        out_set: false,
    };
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--steps" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    o.config.max_steps = parse_num(v);
                }
            }
            "--trace" => o.config.trace = true,
            "--trace-cpu" => {
                o.config.trace_cpu = true;
                o.config.trace = true;
            }
            "--no-stats" => o.config.stats = false,
            "--out" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    o.config.out_dir = PathBuf::from(v);
                    o.out_set = true;
                }
            }
            "--nids" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    o.config.nids = Some(PathBuf::from(v));
                }
            }
            "--log" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    o.config.log_file = Some(PathBuf::from(v));
                }
            }
            "--width" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    o.config.width = parse_num(v) as u32;
                }
            }
            "--height" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    o.config.height = parse_num(v) as u32;
                }
            }
            "--window" => o.config.window = true,
            "--pad" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    o.config.pad_script = Some(v.clone());
                }
            }
            "--screenshot" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    o.config.screenshot = Some(PathBuf::from(v));
                }
            }
            "--linux" => o.linux_abi = true,
            "--fixed-time" => o.config.fixed_time = true,
            "--arg" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    o.guest_args.push(v.clone());
                }
            }
            "-q" | "--quiet" => o.quiet = true,
            other => positional.push(other.to_string()),
        }
        i += 1;
    }
    (o, positional)
}

fn parse_num(s: &str) -> u64 {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).unwrap_or(0)
    } else if let Some(k) = s.strip_suffix('k') {
        k.parse::<u64>().unwrap_or(0) * 1000
    } else if let Some(k) = s.strip_suffix('M') {
        k.parse::<u64>().unwrap_or(0) * 1_000_000
    } else {
        s.parse().unwrap_or(0)
    }
}

fn cmd_run(args: &[String]) -> i32 {
    let (o, positional) = parse_opts(args);
    let file = match positional.first() {
        Some(f) => PathBuf::from(f),
        None => {
            eprintln!("не указан файл для запуска");
            return 2;
        }
    };
    let mut emu = match Emulator::new(o.config.clone()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("не удалось создать эмулятор: {}", e);
            return 1;
        }
    };
    if o.linux_abi {
        emu.sys.abi = ps4emu::sys::Abi::Linux;
    }
    if let Some(script) = &o.config.pad_script {
        emu.pad.apply_script(script);
        emu.sys.pad = emu.pad.clone();
    }
    if let Err(e) = emu.load(&file) {
        eprintln!("не удалось загрузить {:?}: {}", file, e);
        return 1;
    }
    if !o.quiet {
        eprintln!(
            "[ps4emu] загружено {:?}: точка входа {:#x}, модулей {}, импортов {}",
            file.file_name().unwrap_or_default(),
            emu.modules.last().map(|m| m.entry).unwrap_or(0),
            emu.modules.len(),
            emu.hle.exports.len()
        );
    }
    let args = if o.guest_args.is_empty() { vec![emu.config.app0.clone()] } else { o.guest_args.clone() };
    let res = match emu.execute(&args, &[]) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ошибка запуска: {}", e);
            return 1;
        }
    };
    if !o.quiet {
        match &res.reason {
            StopReason::Exit(code) => eprintln!("[ps4emu] процесс завершён с кодом {}", code),
            StopReason::StepLimit => eprintln!("[ps4emu] достигнут предел в {} инструкций", res.steps),
            StopReason::Halt => eprintln!("[ps4emu] остановка (hlt)"),
            StopReason::Fault(msg) => {
                eprintln!("[ps4emu] сбой: {}", msg);
                eprintln!("{}", emu.fault_report(&ps4emu::cpu::CpuError::Custom(msg.clone())));
            }
            StopReason::HleStop(msg) => eprintln!("[ps4emu] остановлено HLE: {}", msg),
            StopReason::Breakpoint(a) => eprintln!("[ps4emu] точка останова на {:#x}", a),
        }
    }
    // Печать вывода гостя (если он шёл не в stdout эмулятора)
    if !res.stdout.is_empty() && !o.quiet {
        eprintln!("[ps4emu] суммарный вывод гостя: {} байт", res.stdout.len());
    }
    if let Some(path) = &o.config.screenshot {
        // Если гость не вызывал SubmitFlip, снимаем текущее содержимое буфера.
        if emu.gpu.last_frame.is_none() {
            let (w, h) = emu.sys.fb_size;
            let addr = emu.sys.framebuffers.get(&0).copied().unwrap_or(0);
            if addr != 0 {
                emu.gpu.capture_guest(&mut emu.mem, addr, w, h, w * 4);
            }
        }
        let p = path.to_string_lossy().to_string();
        let saved = if p.ends_with(".bmp") { emu.gpu.save_bmp(&p) } else { emu.gpu.save_ppm(&p) };
        match saved {
            Ok(()) => eprintln!("[ps4emu] кадр сохранён: {}", p),
            Err(e) => eprintln!("[ps4emu] не удалось сохранить кадр: {}", e),
        }
    }
    if let Some(log_path) = &o.config.log_file {
        let mut text = emu.sys.log.join("\n");
        text.push_str("\n\n");
        text.push_str(&emu.stats_report());
        let _ = std::fs::write(log_path, text);
    }
    if o.config.stats && !o.quiet {
        eprintln!("{}", emu.stats_report());
    }
    if !o.quiet {
        ps4emu::sys::flush_outputs(&emu.sys, &o.config.out_dir.to_string_lossy());
    }
    match res.reason {
        StopReason::Exit(c) => c,
        StopReason::Fault(_) => 3,
        _ => 0,
    }
}

fn cmd_info(args: &[String]) -> i32 {
    let (_, positional) = parse_opts(args);
    let f = match positional.first() {
        Some(f) => f.clone(),
        None => {
            eprintln!("не указан файл");
            return 2;
        }
    };
    let raw = match std::fs::read(&f) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("не удалось прочитать {}: {}", f, e);
            return 1;
        }
    };
    let data = match ps4emu::emulator::unwrap_container(&raw) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    let elf = match ps4emu::elf::parse(&data) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("не удалось разобрать ELF: {}", e);
            return 1;
        }
    };
    println!("файл:      {} ({} байт{})", f, raw.len(),
        if raw.len() != data.len() { format!(", ELF-часть {} байт", data.len()) } else { String::new() });
    println!("тип:       {}", if elf.e_type == 2 { "ET_EXEC" } else { "ET_DYN (PIE)" });
    println!("машина:    x86-64");
    println!("точка входа: {:#x}", elf.entry);
    if let Some(i) = &elf.interp {
        println!("интерпретатор: {}", i);
    }
    println!("сегменты:");
    for ph in &elf.phdrs {
        let t = match ph.p_type {
            1 => "LOAD",
            2 => "DYNAMIC",
            3 => "INTERP",
            4 => "NOTE",
            6 => "PHDR",
            7 => "TLS",
            _ => "прочее",
        };
        println!(
            "  {:<8} vaddr={:#x} filesz={:#x} memsz={:#x} flags={}{}{}",
            t,
            ph.vaddr,
            ph.filesz,
            ph.memsz,
            if ph.flags & 4 != 0 { 'r' } else { '-' },
            if ph.flags & 2 != 0 { 'w' } else { '-' },
            if ph.flags & 1 != 0 { 'x' } else { '-' }
        );
    }
    if !elf.dynamic.needed.is_empty() {
        println!("зависимости: {:?}", elf.dynamic.needed);
    }
    let funcs = elf.symbols.iter().filter(|s| s.is_func() && s.value != 0).count();
    println!("символов-функций: {}", funcs);
    if !elf.sections.is_empty() {
        println!("секций: {}", elf.sections.len());
    }
    0
}

fn cmd_symbols(args: &[String]) -> i32 {
    let (_, positional) = parse_opts(args);
    let f = match positional.first() {
        Some(f) => f.clone(),
        None => {
            eprintln!("не указан файл");
            return 2;
        }
    };
    let raw = std::fs::read(&f).unwrap_or_default();
    let data = ps4emu::emulator::unwrap_container(&raw).unwrap_or(raw);
    let elf = match ps4emu::elf::parse(&data) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    println!("== экспортируемые функции ==");
    for s in elf.symbols.iter().filter(|s| s.is_func() && s.value != 0) {
        let name = elf.symbol_name(s);
        if name.is_empty() {
            continue;
        }
        println!("{:016x}  {:<48} {}", s.value, name, oracle::module_of(name));
    }
    let imports = elf.imports(&data);
    println!("\n== импортируемые символы ({}) ==", imports.len());
    for (name, addr) in &imports {
        let mark = if ps4emu::hle::supported(name) { "+" } else { "-" };
        println!("{} {:016x}  {:<40} {}", mark, addr, name, oracle::module_of(name));
    }
    0
}

fn cmd_disas(args: &[String]) -> i32 {
    let (_, positional) = parse_opts(args);
    let f = match positional.first() {
        Some(f) => f.clone(),
        None => {
            eprintln!("не указан файл");
            return 2;
        }
    };
    let n: usize = positional.get(1).and_then(|s| s.parse().ok()).unwrap_or(32);
    let raw = std::fs::read(&f).unwrap_or_default();
    let data = ps4emu::emulator::unwrap_container(&raw).unwrap_or(raw);
    if let Err(e) = ps4emu::elf::parse(&data) {
        eprintln!("{}", e);
        return 1;
    }
    // Загружаем в память и идём по коду
    let mut emu = match Emulator::new(Config::default()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    if let Err(e) = emu.load_image(&data, &f, None) {
        eprintln!("{}", e);
        return 1;
    }
    let entry = emu.modules.last().map(|m| m.entry).unwrap_or(0);
    let mut pc = entry;
    println!("дизассемблирование с точки входа {:#x}:", entry);
    for _ in 0..n {
        let mut b = [0u8; 16];
        let _ = emu.mem.read_partial(pc, &mut b);
        let sym = emu.symbols.resolve(pc).unwrap_or_default();
        let (text, len) = ps4emu::trace::disasm_len(&b, pc);
        println!("{:016x}  {:<32} {}", pc, text, sym);
        pc = pc.wrapping_add(len as u64);
    }
    0
}


fn cmd_nid(args: &[String]) -> i32 {
    let (_, positional) = parse_opts(args);
    let arg = match positional.first() {
        Some(a) => a.clone(),
        None => {
            eprintln!("укажите имя функции или NID");
            return 2;
        }
    };
    if arg.len() == 8 && arg.chars().all(|c| c.is_ascii_hexdigit()) {
        let nid = u32::from_str_radix(&arg, 16).unwrap_or(0);
        let mut t = ps4emu::oracle::NidTable::new();
        t.populate_known();
        println!("NID {:#010x} -> {}", nid, t.lookup(nid).unwrap_or("неизвестная функция"));
    } else {
        let nid = oracle::nid_from_name(&arg);
        println!("{:<40} NID = {:08x}  модуль: {}", arg, nid, oracle::module_of(&arg));
    }
    0
}

fn cmd_mem(args: &[String]) -> i32 {
    let (o, positional) = parse_opts(args);
    let f = match positional.first() {
        Some(f) => f.clone(),
        None => {
            eprintln!("не указан файл");
            return 2;
        }
    };
    let mut emu = match Emulator::new(o.config.clone()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    if let Err(e) = emu.load(std::path::Path::new(&f)) {
        eprintln!("{}", e);
        return 1;
    }
    print!("{}", emu.memory_map());
    0
}

fn cmd_bench(args: &[String]) -> i32 {
    let (o, positional) = parse_opts(args);
    let f = match positional.first() {
        Some(f) => f.clone(),
        None => {
            eprintln!("не указан файл");
            return 2;
        }
    };
    let steps: u64 = positional.get(1).and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    let mut emu = match Emulator::new(o.config.clone()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    if let Err(e) = emu.load(std::path::Path::new(&f)) {
        eprintln!("{}", e);
        return 1;
    }
    let mut cfg = emu.config.clone();
    cfg.max_steps = steps;
    emu.config = cfg;
    let args = vec![emu.config.app0.clone()];
    let t0 = std::time::Instant::now();
    let res = emu.execute(&args, &[]);
    let dt = t0.elapsed().as_secs_f64();
    match res {
        Ok(r) => {
            println!(
                "выполнено {} инструкций за {:.3} с = {:.1} млн инстр./с",
                r.steps,
                dt,
                r.steps as f64 / dt / 1e6
            );
            println!("причина остановки: {:?}", r.reason);
            if let StopReason::Exit(c) = r.reason {
                println!(
                    "программа завершилась с кодом {} — для замера скорости нужен гость с длинным циклом",
                    c
                );
            }
            if !r.stdout.is_empty() {
                println!("вывод гостя: {}", r.stdout.chars().take(2000).collect::<String>());
            }
        }
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    }
    0
}


fn cmd_patch(args: &[String]) -> i32 {
    let (o, mut positional) = parse_opts(args);
    let out_path: Option<PathBuf> = if o.out_set { Some(o.config.out_dir.clone()) } else { None };
    if positional.is_empty() {
        eprintln!("использование: ps4emu patch <файл> <патч.txt> [--out результат]");
        return 2;
    }
    let target = PathBuf::from(positional.remove(0));
    let patches = if let Some(p) = positional.first() {
        PathBuf::from(p)
    } else {
        eprintln!("не указан файл патча");
        return 2;
    };
    let out = out_path.unwrap_or_else(|| {
        let mut n = target.clone();
        let stem = n.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        n.set_file_name(format!("{}.patched", stem));
        n
    });
    match ps4emu::patch::apply_patch_file(&target, &patches, &out) {
        Ok(log) => {
            for l in log {
                println!("{}", l);
            }
            println!("результат записан в {:?}", out);
            0
        }
        Err(e) => {
            eprintln!("патч не применён: {}", e);
            1
        }
    }
}

fn cmd_pkg(args: &[String]) -> i32 {
    let (_, pos) = parse_opts(args);
    let f = match pos.first() {
        Some(f) => PathBuf::from(f),
        None => {
            eprintln!("использование: ps4emu pkg <файл.pkg>");
            return 2;
        }
    };
    let data = match std::fs::read(&f) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("не читается {:?}: {}", f, e);
            return 1;
        }
    };
    if let Some(files) = ps4emu::patch::OpenPkg::parse(&data) {
        println!("контейнер FPKG: файлов {}", files.len());
        for (name, body) in files {
            println!("  {:<40} {} байт", name, body.len());
        }
        return 0;
    }
    match ps4emu::patch::parse_pkg_header(&data) {
        Ok(h) => {
            println!("PKG: тип {}, файлов {}, записей {}, размер {}", h.pkg_type, h.file_count, h.entry_count, h.total_size);
            if !h.content_id.is_empty() {
                println!("  content id: {}", h.content_id);
            }
            for e in ps4emu::patch::parse_pkg_entries(&data, &h) {
                println!("  #{:<3} {:<40} {:>10} байт @ {:#x}", e.id, e.name, e.size, e.offset);
            }
            0
        }
        Err(e) => {
            eprintln!("{}", e);
            1
        }
    }
}

fn cmd_snapshot(args: &[String]) -> i32 {
    let (o, pos) = parse_opts(args);
    // ps4emu snapshot save <elf> <snap> [--steps N]
    // ps4emu snapshot info <snap>
    let sub = pos.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "save" => {
            let (elf, snap) = match (pos.get(1), pos.get(2)) {
                (Some(a), Some(b)) => (PathBuf::from(a), PathBuf::from(b)),
                _ => {
                    eprintln!("использование: ps4emu snapshot save <elf> <файл.snap> [--steps N]");
                    return 2;
                }
            };
            let mut cfg = o.config.clone();
            // По умолчанию останавливаемся, не доходя до конца программы:
            // снимок посреди выполнения полезнее, чем снимок после exit.
            if cfg.max_steps == 0 {
                cfg.max_steps = 1_000_000;
            }
            let mut emu = match Emulator::new(cfg) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("{}", e);
                    return 1;
                }
            };
            if let Err(e) = emu.load(&elf) {
                eprintln!("{}", e);
                return 1;
            }
            let args = vec![emu.config.app0.clone()];
            match emu.execute(&args, &[]) {
                Ok(r) => {
                    if !o.quiet {
                        eprintln!("[ps4emu] остановка перед снимком: {:?} на {} инструкциях", r.reason, r.steps);
                    }
                    if matches!(r.reason, StopReason::Exit(_)) {
                        eprintln!("[ps4emu] предупреждение: программа уже завершилась, снимок будет пустым для продолжения");
                    }
                }
                Err(e) => {
                    eprintln!("{}", e);
                    return 1;
                }
            }
            match ps4emu::snapshot::save_file(&mut emu, &snap.to_string_lossy()) {
                Ok(n) => {
                    println!("снимок записан: {:?} ({} байт)", snap, n);
                    0
                }
                Err(e) => {
                    eprintln!("не удалось сохранить снимок: {}", e);
                    1
                }
            }
        }
        "load" => {
            let (elf, snap) = match (pos.get(1), pos.get(2)) {
                (Some(a), Some(b)) => (PathBuf::from(a), PathBuf::from(b)),
                _ => {
                    eprintln!("использование: ps4emu snapshot load <elf> <файл.snap>");
                    return 2;
                }
            };
            let mut emu = match Emulator::new(o.config.clone()) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("{}", e);
                    return 1;
                }
            };
            if let Err(e) = emu.load(&elf) {
                eprintln!("{}", e);
                return 1;
            }
            if let Err(e) = ps4emu::snapshot::load_file(&mut emu, &snap.to_string_lossy()) {
                eprintln!("не удалось загрузить снимок: {}", e);
                return 1;
            }
            // Программный счётчик восстанавливается из снимка — не сбрасываем
            // его через setup_stack/execute, а продолжаем выполнение.
            if std::env::var("PS4EMU_VERBOSE").is_ok() {
                eprintln!(
                    "[snapshot] rip={:#x} rsp={:#x} fs={:#x} модулей {}",
                    emu.cpu.regs.rip,
                    emu.cpu.regs.gpr[4],
                    emu.cpu.regs.fs_base,
                    emu.modules.len()
                );
            }
            let r = emu.run();
            println!("после загрузки снимка: {:?} ({} инструкций)", r.reason, r.steps);
            0
        }
        "info" => {
            let f = match pos.get(1) {
                Some(f) => f,
                None => {
                    eprintln!("использование: ps4emu snapshot info <файл.snap>");
                    return 2;
                }
            };
            let data = match std::fs::read(f) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("{}", e);
                    return 1;
                }
            };
            if !data.starts_with(ps4emu::snapshot::MAGIC) {
                eprintln!("это не снимок ps4emu");
                return 1;
            }
            let ver = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
            println!("снимок {}: версия {}, {} байт, CRC {:08x}", f, ver, data.len(),
                u32::from_le_bytes([data[data.len()-4], data[data.len()-3], data[data.len()-2], data[data.len()-1]]));
            0
        }
        _ => {
            eprintln!("ps4emu snapshot save|load|info ...");
            2
        }
    }
}

fn cmd_debug(args: &[String]) -> i32 {
    let (o, pos) = parse_opts(args);
    let f = match pos.first() {
        Some(f) => PathBuf::from(f),
        None => {
            eprintln!("использование: ps4emu debug <файл>");
            return 2;
        }
    };
    let mut emu = match Emulator::new(o.config.clone()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    if o.linux_abi {
        emu.sys.abi = ps4emu::sys::Abi::Linux;
    }
    if let Err(e) = emu.load(&f) {
        eprintln!("{}", e);
        return 1;
    }
    let args = vec![emu.config.app0.clone()];
    if let Err(e) = emu.setup_stack(&args, &[]) {
        eprintln!("не удалось подготовить стек: {}", e);
        return 1;
    }
    // Ставим процессор на точку входа, сохранив подготовленный стек.
    let entry = emu.modules.last().map(|m| m.entry).unwrap_or(0);
    emu.cpu.reset(entry, emu.cpu.regs.gpr[4]);
    let mut dbg = ps4emu::debug::Debugger::new(emu);
    match dbg.run_interactive() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{}", e);
            1
        }
    }
}

fn cmd_screenshot(args: &[String]) -> i32 {
    // ps4emu screenshot <elf> <файл.bmp> [--steps N] [--pad сценарий]
    let (o, pos) = parse_opts(args);
    let (elf, out) = match (pos.first(), pos.get(1)) {
        (Some(a), Some(b)) => (PathBuf::from(a), PathBuf::from(b)),
        _ => {
            eprintln!("использование: ps4emu screenshot <elf> <файл.bmp|ppm> [--steps N]");
            return 2;
        }
    };
    let mut emu = match Emulator::new(o.config.clone()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    if let Err(e) = emu.load(&elf) {
        eprintln!("{}", e);
        return 1;
    }
    if let Some(s) = &o.config.pad_script {
        emu.pad.apply_script(s);
        emu.sys.pad = emu.pad.clone();
    }
    let args = vec![emu.config.app0.clone()];
    let res = emu.execute(&args, &[]);
    match res {
        Ok(r) => {
            if !o.quiet {
                eprintln!("[ps4emu] {:?} за {} инструкций", r.reason, r.steps);
            }
        }
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    }
    if !emu.capture_last_frame() {
        let (w, h) = emu.sys.fb_size;
        let addr = emu.sys.framebuffers.get(&0).copied().unwrap_or(0);
        if addr != 0 {
            emu.gpu.capture_guest(&mut emu.mem, addr, w, h, w * 4);
        }
    }
    if emu.gpu.last_frame.is_none() {
        eprintln!("гость не представил ни одного кадра (sceVideoOutSubmitFlip не вызывался)");
        return 1;
    }
    let p = out.to_string_lossy().to_string();
    let r = if p.ends_with(".bmp") { emu.gpu.save_bmp(&p) } else { emu.gpu.save_ppm(&p) };
    match r {
        Ok(()) => {
            println!("{}", emu.gpu.stats());
            println!("кадр сохранён: {}", p);
            0
        }
        Err(e) => {
            eprintln!("не удалось сохранить кадр: {}", e);
            1
        }
    }
}

fn cmd_self_test() -> i32 {
    println!("проверка ядра эмулятора:");
    println!("  версия: {}", env!("CARGO_PKG_VERSION"));
    println!("  модулей в базе: {}", oracle::MODULES.len());
    println!("  известных экспортов: {}", oracle::KNOWN_EXPORTS.len());
    println!("  HLE-функций: {}", ps4emu::hle::SUPPORTED.len());
    println!("  NID sceKernelGetProcessId = {:08x}", oracle::nid_from_name("sceKernelGetProcessId"));
    0
}
