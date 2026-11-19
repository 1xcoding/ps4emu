/* Гостевая программа: «рисует» кадр 64x64 в свой буфер и представляет его
   через sceVideoOutSubmitFlip, после чего завершается. Собирается как
   разделяемый объект, чтобы импорты HLE шли через PLT (как в eboot.bin):
       gcc -shared -nostdlib -fPIC -o video.so video.S
   Используется для проверки команды `ps4emu screenshot`. */
.globl _start
_start:
    /* Заполняем буфер 64x64 RGBA градиентом (по диагонали). */
    lea  fb(%rip), %rdi
    xor  %ecx, %ecx              /* y */
1:  xor  %edx, %edx              /* x */
2:  lea  (%rcx,%rcx,2), %rax
    add  %edx, %eax
    add  %eax, %eax              /* яркость = 6*y + 2*x */
    cmp  $255, %eax
    jbe  3f
    mov  $255, %eax
3:  mov  %dl, %r8b
    mov  %al, %r9b
    mov  %r8b, 2(%rdi)           /* B */
    mov  %r9b, 1(%rdi)           /* G */
    mov  %r9b, (%rdi)            /* R */
    movb $255, 3(%rdi)           /* A */
    add  $4, %rdi
    inc  %edx
    cmp  $64, %edx
    jb   2b
    inc  %ecx
    cmp  $64, %ecx
    jb   1b

    /* sceVideoOutSetBufferAttribute(&attr, 4, 0, 64, 64, 256) */
    lea  attr(%rip), %rdi
    mov  $4, %esi
    xor  %edx, %edx
    mov  $64, %ecx
    mov  $64, %r8d
    mov  $256, %r9d
    call sceVideoOutSetBufferAttribute@PLT

    /* sceVideoOutRegisterBuffers(1, 0, ptrs, 1, &attr) */
    mov  $1, %edi
    xor  %esi, %esi
    lea  ptrs(%rip), %rdx
    mov  $1, %ecx
    lea  attr(%rip), %r8
    call sceVideoOutRegisterBuffers@PLT

    /* sceVideoOutSubmitFlip(1, 0, 0, 0) */
    mov  $1, %edi
    xor  %esi, %esi
    xor  %edx, %edx
    xor  %ecx, %ecx
    call sceVideoOutSubmitFlip@PLT

    mov  $1, %rax                /* SYS_exit */
    xor  %rdi, %rdi
    syscall

.section .bss
.p2align 6
fb:   .zero 64*64*4
attr: .zero 32
.section .data
.p2align 3
ptrs: .quad fb
