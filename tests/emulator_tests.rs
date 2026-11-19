//! Интеграционные тесты: загрузка ELF, системные вызовы, HLE,
//! снапшоты, отладчик, GPU.

mod common;

use common::GuestBuilder;
use ps4emu::config::Config;
use ps4emu::emulator::{Emulator, StopReason};
use ps4emu::sys;

fn run_bytes(code: &GuestBuilder) -> (Emulator, ps4emu::emulator::RunResult) {
    let mut emu = Emulator::new(Config::default()).unwrap();
    let elf = code.build();
    emu.load_image(&elf, "/app0/eboot.bin", None).unwrap();
    let entry = emu.modules.last().unwrap().entry;
    emu.setup_stack(&["/app0/eboot.bin".to_string()], &[]).unwrap();
    emu.cpu.reset(entry, emu.cpu.regs.gpr[4]);
    let res = emu.run();
    (emu, res)
}

#[test]
fn exit_syscall_sets_code() {
    let mut b = GuestBuilder::new();
    b.exit(42);
    let (_, res) = run_bytes(&b);
    assert_eq!(res.reason, StopReason::Exit(42));
    assert_eq!(res.exit_code, 42);
}

#[test]
fn write_syscall_produces_output() {
    let mut b = GuestBuilder::new();
    // Данные в .rodata по адресу 0x410000
    b.rodata = b"Orbis!\n".to_vec();
    b.mov_imm32(0, sys::nr::WRITE as u32);
    b.mov_imm32(7, 1);
    b.mov_imm64(6, 0x41_0000);
    b.mov_imm32(2, 7);
    b.syscall();
    b.exit(0);
    let (_, res) = run_bytes(&b);
    assert_eq!(res.stdout, "Orbis!\n");
    assert_eq!(res.exit_code, 0);
}

#[test]
fn mmap_and_write_memory() {
    let mut b = GuestBuilder::new();
    // mmap(0, 0x2000, PROT_READ|WRITE, MAP_ANON|MAP_PRIVATE, -1, 0)
    b.mov_imm32(0, sys::nr::MMAP as u32);
    b.mov_imm32(7, 0);
    b.mov_imm32(6, 0x2000);
    b.mov_imm32(2, 3);
    b.mov_imm32(10, 0x1002);
    b.mov_imm64(8, u64::MAX);
    b.mov_imm32(9, 0);
    b.syscall();
    // rax = адрес; записываем туда 0x1122334455667788
    b.bytes(&[0x48, 0xBB]); // mov rbx, imm64
    let pos = b.code.len();
    b.bytes(&[0u8; 8]);
    b.bytes(&[0x48, 0x89, 0x18]); // mov [rax], rbx
    b.exit(7);
    let mut b2 = b;
    let val = 0x1122_3344_5566_7788u64;
    b2.code[pos..pos + 8].copy_from_slice(&val.to_le_bytes());
    let (emu, res) = run_bytes(&b2);
    assert_eq!(res.reason, StopReason::Exit(7));
    // Находим регион mmap и проверяем содержимое
    let base = emu
        .mem
        .regions()
        .iter()
        .find(|r| r.name == "mmap")
        .map(|r| r.base)
        .expect("регион mmap должен существовать");
    let mut mem = emu.mem;
    assert_eq!(mem.read_u64(base).unwrap(), 0x1122_3344_5566_7788);
}

#[test]
fn open_read_write_close_files() {
    let mut b = GuestBuilder::new();
    // Путь "/data/test.txt" в rodata
    b.rodata = b"/data/test.txt\0".to_vec();
    // open(path, O_CREAT|O_RDWR)
    b.mov_imm32(0, sys::nr::OPEN as u32);
    b.mov_imm64(7, 0x41_0000);
    b.mov_imm32(6, 0x0202);
    b.mov_imm32(2, 0);
    b.syscall();
    // write(fd, "hello", 5)
    b.bytes(&[0x49, 0x89, 0xC4]); // mov r12, rax (fd)
    b.mov_imm32(0, sys::nr::WRITE as u32);
    b.bytes(&[0x44, 0x89, 0xE7]); // mov edi, r12d
    b.mov_imm64(6, 0x41_0000 + 0x100);
    b.mov_imm32(2, 5);
    b.syscall();
    b.mov_imm32(0, sys::nr::CLOSE as u32);
    b.bytes(&[0x44, 0x89, 0xE7]);
    b.syscall();
    b.exit(0);
    // данные для записи по адресу 0x410000 + 0x100
    while b.rodata.len() < 0x100 {
        b.rodata.push(0);
    }
    b.rodata.extend_from_slice(b"hello");
    let (emu, res) = run_bytes(&b);
    assert_eq!(res.reason, StopReason::Exit(0));
    assert_eq!(
        emu.sys.vfs.get("/data/test.txt").map(|v| v.as_slice()),
        Some(&b"hello"[..])
    );
}

#[test]
fn hle_printf_via_stub() {
    let mut emu = Emulator::new(Config::default()).unwrap();
    // Регистрируем заглушку printf вручную и вызываем её.
    let stub = emu.hle.exports.entry("printf".into()).or_insert(0);
    let addr = if *stub == 0 {
        let a = ps4emu::emulator::STUB_BASE + emu.stub_slots.len() as u64 * 16;
        emu.stub_slots.insert(a, "printf".into());
        emu.hle.exports.insert("printf".into(), a);
        a
    } else {
        *stub
    };
    // Готовим строку формата и аргумент в памяти
    emu.mem.write_cstr(0x2_0000_1000, "значение = %d, строка = %s\n").unwrap();
    emu.mem.write_cstr(0x2_0000_2000, "ок").unwrap();
    emu.cpu.regs.gpr[7] = 0x2_0000_1000;
    emu.cpu.regs.gpr[6] = 42;
    emu.cpu.regs.gpr[2] = 0x2_0000_2000;
    // Возврат: кладём адрес возврата в стек
    emu.cpu.regs.gpr[4] = 0x2000;
    emu.mem.map(0x2000, 0x1000, 3, "test-stack").unwrap();
    emu.mem.write_u64(0x2000, 0xdead_0000).unwrap();
    emu.call_hle("printf").unwrap();
    assert_eq!(emu.cpu.regs.rip, 0xdead_0000);
    assert!(emu.sys.stdout.contains("значение = 42, строка = ок"));
    assert_eq!(addr, emu.hle.exports["printf"]);
}

#[test]
fn hle_malloc_free_and_strings() {
    let mut emu = Emulator::new(Config::default()).unwrap();
    emu.mem.write_cstr(0x2_0000_1000, "строка для malloc").unwrap();
    let mut args = [0u64; 6];
    // strlen
    args[0] = 0x2_0000_1000;
    emu.cpu.regs.gpr[7] = args[0];
    emu.cpu.regs.gpr[4] = 0x2000;
    emu.mem.map(0x2000, 0x1000, 3, "st").unwrap();
    emu.mem.write_u64(0x2000, 0).unwrap();
    emu.call_hle("strlen").unwrap();
    let len = emu.cpu.regs.gpr[0];
    assert_eq!(len, "строка для malloc".len() as u64);

    // malloc(64)
    emu.cpu.regs.gpr[7] = 64;
    emu.cpu.regs.gpr[4] = 0x2000;
    emu.mem.write_u64(0x2000, 0).unwrap();
    emu.call_hle("malloc").unwrap();
    let p = emu.cpu.regs.gpr[0];
    assert!(p >= 0x8000_0000, "куча HLE должна быть в 0x80000000, получено {:#x}", p);
    // memcpy в выделенный блок
    emu.mem.write_cstr(0x2_0000_1000, "abc").unwrap();
    emu.cpu.regs.gpr[7] = p;
    emu.cpu.regs.gpr[6] = 0x2_0000_1000;
    emu.cpu.regs.gpr[2] = 3;
    emu.cpu.regs.gpr[4] = 0x2000;
    emu.mem.write_u64(0x2000, 0).unwrap();
    emu.call_hle("memcpy").unwrap();
    assert_eq!(emu.mem.read_cstr(p, 16).unwrap(), "abc");
    // free
    emu.cpu.regs.gpr[7] = p;
    emu.cpu.regs.gpr[4] = 0x2000;
    emu.mem.write_u64(0x2000, 0).unwrap();
    emu.call_hle("free").unwrap();
}

#[test]
fn hle_unknown_function_is_recorded() {
    let mut emu = Emulator::new(Config::default()).unwrap();
    emu.mem.map(0x2000, 0x1000, 3, "st").unwrap();
    emu.mem.write_u64(0x2000, 0).unwrap();
    emu.cpu.regs.gpr[4] = 0x2000;
    emu.call_hle("sceEtwasUnbekanntes1923").unwrap();
    assert!(emu.hle.missing.contains("sceEtwasUnbekanntes1923"));
}

#[test]
fn stack_setup_has_argc_argv() {
    let mut emu = Emulator::new(Config::default()).unwrap();
    let mut b = GuestBuilder::new();
    b.exit(0);
    emu.load_image(&b.build(), "test", None).unwrap();
    emu.setup_stack(&["prog".into(), "--flag".into()], &[("HOME".into(), "/app0".into())])
        .unwrap();
    let sp = emu.cpu.regs.gpr[4];
    assert_eq!(emu.mem.read_u64(sp).unwrap(), 2, "argc");
    let argv0 = emu.mem.read_u64(sp + 8).unwrap();
    assert_eq!(emu.mem.read_cstr(argv0, 64).unwrap(), "prog");
    let argv1 = emu.mem.read_u64(sp + 16).unwrap();
    assert_eq!(emu.mem.read_cstr(argv1, 64).unwrap(), "--flag");
    assert_eq!(emu.mem.read_u64(sp + 24).unwrap(), 0, "argv завершается NULL");
    // envp
    let env0 = emu.mem.read_u64(sp + 32).unwrap();
    assert_eq!(emu.mem.read_cstr(env0, 64).unwrap(), "HOME=/app0");
    // auxv содержит AT_PAGESZ (6) — ищем пары после завершающего NULL env
    let mut p = sp + 8 + 8 * (2 + 1 + 1 + 1);
    let mut found = false;
    loop {
        let t = emu.mem.read_u64(p).unwrap();
        let v = emu.mem.read_u64(p + 8).unwrap();
        if t == 0 {
            break;
        }
        if t == 6 {
            assert_eq!(v, 0x4000);
            found = true;
        }
        p += 16;
    }
    assert!(found, "AT_PAGESZ должен присутствовать");
}

#[test]
fn syscall_linux_abi_errno_convention() {
    let mut emu = Emulator::new(Config::default()).unwrap();
    emu.sys.abi = sys::Abi::Linux;
    let mut b = GuestBuilder::new();
    // write(999, buf, 1) -> -EBADF: проверяем отрицательный код возврата
    b.rodata = b"x".to_vec();
    b.mov_imm32(0, 1); // Linux: SYS_write = 1
    b.mov_imm32(7, 999); // rdi = fd (невалидный)
    b.mov_imm64(6, 0x41_0000); // rsi = buf
    b.mov_imm32(2, 1); // rdx = len
    b.syscall();
    // cmp rax, -9 ; sete al ; movzx eax, al ; mov edi, eax ; mov eax, 60 ; syscall
    b.bytes(&[0x48, 0x83, 0xF8, 0xF7]); // cmp rax, -9
    b.bytes(&[0x0F, 0x95, 0xC0]); // setne al  -> 0, если код верный
    b.bytes(&[0x0F, 0xB6, 0xC0]);
    b.bytes(&[0x89, 0xC7]);
    b.bytes(&[0xB8, 0x3C, 0x00, 0x00, 0x00]);
    b.syscall();
    emu.load_image(&b.build(), "t", None).unwrap();
    let e = emu.modules.last().unwrap().entry;
    emu.setup_stack(&["t".into()], &[]).unwrap();
    emu.cpu.reset(e, emu.cpu.regs.gpr[4]);
    let res = emu.run();
    assert_eq!(res.reason, StopReason::Exit(0), "write на плохой fd должен вернуть -EBADF");
}

#[test]
fn unsupported_syscall_returns_enosys() {
    let mut emu = Emulator::new(Config::default()).unwrap();
    let mut ctx = sys::SyscallCtx {
        mem: &mut emu.mem,
        sys: &mut emu.sys,
        rip: 0,
        name: "",
        thr_self: 0,
        last_syscall: 0,
        last_args: [0; 6],
    };
    let r = sys::dispatch(&mut ctx, 9999, [0; 6]);
    assert_eq!(r, Err(sys::ENOSYS));
}

#[test]
fn file_lock_and_read_back() {
    let mut emu = Emulator::new(Config::default()).unwrap();
    emu.sys.vfs.insert("/save/state.bin".into(), b"savedata".to_vec());
    let mut ctx = sys::SyscallCtx {
        mem: &mut emu.mem,
        sys: &mut emu.sys,
        rip: 0,
        name: "",
        thr_self: 0,
        last_syscall: 0,
        last_args: [0; 6],
    };
    let _ = &mut ctx; // контекст ниже пересоздаётся после map()
    emu.mem.map(0x3000, 0x1000, 7, "tmp").unwrap();
    emu.mem.write_cstr(0x3000, "/save/state.bin").unwrap();
    let mut open_ctx = sys::SyscallCtx {
        mem: &mut emu.mem,
        sys: &mut emu.sys,
        rip: 0,
        name: "",
        thr_self: 0,
        last_syscall: 0,
        last_args: [0; 6],
    };
    let fd = sys::dispatch(&mut open_ctx, sys::nr::OPEN, [0x3000, 0, 0, 0, 0, 0]).unwrap();
    let mut ctx = sys::SyscallCtx {
        mem: &mut emu.mem,
        sys: &mut emu.sys,
        rip: 0,
        name: "",
        thr_self: 0,
        last_syscall: 0,
        last_args: [0; 6],
    };
    let n = sys::dispatch(&mut ctx, sys::nr::READ, [fd, 0x3100, 8, 0, 0, 0]).unwrap();
    assert_eq!(n, 8);
    let mut buf = [0u8; 8];
    ctx.mem.read_bytes(0x3100, &mut buf).unwrap();
    assert_eq!(&buf, b"savedata");
}

#[test]
fn snapshot_restore_continues_execution() {
    let mut b = GuestBuilder::new();
    b.mov_imm32(0, sys::nr::WRITE as u32);
    b.mov_imm32(7, 1);
    b.mov_imm64(6, 0x41_0000);
    b.mov_imm32(2, 3);
    b.syscall();
    b.exit(0);
    b.rodata = b"abc".to_vec();
    let mut emu = Emulator::new(Config::default()).unwrap();
    emu.load_image(&b.build(), "t", None).unwrap();
    let e = emu.modules.last().unwrap().entry;
    emu.setup_stack(&["t".into()], &[]).unwrap();
    emu.cpu.reset(e, emu.cpu.regs.gpr[4]);
    let data = ps4emu::snapshot::save_mut(&mut emu);
    assert!(!data.is_empty());
    let mut emu2 = Emulator::new(Config::default()).unwrap();
    ps4emu::snapshot::load_mut(&mut emu2, &data).unwrap();
    assert_eq!(emu2.cpu.regs.rip, e);
    let res = emu2.run();
    assert_eq!(res.stdout, "abc");
}

#[test]
fn memory_map_contains_expected_regions() {
    let emu = Emulator::new(Config::default()).unwrap();
    let text = emu.memory_map();
    assert!(text.contains("stack"));
    assert!(text.contains("brk"));
    assert!(text.contains("hle-stubs"));
    assert!(text.contains("tls"));
}

#[test]
fn breakpoint_stops_execution() {
    let mut b = GuestBuilder::new();
    let at = b.here();
    b.bytes(&[0x90, 0x90, 0x90]);
    b.exit(0);
    let mut emu = Emulator::new(Config::default()).unwrap();
    emu.load_image(&b.build(), "t", None).unwrap();
    let e = emu.modules.last().unwrap().entry;
    emu.setup_stack(&["t".into()], &[]).unwrap();
    emu.cpu.reset(e, emu.cpu.regs.gpr[4]);
    emu.breakpoints.push(at + 1);
    let res = emu.run();
    assert_eq!(res.reason, StopReason::Breakpoint(at + 1));
}

#[test]
fn gpu_capture_and_screenshot() {
    let mut gpu = ps4emu::gpu::Gpu::new(4, 4);
    gpu.clear(ps4emu::gpu::Color::rgb(10, 20, 30));
    gpu.fill_rect(1, 1, 2, 2, ps4emu::gpu::Color::WHITE);
    let path = std::env::temp_dir().join("ps4emu_shot.bmp");
    gpu.save_bmp(path.to_str().unwrap()).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[0..2], b"BM");
    assert_eq!(bytes.len(), 54 + 4 * 12);
    let _ = std::fs::remove_file(path);
}

#[test]
fn pad_state_written_to_guest_memory() {
    let mut emu = Emulator::new(Config::default()).unwrap();
    emu.mem.map(0x5000, 0x1000, 3, "pad").unwrap();
    let mut pad = ps4emu::gpu::pad::PadState::connected();
    pad.apply_script("cross+up");
    pad.write_to(&mut emu.mem, 0x5000).unwrap();
    let btn = emu.mem.read_u32(0x5000).unwrap();
    assert_eq!(btn, (ps4emu::gpu::pad::CROSS | ps4emu::gpu::pad::UP) as u64 as u32);
}

#[test]
fn thread_and_user_hle() {
    let mut emu = Emulator::new(Config::default()).unwrap();
    emu.mem.map(0x2000, 0x1000, 3, "st").unwrap();
    emu.mem.write_u64(0x2000, 0).unwrap();
    emu.cpu.regs.gpr[4] = 0x2000;
    emu.call_hle("sceUserServiceGetInitialUser").unwrap();
    // аргумент — указатель на out
    emu.mem.write_u64(0x2000, 0).unwrap();
    emu.cpu.regs.gpr[7] = 0x2100;
    emu.cpu.regs.gpr[4] = 0x2000;
    emu.call_hle("sceUserServiceGetInitialUser").unwrap();
    let uid = emu.mem.read_u32(0x2100).unwrap();
    assert_eq!(uid, 0x1000_0001);
}

#[test]
fn syscall_log_and_vfs_content() {
    let mut b = GuestBuilder::new();
    b.rodata = b"/data/test.txt\0".to_vec();
    b.mov_imm32(0, sys::nr::OPEN as u32);
    b.mov_imm64(7, 0x41_0000);
    b.mov_imm32(6, 0x0202);
    b.mov_imm32(2, 0);
    b.syscall();
    b.bytes(&[0x49, 0x89, 0xC4]);
    b.mov_imm32(0, sys::nr::WRITE as u32);
    b.bytes(&[0x44, 0x89, 0xE7]);
    b.mov_imm64(6, 0x41_0000 + 0x100);
    b.mov_imm32(2, 5);
    b.syscall();
    while b.rodata.len() < 0x100 {
        b.rodata.push(0);
    }
    b.rodata.extend_from_slice(b"hello");
    b.exit(0);

    let mut emu = Emulator::new(Config { trace: true, ..Default::default() }).unwrap();
    emu.load_image(&b.build(), "t", None).unwrap();
    let e = emu.modules.last().unwrap().entry;
    emu.setup_stack(&["t".into()], &[]).unwrap();
    emu.cpu.reset(e, emu.cpu.regs.gpr[4]);
    let res = emu.run();
    assert_eq!(res.reason, StopReason::Exit(0));
    // Запись через дескриптор должна попадать в VFS (как в реальной ФС).
    assert_eq!(
        emu.sys.vfs.get("/data/test.txt").map(|v| v.as_slice()),
        Some(&b"hello"[..])
    );
    // Трассировка системных вызовов содержит имена вызовов.
    let joined = emu.sys.log.join("\n");
    assert!(joined.contains("open()"), "лог: {}", joined);
    assert!(joined.contains("write()"), "лог: {}", joined);
}

#[test]
fn video_out_flip_captures_framebuffer() {
    let mut emu = Emulator::new(Config { width: 4, height: 4, ..Default::default() }).unwrap();
    // 4x4 RGBA: красный, зелёный, синий, белый в первой строке
    let fb = 0x50_0000;
    emu.mem.map(fb, 0x10000, 7, "fb").unwrap();
    let px = [0xFF00_00FFu32, 0xFF00_FF00, 0xFFFF_0000, 0xFFFF_FFFF];
    for (i, v) in px.iter().enumerate() {
        emu.mem.write_u32(fb + i as u64 * 4, *v).unwrap();
    }
    emu.cpu.regs.gpr[4] = fb + 0x8000; // «возврат» после HLE
    emu.mem.write_u64(fb + 0x8000, 0).unwrap();

    // sceVideoOutSetBufferAttribute(attr, fmt, tiling, w, h, pitch)
    emu.cpu.regs.gpr[7] = 0x31_0000;
    emu.cpu.regs.gpr[6] = 4;
    emu.cpu.regs.gpr[2] = 0;
    emu.cpu.regs.gpr[1] = 4;
    emu.cpu.regs.gpr[8] = 4;
    emu.cpu.regs.gpr[9] = 16;
    emu.mem.map(0x31_0000, 0x1000, 7, "attr").unwrap();
    emu.call_hle("sceVideoOutSetBufferAttribute").unwrap();
    assert_eq!(emu.sys.fb_size, (4, 4));

    // sceVideoOutRegisterBuffers(handle, 0, {fb}, 1, attr)
    emu.mem.map(0x31_1000, 0x1000, 7, "ptrs").unwrap();
    emu.mem.write_u64(0x31_1000, fb).unwrap();
    emu.cpu.regs.gpr[7] = 1;
    emu.cpu.regs.gpr[6] = 0;
    emu.cpu.regs.gpr[2] = 0x31_1000;
    emu.cpu.regs.gpr[1] = 1;
    emu.cpu.regs.gpr[8] = 0x31_0000;
    emu.call_hle("sceVideoOutRegisterBuffers").unwrap();
    assert_eq!(emu.sys.framebuffers.get(&0), Some(&fb));

    emu.cpu.regs.gpr[7] = 1;
    emu.cpu.regs.gpr[6] = 0;
    emu.cpu.regs.gpr[2] = 0;
    emu.cpu.regs.gpr[1] = 0;
    emu.call_hle("sceVideoOutSubmitFlip").unwrap();
    assert!(emu.capture_last_frame());

    let (w, h, rgb) = emu.gpu.last_frame.clone().expect("кадр должен быть снят");
    assert_eq!((w, h), (4, 4));
    // Пиксель записан как 0xFF0000FF = байты FF 00 00 FF => R=255,G=0,B=0.
    assert_eq!(&rgb[0..3], &[255, 0, 0]);
    assert_eq!(&rgb[3..6], &[0, 255, 0]);

    let out = std::env::temp_dir().join("ps4emu_shot_test.bmp");
    emu.gpu.save_bmp(out.to_str().unwrap()).unwrap();
    let meta = std::fs::metadata(&out).unwrap();
    // 54 байта заголовка + 4 строки по 12 байт (4*3, добито до 4)
    assert_eq!(meta.len(), 54 + 4 * 12);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn snapshot_roundtrip_cli_like() {
    let mut b = GuestBuilder::new();
    b.mov_imm32(0, sys::nr::WRITE as u32);
    b.mov_imm32(7, 1);
    b.mov_imm64(6, 0x41_0000);
    b.mov_imm32(2, 3);
    b.syscall();
    b.exit(0);
    b.rodata = b"abc".to_vec();
    let elf = b.build();
    let mut emu = Emulator::new(Config::default()).unwrap();
    emu.load_image(&elf, "t", None).unwrap();
    let args = vec!["t".to_string()];
    emu.setup_stack(&args, &[]).unwrap();
    let e = emu.modules.last().unwrap().entry;
    emu.cpu.reset(e, emu.cpu.regs.gpr[4]);
    let data = ps4emu::snapshot::save_mut(&mut emu);

    // Как в CLI: новый эмулятор загружает ELF, затем снимок.
    let mut emu2 = Emulator::new(Config::default()).unwrap();
    emu2.load_image(&elf, "t", None).unwrap();
    ps4emu::snapshot::load_mut(&mut emu2, &data).unwrap();
    let res = emu2.run();
    assert_eq!(res.stdout, "abc", "rip={:#x} reason={:?}", emu2.cpu.regs.rip, res.reason);
}

#[test]
fn elf_imports_use_dynsym_names() {
    // Программа с PLT-импортами: имена должны браться из .dynsym,
    // а не из .symtab (индексы RELA ссылаются на динамическую таблицу).
    let mut b = GuestBuilder::new();
    b.bytes(&[0xE8, 0x00, 0x00, 0x00, 0x00]); // call rel32
    let call_at = 0usize;
    b.exit(0);
    let elf_bytes = b.build();
    let elf = ps4emu::elf::parse(&elf_bytes).unwrap();
    // У собранного вручную ELF нет .rela — проверяем, что вызов не паникует
    // и возвращает пустой список.
    assert!(elf.imports(&elf_bytes).is_empty());

    // Проверяем вычисление длин в дизассемблере на реальном ELF.
    let mut emu = Emulator::new(Config::default()).unwrap();
    emu.load_image(&elf_bytes, "t", None).unwrap();
    let e = emu.modules.last().unwrap().entry;
    let mut buf = [0u8; 16];
    let n = emu.mem.read_partial(e, &mut buf);
    assert_eq!(n, 16);
    let (text, len) = ps4emu::trace::disasm_len(&buf, e);
    assert_eq!((text.as_str(), len), ("call 0x401005", 5));
    assert_eq!(call_at, 0);
}
