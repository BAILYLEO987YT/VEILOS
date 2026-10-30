/* W5b structured exception handling test (C, clang-cl /MD).
 * Output is compared with the same EXE on Windows (expected/w5seh.out). */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <malloc.h>
#include <windows.h>
#include <intrin.h>

static int filter_count;

static int show(const char *where, EXCEPTION_POINTERS *p, int verdict) {
    EXCEPTION_RECORD *r = p->ExceptionRecord;
    filter_count++;
    printf("  filter %s: code=%08lX params=%lu", where, r->ExceptionCode, r->NumberParameters);
    if (r->ExceptionCode == EXCEPTION_ACCESS_VIOLATION)
        printf(" [%llu, %llX]", (unsigned long long)r->ExceptionInformation[0], (unsigned long long)r->ExceptionInformation[1]);
    if (r->ExceptionCode == 0xE0000001)
        printf(" [%llu, %llu]", (unsigned long long)r->ExceptionInformation[0], (unsigned long long)r->ExceptionInformation[1]);
    printf(" -> %d\n", verdict);
    return verdict;
}

static volatile int zero = 0;
static int *volatile bad_ptr = (int *)0x10;

static __declspec(noinline) int deep(int n) {
    volatile char pad[64];
    pad[n % 64] = (char)n;
    if (n == 0) return *bad_ptr;
    return deep(n - 1) + pad[n % 64];
}

static __declspec(noinline) int with_frame_pointer(int n) {
    /* _alloca forces RBP-based frames, unwound through the frame register. */
    char *p = (char *)_alloca(n + 16);
    memset(p, 1, n);
    return p[0] + deep(3);
}

static __declspec(noinline) int finally_return(void) {
    __try {
        printf("  in __try, returning 7\n");
        return 7;
    } __finally {
        printf("  __finally ran (abnormal=%d)\n", AbnormalTermination());
    }
    return 0;
}

static int vectored_calls;
static LONG CALLBACK vectored(EXCEPTION_POINTERS *p) {
    vectored_calls++;
    printf("  vectored handler saw %08lX\n", p->ExceptionRecord->ExceptionCode);
    return EXCEPTION_CONTINUE_SEARCH;
}

static LONG WINAPI top_level(EXCEPTION_POINTERS *p) {
    printf("top-level filter: %08lX, exiting with it\n", p->ExceptionRecord->ExceptionCode);
    fflush(stdout);
    return EXCEPTION_EXECUTE_HANDLER;
}

int main(void) {
    printf("== access violation ==\n");
    __try {
        *bad_ptr = 1;
        printf("not reached\n");
    } __except (show("write", GetExceptionInformation(), EXCEPTION_EXECUTE_HANDLER)) {
        printf("  caught %08lX\n", GetExceptionCode());
    }
    __try {
        printf("%d\n", *(int *)0x20);
    } __except (show("read", GetExceptionInformation(), EXCEPTION_EXECUTE_HANDLER)) {
        printf("  caught read fault\n");
    }

    printf("== divide by zero ==\n");
    __try {
        printf("%d\n", 10 / zero);
    } __except (GetExceptionCode() == EXCEPTION_INT_DIVIDE_BY_ZERO ? EXCEPTION_EXECUTE_HANDLER : EXCEPTION_CONTINUE_SEARCH) {
        printf("  caught divide by zero\n");
    }

    printf("== RaiseException ==\n");
    ULONG_PTR args[2] = {11, 22};
    __try {
        RaiseException(0xE0000001, 0, 2, args);
        printf("  continued after RaiseException\n");
    } __except (show("continue", GetExceptionInformation(), EXCEPTION_CONTINUE_EXECUTION)) {
        printf("not reached\n");
    }
    __try {
        RaiseException(0xE0000002, EXCEPTION_NONCONTINUABLE, 0, NULL);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        printf("  caught %08lX\n", GetExceptionCode());
    }

    printf("== __finally ==\n");
    __try {
        __try {
            printf("  inner body\n");
        } __finally {
            printf("  finally 1 (abnormal=%d)\n", AbnormalTermination());
        }
        __try {
            __try {
                RaiseException(0xE0000003, 0, 0, NULL);
            } __finally {
                printf("  finally 2 (abnormal=%d)\n", AbnormalTermination());
            }
        } __finally {
            printf("  finally 3 (abnormal=%d)\n", AbnormalTermination());
        }
    } __except (printf("  outer filter runs before the finally blocks\n"), EXCEPTION_EXECUTE_HANDLER) {
        printf("  outer handler after them\n");
    }
    printf("  finally_return() = %d\n", finally_return());

    printf("== nested search ==\n");
    __try {
        __try {
            RaiseException(0xE0000004, 0, 0, NULL);
        } __except (printf("  inner filter: continue search\n"), EXCEPTION_CONTINUE_SEARCH) {
            printf("not reached\n");
        }
    } __except (printf("  outer filter: execute\n"), EXCEPTION_EXECUTE_HANDLER) {
        printf("  outer caught %08lX\n", GetExceptionCode());
    }

    printf("== deep frames ==\n");
    __try {
        printf("%d\n", deep(10));
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        printf("  caught from 11 frames down\n");
    }
    __try {
        printf("%d\n", with_frame_pointer(100));
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        printf("  caught through an _alloca frame\n");
    }

    printf("== breakpoint and illegal instruction ==\n");
    __try {
        __debugbreak();
    } __except (GetExceptionCode() == EXCEPTION_BREAKPOINT) {
        printf("  caught breakpoint\n");
    }
    __try {
        __ud2();
    } __except (GetExceptionCode() == EXCEPTION_ILLEGAL_INSTRUCTION) {
        printf("  caught illegal instruction\n");
    }

    printf("== vectored ==\n");
    void *h = AddVectoredExceptionHandler(1, vectored);
    __try {
        RaiseException(0xE0000005, 0, 0, NULL);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        printf("  caught after the vectored handler\n");
    }
    RemoveVectoredExceptionHandler(h);
    __try {
        RaiseException(0xE0000006, 0, 0, NULL);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        printf("  caught (vectored handler removed: %d call)\n", vectored_calls);
    }

    printf("== many in a row ==\n");
    int caught = 0;
    for (int i = 0; i < 200; i++) {
        __try {
            *bad_ptr = i;
        } __except (EXCEPTION_EXECUTE_HANDLER) {
            caught++;
        }
    }
    printf("  caught %d of 200, filter calls %d\n", caught, filter_count);

    printf("== unhandled ==\n");
    SetUnhandledExceptionFilter(top_level);
    fflush(stdout);
    *bad_ptr = 2;
    printf("not reached\n");
    return 0;
}
