/* W22 keys (S5b): keys typed at a console, as the key events a program reads.
 * Reads key records (raw input: no line editing, Ctrl+C a key) until 'q' goes
 * up, and prints each. `w22keys send` types the keys itself first, through
 * SendInput into its own console window, so Windows' console host makes the
 * records from keyboard input as it would from a person (that's how the
 * expected output is recorded); on VEILOS the test machine types them. Keys:
 * a, Shift+A, Ctrl+C, Up, F5, Delete, Enter, Alt+X, q. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static void press(WORD vk, BOOL up)
{
    INPUT i;
    memset(&i, 0, sizeof i);
    i.type = INPUT_KEYBOARD;
    i.ki.wVk = vk;
    i.ki.wScan = (WORD)MapVirtualKeyW(vk, MAPVK_VK_TO_VSC);
    if (vk == VK_UP || vk == VK_DELETE)
        i.ki.dwFlags |= KEYEVENTF_EXTENDEDKEY;
    if (up)
        i.ki.dwFlags |= KEYEVENTF_KEYUP;
    SendInput(1, &i, sizeof i);
    Sleep(30);
}

static void tap(WORD vk)
{
    press(vk, FALSE);
    press(vk, TRUE);
}

static void chord(WORD mod, WORD vk)
{
    press(mod, FALSE);
    tap(vk);
    press(mod, TRUE);
}

int main(int argc, char **argv)
{
    HANDLE in = GetStdHandle(STD_INPUT_HANDLE);
    if (argc > 1 && strcmp(argv[1], "send") == 0) {
        /* A console of its own, in front, to type into. */
        FreeConsole();
        AllocConsole();
        in = CreateFileW(L"CONIN$", GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
        SetForegroundWindow(GetConsoleWindow());
        Sleep(300);
    }
    SetConsoleMode(in, 0);
    FlushConsoleInputBuffer(in);
    if (argc > 1 && strcmp(argv[1], "send") == 0) {
        tap('A');
        chord(VK_SHIFT, 'A');
        chord(VK_CONTROL, 'C');
        tap(VK_UP);
        tap(VK_F5);
        tap(VK_DELETE);
        tap(VK_RETURN);
        chord(VK_MENU, 'X');
        tap('Q');
    }
    printf("reading\n");
    fflush(stdout);
    for (int n = 0; n < 64; n++) {
        INPUT_RECORD r;
        DWORD got = 0;
        if (!ReadConsoleInputW(in, &r, 1, &got) || got == 0)
            break;
        if (r.EventType != KEY_EVENT)
            continue;
        KEY_EVENT_RECORD *k = &r.Event.KeyEvent;
        /* Num Lock and the like depend on the keyboard's lights: left out. */
        printf("%-4s vk %02x scan %02x char %04x state %03lx\n", k->bKeyDown ? "down" : "up", k->wVirtualKeyCode, k->wVirtualScanCode,
               k->uChar.UnicodeChar, k->dwControlKeyState & ~(NUMLOCK_ON | SCROLLLOCK_ON | CAPSLOCK_ON));
        if (!k->bKeyDown && k->wVirtualKeyCode == 'Q')
            break;
    }
    printf("done\n");
    return 10;
}
