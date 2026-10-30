// W5b C++ exception test (/EHsc, /MD; no C++ library DLL needed).
// Output is compared with the same EXE on Windows (expected/w5cpp*.out).
#include <stdio.h>
#include <exception>
#include <new>

static int live;  // objects alive (constructed - destroyed)

struct Noisy {
    const char *name;
    explicit Noisy(const char *n) : name(n) { live++; printf("  +%s\n", name); }
    Noisy(const Noisy &o) : name(o.name) { live++; printf("  +%s (copy)\n", name); }
    ~Noisy() { live--; printf("  -%s\n", name); }
};

struct Base {
    int code;
    explicit Base(int c) : code(c) { live++; }
    Base(const Base &o) : code(o.code) { live++; printf("  Base copied\n"); }
    virtual ~Base() { live--; }
    virtual const char *what() const { return "Base"; }
};

struct Derived : Base {
    explicit Derived(int c) : Base(c) {}
    Derived(const Derived &o) : Base(o) {}
    const char *what() const override { return "Derived"; }
};

struct Other {};

struct Member {
    Noisy a{"member a"};
    Noisy b;
    Member() : b((throw 42, "never")) {}
};

__declspec(noinline) void thrower(int kind) {
    Noisy n("thrower local");
    switch (kind) {
    case 0: throw 7;
    case 1: throw Derived(5);
    case 2: throw "a string";
    case 3: throw 2.5;
    case 4: throw Other();
    case 5: throw std::exception("std exception");
    }
}

__declspec(noinline) void middle(int kind) {
    Noisy m("middle local");
    thrower(kind);
    printf("not reached\n");
}

static void test(int kind) {
    try {
        middle(kind);
    } catch (int v) {
        printf("  caught int %d\n", v);
    } catch (const Base &b) {
        printf("  caught Base& -> %s code %d\n", b.what(), b.code);
    } catch (const char *s) {
        printf("  caught const char* \"%s\"\n", s);
    } catch (double d) {
        printf("  caught double %g\n", d);
    } catch (const std::exception &e) {
        printf("  caught std::exception \"%s\"\n", e.what());
    } catch (...) {
        printf("  caught ...\n");
    }
}

__declspec(noinline) int rethrow_level(int depth) {
    try {
        if (depth == 0) throw Derived(depth + 100);
        return rethrow_level(depth - 1);
    } catch (Base &b) {
        printf("  level %d caught %d, rethrowing\n", depth, b.code);
        b.code++;
        throw;
    }
}

int main() {
    printf("== types ==\n");
    for (int k = 0; k <= 5; k++) {
        printf("kind %d\n", k);
        test(k);
    }

    printf("== by value ==\n");
    try {
        throw Derived(9);
    } catch (Base b) {
        printf("  caught Base by value: %s %d\n", b.what(), b.code);
    }

    printf("== rethrow ==\n");
    try {
        rethrow_level(2);
    } catch (const Base &b) {
        printf("  main caught %d\n", b.code);
    }

    printf("== throw from catch ==\n");
    try {
        Noisy outer("outer");
        try {
            Noisy inner("inner");
            throw 1;
        } catch (int v) {
            Noisy in_catch("in catch");
            printf("  inner catch %d, throwing 2\n", v);
            throw v + 1;
        }
    } catch (int v) {
        printf("  outer catch %d\n", v);
    }

    printf("== try inside catch ==\n");
    try {
        throw 10;
    } catch (int a) {
        try {
            throw a * 2;
        } catch (int b) {
            printf("  nested catch %d inside catch %d\n", b, a);
        }
        printf("  back in outer catch %d\n", a);
    }

    printf("== partial construction ==\n");
    try {
        Member m;
    } catch (int v) {
        printf("  constructor threw %d\n", v);
    }

    printf("== loop ==\n");
    int n = 0;
    for (int i = 0; i < 100; i++) {
        try {
            if (i % 2) throw i;
            throw Derived(i);
        } catch (int) {
            n++;
        } catch (Base &) {
            n += 2;
        }
    }
    printf("  total %d\n", n);

    printf("== bad_alloc ==\n");
    try {
        throw std::bad_alloc();
    } catch (const std::exception &e) {
        printf("  caught bad_alloc: %s\n", e.what());
    }

    printf("== uncaught_exceptions ==\n");
    printf("  now %d\n", std::uncaught_exceptions());

    printf("live objects: %d\n", live);
    return 0;
}
