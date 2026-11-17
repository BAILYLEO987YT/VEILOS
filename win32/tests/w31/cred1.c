/* W31 credentials: the Credential Manager (CredWriteW, CredReadW,
 * CredEnumerateW, CredDeleteW) and DPAPI (CryptProtectData,
 * CryptUnprotectData), as programs that keep a login use them.
 *
 *   cred1            the whole test; leaves nothing behind
 *   cred1 save       saves a credential that lasts (for a reboot check)
 *   cred1 load       reads it back and deletes it
 *
 * Every credential it makes is named veilos-test/..., and they are deleted
 * again, so it can run on a real machine. */
#include <windows.h>
#include <wincred.h>
#include <dpapi.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void show(const CREDENTIALW *c)
{
    DWORD i;
    printf("  type %lu persist %lu flags %lu target %ls user %ls comment %ls alias %ls\n",
           c->Type, c->Persist, c->Flags, c->TargetName,
           c->UserName ? c->UserName : L"(null)", c->Comment ? c->Comment : L"(null)",
           c->TargetAlias ? c->TargetAlias : L"(null)");
    printf("  blob %lu bytes \"%.*s\" written %s\n", c->CredentialBlobSize, (int)c->CredentialBlobSize,
           c->CredentialBlob ? (const char *)c->CredentialBlob : "",
           (c->LastWritten.dwHighDateTime | c->LastWritten.dwLowDateTime) ? "set" : "zero");
    for (i = 0; i < c->AttributeCount; i++)
        printf("  attribute %ls flags %lu \"%.*s\"\n", c->Attributes[i].Keyword, c->Attributes[i].Flags,
               (int)c->Attributes[i].ValueSize, (const char *)c->Attributes[i].Value);
}

static BOOL write_cred(const wchar_t *target, const char *secret, DWORD persist, DWORD flags)
{
    CREDENTIALW c;
    CREDENTIAL_ATTRIBUTEW a;
    memset(&c, 0, sizeof c);
    c.Type = CRED_TYPE_GENERIC;
    c.TargetName = (LPWSTR)target;
    c.Comment = L"a comment";
    c.UserName = L"alice";
    c.Persist = persist;
    if (secret) {
        c.CredentialBlob = (LPBYTE)secret;
        c.CredentialBlobSize = (DWORD)strlen(secret);
    }
    a.Keyword = L"veilos:kind";
    a.Flags = 0;
    a.Value = (LPBYTE)"token";
    a.ValueSize = 5;
    c.AttributeCount = 1;
    c.Attributes = &a;
    return CredWriteW(&c, flags);
}

static void read_cred(const wchar_t *target, DWORD type)
{
    PCREDENTIALW c;
    if (CredReadW(target, type, 0, &c)) {
        printf("read %ls:\n", target);
        show(c);
        CredFree(c);
    } else {
        printf("read %ls (type %lu): error %lu\n", target, type, GetLastError());
    }
}

/* The call first, then its error (printf's arguments have no set order). */
#define TRY(what, call) do { BOOL r_ = (call); DWORD e_ = GetLastError(); printf("%s: %d %lu\n", what, r_, e_); } while (0)

static int cmp_target(const void *a, const void *b)
{
    return _wcsicmp((*(PCREDENTIALW *)a)->TargetName, (*(PCREDENTIALW *)b)->TargetName);
}

static void enumerate(const wchar_t *filter)
{
    DWORD n, i;
    PCREDENTIALW *list;
    if (!CredEnumerateW(filter, 0, &n, &list)) {
        printf("enumerate %ls: error %lu\n", filter, GetLastError());
        return;
    }
    qsort(list, n, sizeof *list, cmp_target);
    printf("enumerate %ls: %lu\n", filter, n);
    for (i = 0; i < n; i++)
        printf("  %ls persist %lu\n", list[i]->TargetName, list[i]->Persist);
    CredFree(list);
}

static void del(const wchar_t *target)
{
    if (CredDeleteW(target, CRED_TYPE_GENERIC, 0))
        printf("delete %ls: ok\n", target);
    else
        printf("delete %ls: error %lu\n", target, GetLastError());
}

static void protect(void)
{
    DATA_BLOB in, out, back, ent, wrong;
    LPWSTR desc = NULL;
    in.pbData = (BYTE *)"hello, world";
    in.cbData = 12;
    ent.pbData = (BYTE *)"salt";
    ent.cbData = 4;
    wrong.pbData = (BYTE *)"pepper";
    wrong.cbData = 6;
    if (!CryptProtectData(&in, L"what it is", &ent, NULL, NULL, CRYPTPROTECT_UI_FORBIDDEN, &out)) {
        printf("protect: error %lu\n", GetLastError());
        return;
    }
    printf("protect: ok, %s, longer %d\n", memcmp(out.pbData, in.pbData, 12) ? "not plain" : "PLAIN", out.cbData > in.cbData);
    if (CryptUnprotectData(&out, &desc, &ent, NULL, NULL, CRYPTPROTECT_UI_FORBIDDEN, &back)) {
        printf("unprotect: \"%.*s\" description \"%ls\"\n", (int)back.cbData, (const char *)back.pbData, desc);
        LocalFree(back.pbData);
        LocalFree(desc);
    } else {
        printf("unprotect: error %lu\n", GetLastError());
    }
    TRY("unprotect, wrong entropy", CryptUnprotectData(&out, NULL, &wrong, NULL, NULL, 1, &back));
    TRY("unprotect, no entropy", CryptUnprotectData(&out, NULL, NULL, NULL, NULL, 1, &back));
    out.pbData[out.cbData - 1] ^= 1;
    TRY("unprotect, damaged", CryptUnprotectData(&out, NULL, &ent, NULL, NULL, 1, &back));
    LocalFree(out.pbData);

    /* Nothing to protect; no description; CryptProtectData twice differs. */
    in.cbData = 0;
    if (CryptProtectData(&in, NULL, NULL, NULL, NULL, 1, &out)) {
        DATA_BLOB out2;
        CryptProtectData(&in, NULL, NULL, NULL, NULL, 1, &out2);
        printf("protect empty: ok, same twice %d\n", out.cbData == out2.cbData && !memcmp(out.pbData, out2.pbData, out.cbData));
        if (CryptUnprotectData(&out, &desc, NULL, NULL, NULL, 1, &back)) {
            printf("unprotect empty: %lu bytes, description \"%ls\"\n", back.cbData, desc ? desc : L"(null)");
            LocalFree(back.pbData);
            LocalFree(desc);
        } else {
            printf("unprotect empty: error %lu\n", GetLastError());
        }
        LocalFree(out.pbData);
        LocalFree(out2.pbData);
    } else {
        printf("protect empty: error %lu\n", GetLastError());
    }
}

int main(int argc, char **argv)
{
    char big[2600];
    if (argc > 1 && !strcmp(argv[1], "save")) {
        printf("save: %s\n", write_cred(L"veilos-test/lasting", "kept across a reboot", CRED_PERSIST_LOCAL_MACHINE, 0) ? "ok" : "error");
        return 0;
    }
    if (argc > 1 && !strcmp(argv[1], "load")) {
        read_cred(L"veilos-test/lasting", CRED_TYPE_GENERIC);
        del(L"veilos-test/lasting");
        return 0;
    }

    printf("write one: %s\n", write_cred(L"veilos-test/one", "secret1", CRED_PERSIST_LOCAL_MACHINE, 0) ? "ok" : "error");
    read_cred(L"veilos-test/one", CRED_TYPE_GENERIC);
    printf("write one again: %s\n", write_cred(L"veilos-test/one", "second", CRED_PERSIST_LOCAL_MACHINE, 0) ? "ok" : "error");
    read_cred(L"veilos-test/one", CRED_TYPE_GENERIC);
    printf("write one, keeping the blob: %s\n", write_cred(L"veilos-test/one", NULL, CRED_PERSIST_ENTERPRISE, CRED_PRESERVE_CREDENTIAL_BLOB) ? "ok" : "error");
    read_cred(L"VEILOS-TEST/ONE", CRED_TYPE_GENERIC);
    printf("write two: %s\n", write_cred(L"veilos-test/two", "secret2", CRED_PERSIST_SESSION, 0) ? "ok" : "error");
    printf("write other: %s\n", write_cred(L"veilos-testother", "secret3", CRED_PERSIST_SESSION, 0) ? "ok" : "error");
    enumerate(L"veilos-test/*");
    enumerate(L"veilos-test*");
    enumerate(L"VEILOS-TEST/T*");
    enumerate(L"veilos-test/two");
    enumerate(L"veilos-nothing*");

    read_cred(L"veilos-test/none", CRED_TYPE_GENERIC);
    read_cred(L"veilos-test/one", CRED_TYPE_DOMAIN_PASSWORD);
    {
        PCREDENTIALW c;
        /* What a call that works leaves as the last error. */
        SetLastError(5);
        TRY("read with flags", CredReadW(L"veilos-test/one", CRED_TYPE_GENERIC, 1, &c));
        CredFree(c);
    }
    TRY("write persist 0", write_cred(L"veilos-test/bad", "x", 0, 0));
    TRY("write persist 4", write_cred(L"veilos-test/bad", "x", 4, 0));
    memset(big, 'b', sizeof big - 1);
    big[sizeof big - 1] = 0;
    big[2561] = 0;
    TRY("write 2561 bytes", write_cred(L"veilos-test/bad", big, CRED_PERSIST_SESSION, 0));
    big[2560] = 0;
    SetLastError(5);
    TRY("write 2560 bytes", write_cred(L"veilos-test/big", big, CRED_PERSIST_SESSION, 0));
    {
        PCREDENTIALW c;
        if (CredReadW(L"veilos-test/big", CRED_TYPE_GENERIC, 0, &c)) {
            printf("read big: %lu bytes\n", c->CredentialBlobSize);
            CredFree(c);
        }
    }
    TRY("write NULL", CredWriteW(NULL, 0));
    TRY("read NULL target", CredReadW(NULL, CRED_TYPE_GENERIC, 0, NULL));
    SetLastError(5);
    TRY("delete with flags", CredDeleteW(L"veilos-test/big", CRED_TYPE_GENERIC, 1));

    del(L"veilos-test/one");
    del(L"veilos-test/two");
    del(L"veilos-testother");
    del(L"veilos-test/big");
    del(L"veilos-test/one");
    enumerate(L"veilos-test*");

    protect();
    return 0;
}
