// W4 test: files, directories, paths and FindFirstFile.
#include "t.h"

static int write_all(HANDLE h, const char *s) {
    DWORD n = 0;
    return WriteFile(h, s, slen(s), &n, 0) && n == slen(s);
}

void start(void) {
    char buf[256];
    DWORD n = 0;

    // A fresh directory under C:\tmp.
    RemoveDirectoryA("C:\\tmp\\w4\\sub");
    DeleteFileA("C:\\tmp\\w4\\a.txt");
    DeleteFileA("C:\\tmp\\w4\\rel.txt");
    RemoveDirectoryA("C:\\tmp\\w4");
    CreateDirectoryA("C:\\tmp", 0);
    check("CreateDirectory C:\\tmp\\w4", CreateDirectoryA("C:\\tmp\\w4", 0));
    check_n("CreateDirectory again: ERROR_ALREADY_EXISTS", !CreateDirectoryA("C:\\tmp\\w4", 0) && GetLastError() == 183, GetLastError());

    HANDLE h = CreateFileA("C:\\tmp\\w4\\a.txt", GENERIC_WRITE, 0, 0, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, 0);
    check("CreateFile CREATE_ALWAYS", h != INVALID_HANDLE_VALUE);
    check("WriteFile", write_all(h, "first line\r\nsecond line\r\n"));
    check("CloseHandle", CloseHandle(h));

    DWORD attrs = GetFileAttributesA("C:\\tmp\\w4\\a.txt");
    check("GetFileAttributes: a file", attrs != INVALID_FILE_ATTRIBUTES && !(attrs & FILE_ATTRIBUTE_DIRECTORY));
    check("GetFileAttributes: a directory", GetFileAttributesA("C:\\tmp\\w4") & FILE_ATTRIBUTE_DIRECTORY);

    h = CreateFileA("C:\\tmp\\w4\\a.txt", GENERIC_READ, FILE_SHARE_READ, 0, OPEN_EXISTING, 0, 0);
    check_n("GetFileSize", GetFileSize(h, 0) == 25, GetFileSize(h, 0));
    check("ReadFile everything", ReadFile(h, buf, sizeof buf, &n, 0) && n == 25 && buf[0] == 'f' && buf[24] == '\n');
    check("ReadFile at end: 0 bytes", ReadFile(h, buf, sizeof buf, &n, 0) && n == 0);
    check("SetFilePointer to 12", SetFilePointer(h, 12, 0, FILE_BEGIN) == 12);
    check("ReadFile from 12", ReadFile(h, buf, 6, &n, 0) && n == 6 && buf[0] == 's' && buf[5] == 'd');
    check("GetFileType: disk", GetFileType(h) == FILE_TYPE_DISK);
    CloseHandle(h);

    // Append: open, seek to the end, write.
    h = CreateFileA("C:\\tmp\\w4\\a.txt", GENERIC_WRITE, 0, 0, OPEN_ALWAYS, 0, 0);
    check_n("OPEN_ALWAYS on an existing file: ERROR_ALREADY_EXISTS", h != INVALID_HANDLE_VALUE && GetLastError() == 183, GetLastError());
    SetFilePointer(h, 0, 0, FILE_END);
    check("append", write_all(h, "third\r\n"));
    CloseHandle(h);
    h = CreateFileA("C:\\tmp\\w4\\a.txt", GENERIC_READ, 0, 0, OPEN_EXISTING, 0, 0);
    check_n("size after append", GetFileSize(h, 0) == 32, GetFileSize(h, 0));
    CloseHandle(h);

    check_n("CREATE_NEW on an existing file: ERROR_FILE_EXISTS",
        CreateFileA("C:\\tmp\\w4\\a.txt", GENERIC_WRITE, 0, 0, CREATE_NEW, 0, 0) == INVALID_HANDLE_VALUE && GetLastError() == 80, GetLastError());
    check_n("missing file: ERROR_FILE_NOT_FOUND",
        CreateFileA("C:\\tmp\\w4\\nope.txt", GENERIC_READ, 0, 0, OPEN_EXISTING, 0, 0) == INVALID_HANDLE_VALUE && GetLastError() == 2, GetLastError());
    check_n("missing folder: ERROR_PATH_NOT_FOUND",
        CreateFileA("C:\\tmp\\nofolder\\x.txt", GENERIC_READ, 0, 0, OPEN_EXISTING, 0, 0) == INVALID_HANDLE_VALUE && GetLastError() == 3, GetLastError());

    // Relative paths follow the current directory.
    check("SetCurrentDirectory C:\\tmp\\w4", SetCurrentDirectoryA("C:\\tmp\\w4"));
    GetCurrentDirectoryA(sizeof buf, buf);
    check("GetCurrentDirectory", seq(buf, "C:\\tmp\\w4"));
    h = CreateFileA("rel.txt", GENERIC_WRITE, 0, 0, CREATE_NEW, 0, 0);
    check("relative CreateFile", h != INVALID_HANDLE_VALUE && write_all(h, "x"));
    CloseHandle(h);
    check("it landed in C:\\tmp\\w4", GetFileAttributesA("C:\\tmp\\w4\\rel.txt") != INVALID_FILE_ATTRIBUTES);
    char *part = 0;
    GetFullPathNameA("..\\w4\\.\\rel.txt", sizeof buf, buf, &part);
    check("GetFullPathName resolves . and ..", seq(buf, "C:\\tmp\\w4\\rel.txt") && part && seq(part, "rel.txt"));
    CreateDirectoryA("sub", 0);

    // FindFirstFile over the folder: ., .., a.txt, rel.txt, sub.
    WIN32_FIND_DATAA fd;
    int count = 0, saw_a = 0, saw_sub_dir = 0;
    HANDLE f = FindFirstFileA("C:\\tmp\\w4\\*", &fd);
    if (f != INVALID_HANDLE_VALUE) {
        do {
            count++;
            if (seq(fd.cFileName, "A.TXT") || seq(fd.cFileName, "a.txt")) saw_a = fd.nFileSizeLow == 32;
            if ((seq(fd.cFileName, "SUB") || seq(fd.cFileName, "sub")) && (fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)) saw_sub_dir = 1;
        } while (FindNextFileA(f, &fd));
        check_n("FindNextFile ends with ERROR_NO_MORE_FILES", GetLastError() == 18, GetLastError());
        FindClose(f);
    }
    check_n("FindFirstFile *: 5 entries", count == 5, count);
    check("a.txt found with its size", saw_a);
    check("sub found as a directory", saw_sub_dir);
    f = FindFirstFileA("C:\\tmp\\w4\\*.txt", &fd);
    count = 0;
    if (f != INVALID_HANDLE_VALUE) { do count++; while (FindNextFileA(f, &fd)); FindClose(f); }
    check_n("FindFirstFile *.txt: 2 entries", count == 2, count);

    check("DeleteFile", DeleteFileA("rel.txt") && GetFileAttributesA("rel.txt") == INVALID_FILE_ATTRIBUTES);
    check_n("RemoveDirectory of a non-empty folder fails", !RemoveDirectoryA("C:\\tmp\\w4") && GetLastError() == 145, GetLastError());
    check("RemoveDirectory of an empty folder", RemoveDirectoryA("sub"));

    done("w4file");
}
