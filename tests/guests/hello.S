/* Вывод строки через syscall write (Orbis/FreeBSD: write = 4). */
.globl _start
_start:
    mov $4, %rax        /* SYS_write */
    mov $1, %rdi        /* stdout */
    lea msg(%rip), %rsi
    mov $len, %rdx
    syscall
    mov $1, %rax        /* SYS_exit */
    xor %rdi, %rdi
    syscall
.section .rodata
msg: .ascii "Hello from PS4 guest!\n"
.set len, . - msg
