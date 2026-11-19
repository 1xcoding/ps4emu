/* Гостевая программа для замера скорости: длинный счётный цикл.
   Собирается статически (gcc -static -nostdlib) и запускается
   командой `ps4emu bench loop.elf 5000000`. */
.globl _start
_start:
    mov $10000000, %ecx
    xor %eax, %eax
1:  add $3, %eax
    xor %edx, %edx
    imul $7, %eax, %r8d
    and $0xffff, %r8d
    dec %ecx
    jnz 1b
    mov $1, %rax          /* SYS_exit */
    xor %rdi, %rdi
    syscall
