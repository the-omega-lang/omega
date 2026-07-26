.global exit
exit:
        mov $60, %rax
        syscall

strlen:
	xor %rax, %rax            # len = 0
	mov %rdi, %rcx            # rcx = ptr
.L_loop2:
	movzbq (%rcx), %rdx      # load byte -> rdx (zero-extend)
	test %dl, %dl            # test low byte
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

        mov $10, %rax /* new line */
        push %rax

        mov $1, %rax
        mov $1, %rdi
        mov %rsp, %rsi
        mov $1, %rdx
        syscall

        add $8, %rsp

        ret

/* TODO: Handle void functions */
.global _start
_start:
        call main
        mov %rax, %rdi
        call exit

/*
main:
        mov $123, %rax
        ret
*/
