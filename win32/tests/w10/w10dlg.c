/* G3a test: controls and dialogs. A dialog from a DIALOGEX resource, its
 * controls driven through messages, keyboard navigation through
 * IsDialogMessage, a modal DialogBox and a MessageBox ended from timers.
 * Prints what the dialog procedure is told and what the calls return
 * (nothing that depends on the font's size).
 *
 *   w10dlg        the scripted run, compared with Windows' output
 *   w10dlg wait   the dialog, open until closed with OK or Cancel
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static HWND dlg;
static int quiet, waiting;

static const char *code_name(int id, int code)
{
    if (id == 1001 || id == 1007) {
        switch (code) {
        case EN_CHANGE: return "EN_CHANGE";
        case EN_UPDATE: return "EN_UPDATE";
        case EN_SETFOCUS: return "EN_SETFOCUS";
        case EN_KILLFOCUS: return "EN_KILLFOCUS";
        case EN_MAXTEXT: return "EN_MAXTEXT";
        }
    } else if (id == 1005) {
        switch (code) {
        case LBN_SELCHANGE: return "LBN_SELCHANGE";
        case LBN_SETFOCUS: return "LBN_SETFOCUS";
        case LBN_KILLFOCUS: return "LBN_KILLFOCUS";
        case LBN_DBLCLK: return "LBN_DBLCLK";
        }
    } else if (id == 1006) {
        switch (code) {
        case CBN_SELCHANGE: return "CBN_SELCHANGE";
        case CBN_DROPDOWN: return "CBN_DROPDOWN";
        case CBN_CLOSEUP: return "CBN_CLOSEUP";
        case CBN_SETFOCUS: return "CBN_SETFOCUS";
        case CBN_KILLFOCUS: return "CBN_KILLFOCUS";
        case CBN_SELENDOK: return "CBN_SELENDOK";
        case CBN_SELENDCANCEL: return "CBN_SELENDCANCEL";
        }
    } else {
        switch (code) {
        case BN_CLICKED: return "BN_CLICKED";
        case BN_SETFOCUS: return "BN_SETFOCUS";
        case BN_KILLFOCUS: return "BN_KILLFOCUS";
        }
    }
    return "?";
}

static INT_PTR CALLBACK dlg_proc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    switch (m) {
    case WM_INITDIALOG:
        if (!waiting)
            printf("  WM_INITDIALOG param %d, focus control %d\n", (int)l, GetDlgCtrlID((HWND)w));
        return TRUE;
    case WM_SETFONT:
        if (!waiting)
            printf("  WM_SETFONT %d\n", w != 0);
        return 0;
    case WM_COMMAND:
        if (!quiet && !waiting)
            printf("  WM_COMMAND %d %s\n", LOWORD(w), code_name(LOWORD(w), HIWORD(w)));
        if (waiting && (LOWORD(w) == IDOK || LOWORD(w) == IDCANCEL))
            DestroyWindow(h);
        return TRUE;
    case WM_DESTROY:
        if (!waiting)
            printf("  WM_DESTROY\n");
        else
            PostQuitMessage(0);
        return 0;
    }
    return FALSE;
}

static void pump(void)
{
    MSG msg;
    while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
        if (!IsDialogMessageA(dlg, &msg)) {
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }
    }
}

static void key(int vk)
{
    PostMessageA(GetFocus(), WM_KEYDOWN, vk, 1);
    PostMessageA(GetFocus(), WM_KEYUP, vk, 0xC0000001);
    pump();
}

static INT_PTR CALLBACK modal_proc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    if (m == WM_INITDIALOG) {
        printf("  modal WM_INITDIALOG param %d\n", (int)l);
        SetTimer(h, 1, 50, NULL);
        return TRUE;
    }
    if (m == WM_TIMER) {
        KillTimer(h, 1);
        printf("  modal WM_TIMER, EndDialog\n");
        EndDialog(h, 42);
        return TRUE;
    }
    return FALSE;
}

static void CALLBACK answer_box(HWND h, UINT m, UINT_PTR id, DWORD t)
{
    HWND box = GetActiveWindow();
    char cls[64], title[64];
    KillTimer(NULL, id);
    GetClassNameA(box, cls, sizeof cls);
    GetWindowTextA(box, title, sizeof title);
    printf("  message box class %s title \"%s\" No button %d\n", cls, title, GetDlgItem(box, IDNO) != NULL);
    SendMessageA(box, WM_COMMAND, IDNO, 0);
}

int main(int argc, char **argv)
{
    HINSTANCE inst = GetModuleHandleA(NULL);
    HWND c;
    char text[128], cls[32];
    int i, ok;
    DWORD a, b;
    LONG base;
    RECT r = { 0, 0, 4, 8 };
    HWND list, combo, edit, multi, sb;

    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc > 1) {
        MSG msg;
        waiting = 1;
        dlg = CreateDialogParamA(inst, MAKEINTRESOURCE(101), NULL, dlg_proc, 0);
        SendDlgItemMessageA(dlg, 1005, LB_ADDSTRING, 0, (LPARAM)"pear");
        SendDlgItemMessageA(dlg, 1005, LB_ADDSTRING, 0, (LPARAM)"apple");
        SendDlgItemMessageA(dlg, 1006, CB_ADDSTRING, 0, (LPARAM)"red");
        SendDlgItemMessageA(dlg, 1006, CB_ADDSTRING, 0, (LPARAM)"green");
        SendDlgItemMessageA(dlg, 1006, CB_SETCURSEL, 0, 0);
        SetDlgItemTextA(dlg, 1007, "Line one\r\nLine two");
        ShowWindow(dlg, SW_SHOW);
        while (GetMessageA(&msg, NULL, 0, 0) > 0)
            if (!IsDialogMessageA(dlg, &msg)) {
                TranslateMessage(&msg);
                DispatchMessageA(&msg);
            }
        return 0;
    }

    printf("CreateDialogParam\n");
    dlg = CreateDialogParamA(inst, MAKEINTRESOURCE(101), NULL, dlg_proc, 55);
    GetClassNameA(dlg, cls, sizeof cls);
    printf("dialog class %s, visible %d, focus %d\n", cls, IsWindowVisible(dlg), GetDlgCtrlID(GetFocus()));
    for (c = GetWindow(dlg, GW_CHILD); c; c = GetWindow(c, GW_HWNDNEXT)) {
        GetClassNameA(c, cls, sizeof cls);
        GetWindowTextA(c, text, sizeof text);
        printf("  %d %s \"%s\" style %08lx\n", GetDlgCtrlID(c), cls, text, (unsigned long)GetWindowLongA(c, GWL_STYLE));
    }
    base = GetDialogBaseUnits();
    MapDialogRect(dlg, &r);
    printf("base units %d %d, MapDialogRect positive %d\n", LOWORD(base), HIWORD(base), r.right > 0 && r.bottom > 0);

    printf("edit\n");
    SetDlgItemTextA(dlg, 1001, "hello");
    GetDlgItemTextA(dlg, 1001, text, sizeof text);
    printf("text \"%s\" length %d\n", text, GetWindowTextLengthA(GetDlgItem(dlg, 1001)));
    SetDlgItemInt(dlg, 1001, (UINT)-42, TRUE);
    printf("int %d ok %d\n", (int)GetDlgItemInt(dlg, 1001, &ok, TRUE), ok);
    edit = GetDlgItem(dlg, 1001);
    SetWindowTextA(edit, "abcdef");
    SendMessageA(edit, EM_SETSEL, 1, 3);
    SendMessageA(edit, EM_GETSEL, (WPARAM)&a, (LPARAM)&b);
    printf("sel %lu %lu\n", a, b);
    SendMessageA(edit, EM_REPLACESEL, FALSE, (LPARAM)"XY");
    GetWindowTextA(edit, text, sizeof text);
    SendMessageA(edit, EM_GETSEL, (WPARAM)&a, (LPARAM)&b);
    printf("replaced \"%s\" sel %lu %lu\n", text, a, b);
    SendMessageA(edit, WM_CHAR, 'z', 1);
    GetWindowTextA(edit, text, sizeof text);
    printf("typed \"%s\"\n", text);
    SendMessageA(edit, WM_CHAR, VK_BACK, 1);
    SendMessageA(edit, EM_LIMITTEXT, 6, 0);
    SendMessageA(edit, WM_CHAR, 'q', 1);
    SendMessageA(edit, WM_CHAR, 'r', 1);
    GetWindowTextA(edit, text, sizeof text);
    printf("limited \"%s\" modified %d\n", text, (int)SendMessageA(edit, EM_GETMODIFY, 0, 0));
    multi = GetDlgItem(dlg, 1007);
    quiet = 1;
    SetWindowTextA(multi, "a\r\nbb\r\nccc");
    quiet = 0;
    printf("lines %d, index of 2 %d, length at 7 %d, line of 5 %d\n", (int)SendMessageA(multi, EM_GETLINECOUNT, 0, 0),
           (int)SendMessageA(multi, EM_LINEINDEX, 2, 0), (int)SendMessageA(multi, EM_LINELENGTH, 7, 0),
           (int)SendMessageA(multi, EM_LINEFROMCHAR, 5, 0));
    *(WORD *)text = sizeof text;
    i = (int)SendMessageA(multi, EM_GETLINE, 1, (LPARAM)text);
    text[i] = 0;
    printf("line 1 \"%s\"\n", text);

    printf("buttons\n");
    CheckDlgButton(dlg, 1002, BST_CHECKED);
    printf("checked %d\n", IsDlgButtonChecked(dlg, 1002));
    SendDlgItemMessageA(dlg, 1002, BM_CLICK, 0, 0);
    printf("after click %d\n", IsDlgButtonChecked(dlg, 1002));
    SendDlgItemMessageA(dlg, 1004, BM_CLICK, 0, 0);
    printf("radios %d %d\n", IsDlgButtonChecked(dlg, 1003), IsDlgButtonChecked(dlg, 1004));
    CheckRadioButton(dlg, 1003, 1004, 1003);
    printf("radios %d %d\n", IsDlgButtonChecked(dlg, 1003), IsDlgButtonChecked(dlg, 1004));
    SendDlgItemMessageA(dlg, IDCANCEL, BM_CLICK, 0, 0);
    printf("default button %d\n", LOWORD(SendMessageA(dlg, DM_GETDEFID, 0, 0)));

    printf("list box\n");
    list = GetDlgItem(dlg, 1005);
    printf("add %d %d %d\n", (int)SendMessageA(list, LB_ADDSTRING, 0, (LPARAM)"pear"),
           (int)SendMessageA(list, LB_ADDSTRING, 0, (LPARAM)"apple"), (int)SendMessageA(list, LB_ADDSTRING, 0, (LPARAM)"fig"));
    SendMessageA(list, LB_GETTEXT, 0, (LPARAM)text);
    printf("count %d, first \"%s\", length of 2 %d\n", (int)SendMessageA(list, LB_GETCOUNT, 0, 0), text,
           (int)SendMessageA(list, LB_GETTEXTLEN, 2, 0));
    SendMessageA(list, LB_SETCURSEL, 2, 0);
    printf("cursel %d, find f %d, exact fig %d, missing %d\n", (int)SendMessageA(list, LB_GETCURSEL, 0, 0),
           (int)SendMessageA(list, LB_FINDSTRING, (WPARAM)-1, (LPARAM)"f"), (int)SendMessageA(list, LB_FINDSTRINGEXACT, (WPARAM)-1, (LPARAM)"FIG"),
           (int)SendMessageA(list, LB_FINDSTRING, (WPARAM)-1, (LPARAM)"zz"));
    SendMessageA(list, LB_SETITEMDATA, 1, 1234);
    printf("item data %d, bad index %d\n", (int)SendMessageA(list, LB_GETITEMDATA, 1, 0), (int)SendMessageA(list, LB_GETTEXTLEN, 9, 0));
    printf("delete -> %d, insert kiwi at 1 -> %d\n", (int)SendMessageA(list, LB_DELETESTRING, 0, 0),
           (int)SendMessageA(list, LB_INSERTSTRING, 1, (LPARAM)"kiwi"));
    SendMessageA(list, LB_GETTEXT, 1, (LPARAM)text);
    printf("at 1 \"%s\", cursel %d, select p -> %d\n", text, (int)SendMessageA(list, LB_GETCURSEL, 0, 0),
           (int)SendMessageA(list, LB_SELECTSTRING, (WPARAM)-1, (LPARAM)"p"));

    printf("combo box\n");
    combo = GetDlgItem(dlg, 1006);
    SendMessageA(combo, CB_ADDSTRING, 0, (LPARAM)"red");
    SendMessageA(combo, CB_ADDSTRING, 0, (LPARAM)"green");
    SendMessageA(combo, CB_ADDSTRING, 0, (LPARAM)"blue");
    printf("count %d, cursel %d\n", (int)SendMessageA(combo, CB_GETCOUNT, 0, 0), (int)SendMessageA(combo, CB_GETCURSEL, 0, 0));
    SendMessageA(combo, CB_SETCURSEL, 1, 0);
    GetWindowTextA(combo, text, sizeof text);
    printf("cursel %d, text \"%s\", find blue %d\n", (int)SendMessageA(combo, CB_GETCURSEL, 0, 0), text,
           (int)SendMessageA(combo, CB_FINDSTRINGEXACT, (WPARAM)-1, (LPARAM)"blue"));
    SendMessageA(combo, CB_GETLBTEXT, 2, (LPARAM)text);
    printf("item 2 \"%s\" length %d\n", text, (int)SendMessageA(combo, CB_GETLBTEXTLEN, 2, 0));
    SendMessageA(combo, CB_SHOWDROPDOWN, TRUE, 0);
    printf("dropped %d\n", (int)SendMessageA(combo, CB_GETDROPPEDSTATE, 0, 0));
    SendMessageA(combo, CB_SHOWDROPDOWN, FALSE, 0);
    printf("dropped %d\n", (int)SendMessageA(combo, CB_GETDROPPEDSTATE, 0, 0));

    printf("scroll bar\n");
    sb = GetDlgItem(dlg, 1008);
    SetScrollRange(sb, SB_CTL, 0, 50, FALSE);
    SetScrollPos(sb, SB_CTL, 20, FALSE);
    printf("pos %d\n", GetScrollPos(sb, SB_CTL));
    {
        SCROLLINFO si = { sizeof si, SIF_PAGE | SIF_POS };
        si.nPage = 10;
        si.nPos = 45;
        printf("set page 10 pos 45 -> %d\n", SetScrollInfo(sb, SB_CTL, &si, FALSE));
    }

    printf("keyboard\n");
    ShowWindow(dlg, SW_SHOW);
    pump();
    SetFocus(GetDlgItem(dlg, 1001));
    pump();
    printf("focus %d\n", GetDlgCtrlID(GetFocus()));
    key(VK_TAB);
    printf("tab -> %d\n", GetDlgCtrlID(GetFocus()));
    key(VK_TAB);
    printf("tab -> %d\n", GetDlgCtrlID(GetFocus()));
    key(VK_DOWN);
    printf("down -> %d, radios %d %d\n", GetDlgCtrlID(GetFocus()), IsDlgButtonChecked(dlg, 1003), IsDlgButtonChecked(dlg, 1004));
    printf("next tab item after 1001: %d, next group item after 1003: %d\n",
           GetDlgCtrlID(GetNextDlgTabItem(dlg, GetDlgItem(dlg, 1001), FALSE)),
           GetDlgCtrlID(GetNextDlgGroupItem(dlg, GetDlgItem(dlg, 1003), FALSE)));
    SetFocus(GetDlgItem(dlg, 1001));
    pump();
    key(VK_RETURN);
    key(VK_ESCAPE);
    printf("destroy\n");
    DestroyWindow(dlg);
    dlg = NULL;

    printf("DialogBoxParam\n");
    printf("returned %d\n", (int)DialogBoxParamA(inst, MAKEINTRESOURCE(102), NULL, modal_proc, 77));
    printf("MessageBox\n");
    SetTimer(NULL, 0, 50, answer_box);
    printf("returned %d\n", MessageBoxA(NULL, "Shall we?", "Question", MB_YESNO | MB_ICONQUESTION));
    return 0;
}
