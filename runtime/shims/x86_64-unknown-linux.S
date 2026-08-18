.global exit
exit:
        mov $60, %rax
        syscall

strlen:
	xor %rax, %rax
	mov %rdi, %rcx
.L_loop2:
	movzbq (%rcx), %rdx
	test %dl, %dl
	je .L_done
	inc %rax
	inc %rcx
	jmp .L_loop2

.L_done:
	ret

.global puts
puts:
        call strlen

        mov %rax, %rdx
        mov %rdi, %rsi
        mov $1, %rax
        mov $1, %rdi
        syscall

        mov $10, %rax /* newline */
        push %rax

        mov $1, %rax
        mov $1, %rdi
        mov %rsp, %rsi
        mov $1, %rdx
        syscall

        add $8, %rsp

        ret

.global _start
_start:
        call main
        mov %rax, %rdi
        call exit
