/* Минимальная гостевая программа: завершиться с кодом 0 (FreeBSD ABI). */
.globl _start
_start:
    mov $1, %rax        /* SYS_exit */
    xor %rdi, %rdi
    syscall
