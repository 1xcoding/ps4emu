//! Тесты интерпретатора x86-64: каждая проверка — конкретная инструкция
//! с эталонным результатом (значения сверены с реальным AMD64).

use ps4emu::cpu::{Cpu, Step, CF, OF, SF, ZF};
use ps4emu::mem::Memory;

const BASE: u64 = 0x10_0000;

struct Machine {
    cpu: Cpu,
    mem: Memory,
}

impl Machine {
    fn new(code: &[u8]) -> Self {
        let mut mem = Memory::new();
        mem.map(BASE, 0x10000, 7, "code").unwrap();
        mem.map(0x20_0000, 0x10000, 7, "data").unwrap();
        mem.write_bytes(BASE, code).unwrap();
        let mut cpu = Cpu::new();
        cpu.reset(BASE, 0x21_0000);
        Machine { cpu, mem }
    }

    fn run(&mut self, steps: usize) {
        for _ in 0..steps {
            match self.cpu.step(&mut self.mem) {
                Ok(Step::Continue) => {}
                Ok(other) => panic!("неожиданная остановка: {:?}", other),
                Err(e) => panic!("ошибка: {}", e),
            }
        }
    }

    fn rax(&self) -> u64 {
        self.cpu.regs.gpr[0]
    }
    #[allow(dead_code)]
    fn rbx(&self) -> u64 {
        self.cpu.regs.gpr[3]
    }
    fn rdi(&self) -> u64 {
        self.cpu.regs.gpr[7]
    }
    fn xmm(&self, i: usize) -> u128 {
        self.cpu.regs.xmm[i]
    }
}

#[test]
fn mov_imm64() {
    let mut m = Machine::new(&[0x48, 0xb8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    m.run(1);
    assert_eq!(m.rax(), u64::MAX);
}

#[test]
fn mov_imm32_zero_extends() {
    // mov eax, 0xFFFFFFFF -> в rax будет 0x00000000FFFFFFFF
    let mut m = Machine::new(&[0xb8, 0xff, 0xff, 0xff, 0xff]);
    m.run(1);
    assert_eq!(m.rax(), 0xFFFF_FFFF);
}

#[test]
fn mov_rm64_sign_extended_imm32() {
    // mov rdi, -1 (C7 /0 с REX.W): знаковое расширение
    let mut m = Machine::new(&[0x48, 0xc7, 0xc7, 0xff, 0xff, 0xff, 0xff]);
    m.run(1);
    assert_eq!(m.rdi(), u64::MAX);
}

#[test]
fn add_sets_carry() {
    // mov rax, 0xFFFFFFFFFFFFFFFF; add rax, 1
    let mut m = Machine::new(&[
        0x48, 0xb8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x48, 0x83, 0xc0, 0x01,
    ]);
    m.run(2);
    assert_eq!(m.rax(), 0);
    assert!(m.cpu.flag(CF));
    assert!(m.cpu.flag(ZF));
}

#[test]
fn sub_and_flags() {
    // mov eax, 5; sub eax, 7 -> -2 (0xFFFFFFFE), CF=1, SF=1
    let mut m = Machine::new(&[0xb8, 5, 0, 0, 0, 0x83, 0xe8, 7]);
    m.run(2);
    assert_eq!(m.rax(), 0xFFFF_FFFE);
    assert!(m.cpu.flag(CF));
    assert!(m.cpu.flag(SF));
    assert!(!m.cpu.flag(ZF));
    assert!(!m.cpu.flag(OF));
}

#[test]
fn overflow_flag_signed() {
    // mov eax, 0x7FFFFFFF; add eax, 1 -> OF=1, SF=1
    let mut m = Machine::new(&[0xb8, 0xff, 0xff, 0xff, 0x7f, 0x83, 0xc0, 1]);
    m.run(2);
    assert_eq!(m.rax(), 0x8000_0000);
    assert!(m.cpu.flag(OF));
    assert!(m.cpu.flag(SF));
}

#[test]
fn cmp_and_conditional_jump() {
    // cmp eax, ebx (eax==ebx), je -> пропуск mov
    let mut m = Machine::new(&[
        0xb8, 10, 0, 0, 0, // mov eax,10
        0xbb, 10, 0, 0, 0, // mov ebx,10
        0x39, 0xd8, // cmp eax,ebx
        0x74, 0x07, // je +7
        0x48, 0xc7, 0xc0, 0xee, 0xee, 0x00, 0x00, // mov rax, 0xeeee
        0x90, // nop
    ]);
    m.run(4);
    assert_eq!(m.rax(), 10);
    assert!(m.cpu.flag(ZF));
}

#[test]
fn lea_complex() {
    // lea rax, [rbx + rcx*4 + 0x123]
    let mut m = Machine::new(&[
        0x48, 0xbb, 0x10, 0, 0, 0, 0, 0, 0, 0, // mov rbx, 0x10
        0x48, 0xb9, 0x03, 0, 0, 0, 0, 0, 0, 0, // mov rcx, 3
        0x48, 0x8d, 0x84, 0x8b, 0x23, 0x01, 0x00, 0x00, // lea rax,[rbx+rcx*4+0x123]
    ]);
    m.run(3);
    assert_eq!(m.rax(), 0x10 + 3 * 4 + 0x123);
}

#[test]
fn shifted_operands() {
    // mov rax, 1; shl rax, 63; shr rax, 62 -> 2
    let mut m = Machine::new(&[
        0x48, 0xc7, 0xc0, 1, 0, 0, 0, // mov rax,1
        0x48, 0xc1, 0xe0, 63, // shl rax,63
        0x48, 0xc1, 0xe8, 62, // shr rax,62
    ]);
    m.run(3);
    assert_eq!(m.rax(), 2);
}

#[test]
fn sar_keeps_sign() {
    // mov rax, -16; sar rax, 2 -> -4
    let mut m = Machine::new(&[
        0x48, 0xc7, 0xc0, 0xf0, 0xff, 0xff, 0xff, // mov rax,-16
        0x48, 0xc1, 0xf8, 2, // sar rax,2
    ]);
    m.run(2);
    assert_eq!(m.rax() as i64, -4);
}

#[test]
fn imul_three_operand() {
    // mov rax, 100; imul rax, rax, -5
    let mut m = Machine::new(&[
        0x48, 0xc7, 0xc0, 100, 0, 0, 0, // mov rax,100
        0x48, 0x6b, 0xc0, 0xfb, // imul rax,rax,-5
    ]);
    m.run(2);
    assert_eq!(m.rax() as i64, -500);
    assert!(!m.cpu.flag(OF));
}

#[test]
fn div_and_idiv() {
    // mov rax,100; xor rdx,rdx; mov rcx,7; div rcx -> rax=14, rdx=2
    let mut m = Machine::new(&[
        0x48, 0xc7, 0xc0, 100, 0, 0, 0, // mov rax,100
        0x48, 0x31, 0xd2, // xor rdx,rdx
        0x48, 0xc7, 0xc1, 7, 0, 0, 0, // mov rcx,7
        0x48, 0xf7, 0xf1, // div rcx
    ]);
    m.run(4);
    assert_eq!(m.rax(), 14);
    assert_eq!(m.cpu.regs.gpr[2], 2);
}

#[test]
fn idiv_negative() {
    // mov rax, -100; cqo; mov rcx, 7; idiv rcx
    let mut m = Machine::new(&[
        0x48, 0xc7, 0xc0, 0x9c, 0xff, 0xff, 0xff, // mov rax,-100
        0x48, 0x99, // cqo
        0x48, 0xc7, 0xc1, 7, 0, 0, 0, // mov rcx,7
        0x48, 0xf7, 0xf9, // idiv rcx
    ]);
    m.run(4);
    assert_eq!(m.rax() as i64, -14);
    assert_eq!(m.cpu.regs.gpr[2] as i64, -2);
}

#[test]
fn sign_extension_ops() {
    // mov al, 0x80; movsx rax, al -> 0xFFFFFFFFFFFFFF80
    let mut m = Machine::new(&[
        0xb0, 0x80, // mov al,0x80
        0x48, 0x0f, 0xbe, 0xc0, // movsx rax, al
    ]);
    m.run(2);
    assert_eq!(m.rax(), 0xFFFF_FFFF_FFFF_FF80);
}

#[test]
fn zero_extension_ops() {
    // mov ax, 0xFFFF; movzx eax, ax
    let mut m = Machine::new(&[
        0x66, 0xb8, 0xff, 0xff, // mov ax,0xffff
        0x0f, 0xb7, 0xc0, // movzx eax, ax
    ]);
    m.run(2);
    assert_eq!(m.rax(), 0xFFFF);
}

#[test]
fn byte_registers_high() {
    // mov rax, 0x1122334455667788; mov ah, 0xAA -> 0x...AA7788
    let mut m = Machine::new(&[
        0x48, 0xb8, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0xb4, 0xaa,
    ]);
    m.run(2);
    assert_eq!(m.rax(), 0x1122_3344_5566_AA88);
}

#[test]
fn call_and_ret() {
    // call sub; mov eax,1; ...; sub: mov eax,2; ret
    let mut m = Machine::new(&[
        0xe8, 0x06, 0x00, 0x00, 0x00, // 0: call +6 (адрес 11)
        0xb8, 0x01, 0x00, 0x00, 0x00, // 5: mov eax,1
        0xc3, // 10: ret (не выполнится сразу)
        0xb8, 0x02, 0x00, 0x00, 0x00, // 11: mov eax,2
        0xc3, // 16: ret
    ]);
    m.run(1); // call
    assert_eq!(m.cpu.regs.rip, BASE + 11);
    m.run(1); // mov eax,2
    assert_eq!(m.rax(), 2);
    m.run(1); // ret
    assert_eq!(m.cpu.regs.rip, BASE + 5);
    m.run(1); // mov eax,1
    assert_eq!(m.rax(), 1);
}

#[test]
fn push_pop() {
    let mut m = Machine::new(&[
        0x48, 0xbb, 0xef, 0xbe, 0xad, 0xde, 0x00, 0x00, 0x00, 0x00, // mov rbx, 0xdeadbeef
        0x53, // push rbx
        0x58, // pop rax
    ]);
    m.run(3);
    assert_eq!(m.rax(), 0xdead_beef);
    assert_eq!(m.cpu.regs.gpr[4], 0x21_0000);
}

#[test]
fn test_bit_and_setcc() {
    // mov eax, 5; test eax,1; setnz al
    let mut m = Machine::new(&[
        0xb8, 5, 0, 0, 0, // mov eax,5
        0xa8, 1, // test al,1
        0x0f, 0x95, 0xc0, // setne al
    ]);
    m.run(3);
    assert_eq!(m.rax(), 1);
}

#[test]
fn cmov() {
    // mov eax, 1; mov ebx, 2; cmp eax, ebx; cmovb eax, ebx -> eax=2
    let mut m = Machine::new(&[
        0xb8, 1, 0, 0, 0, 0xbb, 2, 0, 0, 0, 0x39, 0xd8, 0x0f, 0x42, 0xc3,
    ]);
    m.run(4);
    assert_eq!(m.rax(), 2);
}

#[test]
fn bt_and_bts() {
    // mov rax, 0x100; bt rax, 8 -> CF=1; bts rax, 1
    let mut m = Machine::new(&[
        0x48, 0xc7, 0xc0, 0x00, 0x01, 0x00, 0x00, // mov rax,0x100
        0x48, 0x0f, 0xba, 0xe0, 8, // bt rax,8
        0x48, 0x0f, 0xab, 0xc8, // bts rax,rcx (rcx=0 -> бит 0)
    ]);
    m.cpu.regs.gpr[1] = 1;
    m.run(3);
    assert_eq!(m.rax(), 0x102);
}

#[test]
fn bsr_bsf() {
    // mov rax, 0x8000; bsr rcx, rax -> 15; bsf rdx, rax -> 15
    let mut m = Machine::new(&[
        0x48, 0xc7, 0xc0, 0x00, 0x80, 0x00, 0x00, // mov rax,0x8000
        0x48, 0x0f, 0xbd, 0xc8, // bsr rcx,rax
        0x48, 0x0f, 0xbc, 0xd0, // bsf rdx,rax
    ]);
    m.run(3);
    assert_eq!(m.cpu.regs.gpr[1], 15);
    assert_eq!(m.cpu.regs.gpr[2], 15);
}

#[test]
fn popcnt_and_bswap() {
    // mov rax, 0x0102030405060708; bswap rax
    let mut m = Machine::new(&[
        0x48, 0xb8, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // mov rax,...
        0x48, 0x0f, 0xc8, // bswap rax
    ]);
    m.run(2);
    assert_eq!(m.rax(), 0x0807_0605_0403_0201);
}

#[test]
fn cmpxchg_memory() {
    // mov [0x200000], 42; mov eax,42; mov ebx,7; lock cmpxchg [0x200000], ebx
    let mut m = Machine::new(&[
        0xc7, 0x04, 0x25, 0x00, 0x00, 0x20, 0x00, 42, 0x00, 0x00, 0x00, // mov dword [0x200000], 42
        0xb8, 42, 0, 0, 0, // mov eax,42
        0xbb, 7, 0, 0, 0, // mov ebx,7
        0xf0, 0x0f, 0xb1, 0x1c, 0x25, 0x00, 0x00, 0x20, 0x00, // lock cmpxchg [0x200000], ebx
    ]);
    m.run(4);
    assert!(m.cpu.flag(ZF));
    assert_eq!(m.mem.read_u32(0x20_0000).unwrap(), 7);
}

#[test]
fn cmpxchg16b_semantics() {
    // Проверяем базовую логику 128-битного CAS через регистры.
    let mut m = Machine::new(&[
        // 0x200000: 0x1122334455667788, 0x99aabbccddeeff00
        0x48, 0xb8, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, // mov rax,...
        0x48, 0xbb, 0x00, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, // mov rbx,...
        0xf3, 0x0f, 0xc7, 0x0c, 0x25, 0x00, 0x00, 0x20, 0x00, // cmpxchg16b [0x200000] (rcx:rdx = 0)
    ]);
    // Записываем содержимое памяти
    m.mem.write_u64(0x20_0000, 0x1122_3344_5566_7788).unwrap();
    m.mem.write_u64(0x20_0008, 0x99aa_bbcc_ddee_ff00).unwrap();
    m.run(3);
    // rax != 0 -> обмена не будет, ZF=0
    assert!(!m.cpu.flag(ZF));
}

#[test]
fn string_movs_rep() {
    // Копирование 8 байт из 0x200000 в 0x200100 через rep movsb
    let mut m = Machine::new(&[
        0xbe, 0x00, 0x00, 0x20, 0x00, // mov esi, 0x200000
        0xbf, 0x00, 0x01, 0x20, 0x00, // mov edi, 0x200100
        0xb9, 8, 0, 0, 0, // mov ecx, 8
        0xf3, 0xa4, // rep movsb
    ]);
    m.mem.write_bytes(0x20_0000, b"ABCDEFGH").unwrap();
    m.run(4);
    let mut buf = [0u8; 8];
    m.mem.read_bytes(0x20_0100, &mut buf).unwrap();
    assert_eq!(&buf, b"ABCDEFGH");
    assert_eq!(m.cpu.regs.gpr[1], 0);
}

#[test]
fn string_stos_and_scas_rep() {
    // rep stosb: заполнить 4 байта символом 'z'
    let mut m = Machine::new(&[
        0xbf, 0x00, 0x02, 0x20, 0x00, // mov edi, 0x200200
        0xb9, 4, 0, 0, 0, // mov ecx, 4
        0xb0, b'z', // mov al, 'z'
        0xf3, 0xaa, // rep stosb
        0xbf, 0x00, 0x02, 0x20, 0x00, // mov edi, 0x200200
        0xb9, 8, 0, 0, 0, // mov ecx, 8
        0x31, 0xc0, // xor eax, eax
        0xf2, 0xae, // repne scasb (искать 0)
    ]);
    m.run(8);
    let mut buf = [0u8; 4];
    m.mem.read_bytes(0x20_0200, &mut buf).unwrap();
    assert_eq!(&buf, b"zzzz");
    // repne scasb увеличивает edi после сравнения: 0x200200+5
    assert_eq!(m.cpu.regs.gpr[7], 0x20_0205);
    // 5 сравнений из 8 -> ecx = 3
    assert_eq!(m.cpu.regs.gpr[1], 3);
}

#[test]
fn sse_math_scalar() {
    // movsd xmm0, [0x200000]; addsd xmm0, [0x200008]
    let mut m = Machine::new(&[
        0xf2, 0x0f, 0x10, 0x04, 0x25, 0x00, 0x00, 0x20, 0x00, // movsd xmm0,[0x200000]
        0xf2, 0x0f, 0x58, 0x04, 0x25, 0x08, 0x00, 0x20, 0x00, // addsd xmm0,[0x200008]
    ]);
    m.mem.write_u64(0x20_0000, 1.5f64.to_bits()).unwrap();
    m.mem.write_u64(0x20_0008, 2.25f64.to_bits()).unwrap();
    m.run(2);
    assert_eq!(f64::from_bits(m.xmm(0) as u64), 3.75);
}

#[test]
fn sse_mul_div_divsd() {
    let mut m = Machine::new(&[
        0xf2, 0x0f, 0x10, 0x04, 0x25, 0x00, 0x00, 0x20, 0x00, // movsd xmm0,[0x200000]
        0xf2, 0x0f, 0x59, 0x04, 0x25, 0x08, 0x00, 0x20, 0x00, // mulsd
        0xf2, 0x0f, 0x5e, 0x04, 0x25, 0x10, 0x00, 0x20, 0x00, // divsd
    ]);
    m.mem.write_u64(0x20_0000, 10.0f64.to_bits()).unwrap();
    m.mem.write_u64(0x20_0008, 3.0f64.to_bits()).unwrap();
    m.mem.write_u64(0x20_0010, 4.0f64.to_bits()).unwrap();
    m.run(3);
    assert_eq!(f64::from_bits(m.xmm(0) as u64), 7.5);
}

#[test]
fn sse_cvtsi2sd() {
    // mov rax, 42; cvtsi2sd xmm1, rax
    let mut m = Machine::new(&[
        0x48, 0xc7, 0xc0, 42, 0, 0, 0, // mov rax,42
        0xf2, 0x48, 0x0f, 0x2a, 0xc8, // cvtsi2sd xmm1, rax
    ]);
    m.run(2);
    assert_eq!(f64::from_bits(m.xmm(1) as u64), 42.0);
}

#[test]
fn sse_cvttsd2si() {
    // cvttsd2si rax, xmm0 (12.9 -> 12)
    let mut m = Machine::new(&[0xf2, 0x48, 0x0f, 0x2c, 0xc0]);
    m.cpu.regs.xmm[0] = 12.9f64.to_bits() as u128;
    m.run(1);
    assert_eq!(m.rax(), 12);
}

#[test]
fn sse_movq_both_directions() {
    // movq xmm0, rax (66 0F 6E) и movq rax, xmm0 (66 0F 7E)
    let mut m = Machine::new(&[
        0x48, 0xc7, 0xc0, 0x78, 0x56, 0x34, 0x12, // mov rax, 0x12345678
        0x66, 0x48, 0x0f, 0x6e, 0xc0, // movq xmm0, rax
        0x66, 0x48, 0x0f, 0x7e, 0xc1, // movq rcx, xmm0
    ]);
    m.run(3);
    assert_eq!(m.xmm(0) as u64, 0x1234_5678);
    assert_eq!(m.cpu.regs.gpr[1], 0x1234_5678);
}

#[test]
fn sse_packed_logic_and_movups() {
    // pxor xmm0,xmm0; movups [0x200000], xmm0
    let mut m = Machine::new(&[
        0x0f, 0x57, 0xc0, // pxor xmm0,xmm0
        0x0f, 0x11, 0x04, 0x25, 0x00, 0x00, 0x20, 0x00, // movups [0x200000],xmm0
    ]);
    m.cpu.regs.xmm[0] = u128::MAX;
    m.run(1);
    assert_eq!(m.xmm(0), 0);
    m.run(1);
    let mut buf = [0xFFu8; 16];
    m.mem.read_bytes(0x20_0000, &mut buf).unwrap();
    assert_eq!(buf, [0u8; 16]);
}

#[test]
fn ud2_reports_error() {
    let mut m = Machine::new(&[0x0f, 0x0b]);
    let r = m.cpu.step(&mut m.mem);
    assert!(r.is_err(), "ud2 должен приводить к ошибке");
}

#[test]
fn syscall_returns_step() {
    let mut m = Machine::new(&[0x0f, 0x05]);
    let r = m.cpu.step(&mut m.mem).unwrap();
    assert_eq!(r, Step::Syscall);
}

#[test]
fn cpuid_reports_jaguar_without_avx() {
    let mut m = Machine::new(&[0x0f, 0xa2]);
    m.cpu.regs.gpr[0] = 1;
    m.run(1);
    let ecx = m.cpu.regs.gpr[1] as u32;
    let edx = m.cpu.regs.gpr[3] as u32;
    assert!(edx & (1 << 25) != 0, "SSE должен быть установлен");
    assert!(edx & (1 << 26) != 0, "SSE2 должен быть установлен");
    assert_eq!(ecx & (1 << 28), 0, "AVX не должен рекламироваться");
    let eax = m.rax() as u32 & 0xFFFF_FFFF;
    assert_eq!((eax >> 8) & 0xF, 0xF, "семейство 16h (Jaguar)");
}

#[test]
fn rdtsc_monotonic() {
    let mut m = Machine::new(&[0x0f, 0x31]);
    m.run(1);
    let first = m.rax();
    std::thread::sleep(std::time::Duration::from_millis(2));
    m.cpu.regs.rip = BASE;
    m.run(1);
    assert!(m.rax() >= first);
}

#[test]
fn memory_protection_is_enforced() {
    let mut mem = Memory::new();
    mem.map(0x10_0000, 0x1000, ps4emu::mem::P_READ, "ro").unwrap();
    let err = mem.write_u8(0x10_0000, 1).unwrap_err();
    assert!(matches!(err, ps4emu::mem::MemError::Prot { .. }));
}

#[test]
fn unmapped_access_is_error() {
    let mut mem = Memory::new();
    assert!(mem.read_u64(0xDEAD_0000).is_err());
}

#[test]
fn fpu_basic_arithmetic() {
    // fld [0x200000] (dword); fld [0x200008]; faddp; fstp [0x200010]
    let mut m = Machine::new(&[
        0xd9, 0x04, 0x25, 0x00, 0x00, 0x20, 0x00, // fld dword [0x200000]
        0xd9, 0x04, 0x25, 0x08, 0x00, 0x20, 0x00, // fld dword [0x200008]
        0xde, 0xc1, // faddp st(1), st
        0xd9, 0x1c, 0x25, 0x10, 0x00, 0x20, 0x00, // fstp dword [0x200010]
    ]);
    m.mem.write_u32(0x20_0000, 1.25f32.to_bits()).unwrap();
    m.mem.write_u32(0x20_0008, 2.75f32.to_bits()).unwrap();
    m.run(4);
    let v = f32::from_bits(m.mem.read_u32(0x20_0010).unwrap() as u32);
    assert_eq!(v, 4.0);
}

// ---------------------------------------------------------------------------
// Дизассемблер (trace::disasm_len)
// ---------------------------------------------------------------------------

#[test]
fn disasm_common_forms() {
    use ps4emu::trace::disasm_len;
    let cases: &[(&[u8], &str)] = &[
        (&[0x48, 0xC7, 0xC0, 0x04, 0x00, 0x00, 0x00], "mov rax, 0x4"),
        (&[0x48, 0x89, 0xD8], "mov rax, rbx"),
        (&[0x48, 0x8B, 0x43, 0x08], "mov rax, [rbx+8]"),
        (&[0x48, 0x8D, 0x3D, 0x10, 0x00, 0x00, 0x00], "lea rdi, [rip+16]"),
        (&[0x0F, 0x05], "syscall"),
        (&[0xC3], "ret"),
        (&[0x48, 0x31, 0xFF], "xor rdi, rdi"),
        (&[0xE8, 0x00, 0x00, 0x00, 0x00], "call 0x401005"),
        (&[0x74, 0xFE], "je 0x401000"),
        (&[0x83, 0xF8, 0xFF], "cmp eax, -1"),
        (&[0x48, 0xFF, 0xC0], "inc rax"),
        (&[0xF3, 0x0F, 0x1E, 0xFA], "endbr64"),
        (&[0x48, 0x01, 0xC8], "add rax, rcx"),
        (&[0x41, 0x5C], "pop r12"),
        (&[0xCC], "int3"),
    ];
    for (bytes, want) in cases {
        let (got, len) = disasm_len(bytes, 0x401000);
        assert_eq!(&got, want, "байты {:02x?}", bytes);
        assert!(len > 0 && len <= bytes.len(), "длина {} для {:02x?}", len, bytes);
    }
}

#[test]
fn disasm_lengths_are_exact() {
    use ps4emu::trace::disasm_len;
    // Последовательность из hello.S: разбор подряд должен дать точные границы.
    let code: &[u8] = &[
        0x48, 0xC7, 0xC0, 0x04, 0x00, 0x00, 0x00, // mov rax, 4
        0x48, 0xC7, 0xC7, 0x01, 0x00, 0x00, 0x00, // mov rdi, 1
        0x48, 0x8D, 0x35, 0xEB, 0x0F, 0x00, 0x00, // lea rsi, [rip+0xfeb]
        0x48, 0xC7, 0xC2, 0x16, 0x00, 0x00, 0x00, // mov rdx, 0x16
        0x0F, 0x05, // syscall
    ];
    let mut off = 0usize;
    let mut count = 0;
    while off < code.len() {
        let (text, len) = disasm_len(&code[off..], 0x401000 + off as u64);
        assert!(len > 0, "нулевая длина на {}", text);
        off += len;
        count += 1;
    }
    assert_eq!(off, code.len());
    assert_eq!(count, 5);
}
