/* W15: rebar, ComboBoxEx, month calendar, date and time picker, list view
 * groups, NM_CUSTOMDRAW and the standard toolbar pictures. What's printed is
 * logic rather than font-dependent pixels. "w15ctl wait" also leaves a window
 * with all of them on screen. */
#define UNICODE
#define _UNICODE
#include <windows.h>
#include <commctrl.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>

static void wprint(const char *label, const WCHAR *s)
{
    char buf[512];
    WideCharToMultiByte(CP_UTF8, 0, s, -1, buf, sizeof buf, NULL, NULL);
    printf("%s: \"%s\"\n", label, buf);
}

static int cd_list_pre, cd_list_item, cd_list_post, cd_tree_item, cd_head_item, cd_tool_item;
static int cbe_insert, cbe_delete, dt_change;
static HWND lv_win, tv_win, hd_win, tb_win;

static LRESULT CALLBACK win_proc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    if (m == WM_NOTIFY) {
        NMHDR *n = (NMHDR *)l;
        if (n->code == NM_CUSTOMDRAW) {
            NMCUSTOMDRAW *c = (NMCUSTOMDRAW *)l;
            if (n->hwndFrom == lv_win) {
                NMLVCUSTOMDRAW *v = (NMLVCUSTOMDRAW *)l;
                if (c->dwDrawStage == CDDS_PREPAINT) { cd_list_pre++; return CDRF_NOTIFYITEMDRAW | CDRF_NOTIFYPOSTPAINT; }
                if (c->dwDrawStage == CDDS_ITEMPREPAINT) { cd_list_item++; v->clrText = RGB(200, 0, 0); return CDRF_NEWFONT; }
                if (c->dwDrawStage == CDDS_POSTPAINT) cd_list_post++;
                return CDRF_DODEFAULT;
            }
            if (n->hwndFrom == tv_win) {
                if (c->dwDrawStage == CDDS_PREPAINT) return CDRF_NOTIFYITEMDRAW;
                if (c->dwDrawStage == CDDS_ITEMPREPAINT) cd_tree_item++;
                return CDRF_DODEFAULT;
            }
            if (n->hwndFrom == tb_win) {
                if (c->dwDrawStage == CDDS_PREPAINT) return CDRF_NOTIFYITEMDRAW;
                if (c->dwDrawStage == CDDS_ITEMPREPAINT) cd_tool_item++;
                return CDRF_DODEFAULT;
            }
            return CDRF_DODEFAULT;
        }
        if (n->code == CBEN_INSERTITEM) cbe_insert++;
        if (n->code == CBEN_DELETEITEM) cbe_delete++;
        if (n->code == DTN_DATETIMECHANGE) dt_change++;
    }
    if (m == WM_DESTROY) { PostQuitMessage(0); return 0; }
    return DefWindowProcW(h, m, w, l);
}

static void pump(void)
{
    MSG msg;
    while (PeekMessageW(&msg, NULL, 0, 0, PM_REMOVE)) {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

static void test_rebar(HWND p)
{
    HWND rb = CreateWindowExW(0, REBARCLASSNAMEW, NULL, WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | RBS_VARHEIGHT | CCS_NODIVIDER,
        0, 0, 500, 40, p, (HMENU)10, NULL, NULL);
    printf("rebar created %d\n", rb != NULL);
    HWND child = CreateWindowExW(0, L"BUTTON", L"In band", WS_CHILD | WS_VISIBLE, 0, 0, 80, 22, rb, (HMENU)11, NULL, NULL);
    HWND child2 = CreateWindowExW(0, L"EDIT", L"", WS_CHILD | WS_VISIBLE | WS_BORDER, 0, 0, 80, 22, rb, (HMENU)12, NULL, NULL);
    REBARBANDINFOW b;
    memset(&b, 0, sizeof b);
    b.cbSize = sizeof b;
    b.fMask = RBBIM_CHILD | RBBIM_CHILDSIZE | RBBIM_SIZE | RBBIM_TEXT | RBBIM_ID | RBBIM_LPARAM | RBBIM_STYLE;
    b.fStyle = RBBS_GRIPPERALWAYS;
    b.hwndChild = child;
    b.cxMinChild = 60;
    b.cyMinChild = 22;
    b.cx = 200;
    b.lpText = L"First";
    b.wID = 77;
    b.lParam = 1234;
    printf("insert %d\n", (int)SendMessageW(rb, RB_INSERTBANDW, (WPARAM)-1, (LPARAM)&b));
    b.hwndChild = child2;
    b.lpText = L"Second";
    b.wID = 78;
    b.lParam = 5678;
    printf("insert %d\n", (int)SendMessageW(rb, RB_INSERTBANDW, (WPARAM)-1, (LPARAM)&b));
    printf("bands %d rows %d\n", (int)SendMessageW(rb, RB_GETBANDCOUNT, 0, 0), (int)SendMessageW(rb, RB_GETROWCOUNT, 0, 0));
    printf("id 78 at %d, id 9 at %d\n", (int)SendMessageW(rb, RB_IDTOINDEX, 78, 0), (int)SendMessageW(rb, RB_IDTOINDEX, 9, 0));
    WCHAR t[32];
    REBARBANDINFOW g;
    memset(&g, 0, sizeof g);
    g.cbSize = sizeof g;
    g.fMask = RBBIM_TEXT | RBBIM_ID | RBBIM_LPARAM | RBBIM_CHILD | RBBIM_CHILDSIZE;
    g.lpText = t;
    g.cch = 32;
    printf("get %d\n", (int)SendMessageW(rb, RB_GETBANDINFOW, 1, (LPARAM)&g));
    wprint("band text", t);
    printf("id %u param %d child ok %d min %u %u\n", g.wID, (int)g.lParam, g.hwndChild == child2, g.cxMinChild, g.cyMinChild);
    printf("bad get %d\n", (int)SendMessageW(rb, RB_GETBANDINFOW, 9, (LPARAM)&g));
    RECT r1, r2;
    SendMessageW(rb, RB_GETRECT, 0, (LPARAM)&r1);
    SendMessageW(rb, RB_GETRECT, 1, (LPARAM)&r2);
    printf("one row %d, second after first %d\n", r1.top == r2.top, r2.left >= r1.right);
    printf("bar height positive %d\n", (int)SendMessageW(rb, RB_GETBARHEIGHT, 0, 0) > 0);
    RECT cr;
    GetWindowRect(child, &cr);
    MapWindowPoints(NULL, rb, (POINT *)&cr, 2);
    printf("child in band 0 %d\n", cr.left >= r1.left && cr.right <= r1.right);
    /* A band on a row of its own. */
    b.fMask = RBBIM_STYLE;
    b.fStyle = RBBS_BREAK;
    SendMessageW(rb, RB_SETBANDINFOW, 1, (LPARAM)&b);
    printf("rows after break %d\n", (int)SendMessageW(rb, RB_GETROWCOUNT, 0, 0));
    SendMessageW(rb, RB_GETRECT, 1, (LPARAM)&r2);
    printf("below the first %d\n", r2.top >= r1.bottom);
    b.fStyle = 0;
    SendMessageW(rb, RB_SETBANDINFOW, 1, (LPARAM)&b);
    printf("show %d\n", (int)SendMessageW(rb, RB_SHOWBAND, 1, FALSE));
    printf("hidden visible %d\n", IsWindowVisible(child2));
    SendMessageW(rb, RB_SHOWBAND, 1, TRUE);
    printf("shown visible %d\n", IsWindowVisible(child2));
    COLORREF old = (COLORREF)SendMessageW(rb, RB_SETBKCOLOR, 0, RGB(1, 2, 3));
    printf("bk none %d, now %06x\n", old == CLR_NONE, (unsigned)SendMessageW(rb, RB_GETBKCOLOR, 0, 0));
    SendMessageW(rb, RB_SETBKCOLOR, 0, CLR_DEFAULT);
    int del = (int)SendMessageW(rb, RB_DELETEBAND, 1, 0);
    int left = (int)SendMessageW(rb, RB_GETBANDCOUNT, 0, 0);
    int bad = (int)SendMessageW(rb, RB_DELETEBAND, 5, 0);
    printf("delete %d, bands %d, bad %d\n", del, left, bad);
    b.fMask = RBBIM_CHILD | RBBIM_CHILDSIZE | RBBIM_SIZE | RBBIM_TEXT | RBBIM_ID;
    b.hwndChild = child2;
    b.lpText = L"Second";
    b.fStyle = 0;
    SendMessageW(rb, RB_INSERTBANDW, (WPARAM)-1, (LPARAM)&b);
}

static void test_comboex(HWND p)
{
    HWND c = CreateWindowExW(0, WC_COMBOBOXEXW, NULL, WS_CHILD | WS_VISIBLE | CBS_DROPDOWNLIST,
        10, 60, 200, 200, p, (HMENU)20, NULL, NULL);
    printf("comboex created %d\n", c != NULL);
    HWND inner = (HWND)SendMessageW(c, CBEM_GETCOMBOCONTROL, 0, 0);
    printf("has combo %d\n", inner != NULL);
    COMBOBOXEXITEMW it;
    memset(&it, 0, sizeof it);
    it.mask = CBEIF_TEXT | CBEIF_IMAGE | CBEIF_SELECTEDIMAGE | CBEIF_INDENT | CBEIF_LPARAM;
    const WCHAR *names[] = { L"Alpha", L"Beta", L"Gamma" };
    for (int i = 0; i < 3; i++) {
        it.iItem = i;
        it.pszText = (LPWSTR)names[i];
        it.iImage = i + 1;
        it.iSelectedImage = i + 2;
        it.iIndent = i;
        it.lParam = 100 + i;
        printf("insert %d\n", (int)SendMessageW(c, CBEM_INSERTITEMW, 0, (LPARAM)&it));
    }
    it.iItem = 1;
    it.pszText = L"Between";
    it.lParam = 500;
    printf("insert at 1: %d\n", (int)SendMessageW(c, CBEM_INSERTITEMW, 0, (LPARAM)&it));
    printf("count %d, notified %d\n", (int)SendMessageW(c, CB_GETCOUNT, 0, 0), cbe_insert);
    WCHAR t[32];
    COMBOBOXEXITEMW g;
    for (int i = 0; i < 4; i++) {
        memset(&g, 0, sizeof g);
        g.mask = CBEIF_TEXT | CBEIF_IMAGE | CBEIF_INDENT | CBEIF_LPARAM;
        g.iItem = i;
        g.pszText = t;
        g.cchTextMax = 32;
        int ok = (int)SendMessageW(c, CBEM_GETITEMW, 0, (LPARAM)&g);
        printf("get %d: %d image %d indent %d param %d ", i, ok, g.iImage, g.iIndent, (int)g.lParam);
        wprint("text", t);
    }
    memset(&g, 0, sizeof g);
    g.mask = CBEIF_LPARAM;
    g.iItem = 77;
    printf("bad get %d\n", (int)SendMessageW(c, CBEM_GETITEMW, 0, (LPARAM)&g));
    memset(&it, 0, sizeof it);
    it.mask = CBEIF_TEXT | CBEIF_LPARAM;
    it.iItem = 0;
    it.pszText = L"Changed";
    it.lParam = 999;
    printf("set %d\n", (int)SendMessageW(c, CBEM_SETITEMW, 0, (LPARAM)&it));
    memset(&g, 0, sizeof g);
    g.mask = CBEIF_TEXT | CBEIF_LPARAM;
    g.iItem = 0;
    g.pszText = t;
    g.cchTextMax = 32;
    SendMessageW(c, CBEM_GETITEMW, 0, (LPARAM)&g);
    printf("param %d ", (int)g.lParam);
    wprint("text", t);
    SendMessageW(c, CB_SETCURSEL, 2, 0);
    printf("sel %d\n", (int)SendMessageW(c, CB_GETCURSEL, 0, 0));
    memset(&g, 0, sizeof g);
    g.mask = CBEIF_TEXT;
    g.iItem = -1;
    g.pszText = t;
    g.cchTextMax = 32;
    SendMessageW(c, CBEM_GETITEMW, 0, (LPARAM)&g);
    wprint("current", t);
    int after = (int)SendMessageW(c, CB_DELETESTRING, 3, 0);
    printf("delete -> %d, notified %d\n", after, cbe_delete);
    HIMAGELIST il = ImageList_Create(16, 16, ILC_COLOR32, 2, 2);
    printf("old list %d\n", SendMessageW(c, CBEM_SETIMAGELIST, 0, (LPARAM)il) != 0);
    printf("list back %d\n", (HIMAGELIST)SendMessageW(c, CBEM_GETIMAGELIST, 0, 0) == il);
    printf("ex old %x\n", (unsigned)SendMessageW(c, CBEM_SETEXTENDEDSTYLE, 0, CBES_EX_NOEDITIMAGE));
    printf("ex now %x\n", (unsigned)SendMessageW(c, CBEM_GETEXTENDEDSTYLE, 0, 0));
}

static void show_date(const char *label, const SYSTEMTIME *s)
{
    printf("%s %04u-%02u-%02u dow %u %02u:%02u:%02u\n", label, s->wYear, s->wMonth, s->wDay, s->wDayOfWeek, s->wHour, s->wMinute, s->wSecond);
}

static void test_dates(HWND p)
{
    HWND mc = CreateWindowExW(0, MONTHCAL_CLASSW, NULL, WS_CHILD | WS_VISIBLE | WS_BORDER,
        230, 60, 240, 180, p, (HMENU)30, NULL, NULL);
    printf("month cal %d\n", mc != NULL);
    SYSTEMTIME s;
    memset(&s, 0, sizeof s);
    s.wYear = 2024; s.wMonth = 2; s.wDay = 29;
    printf("set %d\n", (int)SendMessageW(mc, MCM_SETCURSEL, 0, (LPARAM)&s));
    memset(&s, 0, sizeof s);
    printf("get %d\n", (int)SendMessageW(mc, MCM_GETCURSEL, 0, (LPARAM)&s));
    /* Only the date: Windows leaves the time fields as they fall. */
    printf("sel %04u-%02u-%02u dow %u\n", s.wYear, s.wMonth, s.wDay, s.wDayOfWeek);
    s.wYear = 2023; s.wMonth = 2; s.wDay = 29;
    printf("bad date %d\n", (int)SendMessageW(mc, MCM_SETCURSEL, 0, (LPARAM)&s));
    RECT r;
    int gotr = (int)SendMessageW(mc, MCM_GETMINREQRECT, 0, (LPARAM)&r);
    printf("min rect %d, sensible %d\n", gotr, r.right > 100 && r.bottom > 80);
    SYSTEMTIME rg[2];
    memset(rg, 0, sizeof rg);
    printf("no range %d\n", (int)SendMessageW(mc, MCM_GETRANGE, 0, (LPARAM)rg));
    rg[0].wYear = 2024; rg[0].wMonth = 1; rg[0].wDay = 1;
    rg[1].wYear = 2024; rg[1].wMonth = 12; rg[1].wDay = 31;
    printf("range %d\n", (int)SendMessageW(mc, MCM_SETRANGE, GDTR_MIN | GDTR_MAX, (LPARAM)rg));
    printf("get range %d\n", (int)SendMessageW(mc, MCM_GETRANGE, 0, (LPARAM)rg));
    SendMessageW(mc, MCM_SETRANGE, 0, 0);

    HWND dt = CreateWindowExW(0, DATETIMEPICK_CLASSW, NULL, WS_CHILD | WS_VISIBLE | DTS_SHORTDATEFORMAT | DTS_SHOWNONE,
        10, 100, 200, 24, p, (HMENU)31, NULL, NULL);
    printf("picker %d\n", dt != NULL);
    memset(&s, 0, sizeof s);
    s.wYear = 2025; s.wMonth = 12; s.wDay = 25; s.wHour = 13; s.wMinute = 5; s.wSecond = 9;
    printf("set %d\n", (int)SendMessageW(dt, DTM_SETSYSTEMTIME, GDT_VALID, (LPARAM)&s));
    memset(&s, 0, sizeof s);
    printf("get %d\n", (int)SendMessageW(dt, DTM_GETSYSTEMTIME, 0, (LPARAM)&s));
    show_date("value", &s);
    printf("set none %d\n", (int)SendMessageW(dt, DTM_SETSYSTEMTIME, GDT_NONE, 0));
    printf("get none %d\n", (int)SendMessageW(dt, DTM_GETSYSTEMTIME, 0, (LPARAM)&s));
    s.wYear = 2025; s.wMonth = 12; s.wDay = 25;
    SendMessageW(dt, DTM_SETSYSTEMTIME, GDT_VALID, (LPARAM)&s);
    printf("format %d\n", (int)SendMessageW(dt, DTM_SETFORMATW, 0, (LPARAM)L"yyyy'-'MM'-'dd HH':'mm"));
    WCHAR t[64];
    GetWindowTextW(dt, t, 64);
    wprint("shown", t);
    SendMessageW(dt, DTM_SETFORMATW, 0, (LPARAM)L"dddd d MMMM yy, h:mm tt");
    GetWindowTextW(dt, t, 64);
    wprint("shown", t);
    SendMessageW(dt, DTM_SETFORMATW, 0, 0);
    HWND d2 = CreateWindowExW(0, DATETIMEPICK_CLASSW, NULL, WS_CHILD | WS_VISIBLE | DTS_TIMEFORMAT,
        10, 130, 200, 24, p, (HMENU)32, NULL, NULL);
    printf("time picker %d\n", d2 != NULL);
    s.wYear = 2025; s.wMonth = 1; s.wDay = 2; s.wHour = 23; s.wMinute = 59; s.wSecond = 58;
    SendMessageW(d2, DTM_SETSYSTEMTIME, GDT_VALID, (LPARAM)&s);
    SendMessageW(d2, DTM_GETSYSTEMTIME, 0, (LPARAM)&s);
    show_date("time", &s);
}

static void test_groups(HWND p)
{
    lv_win = CreateWindowExW(0, WC_LISTVIEWW, NULL, WS_CHILD | WS_VISIBLE | WS_BORDER | LVS_REPORT,
        10, 250, 300, 170, p, (HMENU)40, NULL, NULL);
    hd_win = ListView_GetHeader(lv_win);
    LVCOLUMNW col;
    memset(&col, 0, sizeof col);
    col.mask = LVCF_TEXT | LVCF_WIDTH;
    col.cx = 200;
    col.pszText = L"Name";
    ListView_InsertColumn(lv_win, 0, &col);
    printf("view on %d\n", (int)SendMessageW(lv_win, LVM_ENABLEGROUPVIEW, TRUE, 0));
    printf("view again %d\n", (int)SendMessageW(lv_win, LVM_ENABLEGROUPVIEW, TRUE, 0));
    printf("enabled %d\n", (int)SendMessageW(lv_win, LVM_ISGROUPVIEWENABLED, 0, 0));
    LVGROUP g;
    memset(&g, 0, sizeof g);
    g.cbSize = sizeof g;
    g.mask = LVGF_HEADER | LVGF_GROUPID;
    g.pszHeader = L"Fruit";
    g.iGroupId = 1;
    printf("insert %d\n", (int)SendMessageW(lv_win, LVM_INSERTGROUP, (WPARAM)-1, (LPARAM)&g));
    g.pszHeader = L"Vegetables";
    g.iGroupId = 2;
    printf("insert %d\n", (int)SendMessageW(lv_win, LVM_INSERTGROUP, (WPARAM)-1, (LPARAM)&g));
    g.iGroupId = 2;
    printf("duplicate %d\n", (int)SendMessageW(lv_win, LVM_INSERTGROUP, (WPARAM)-1, (LPARAM)&g));
    int cnt = (int)SendMessageW(lv_win, LVM_GETGROUPCOUNT, 0, 0);
    int has1 = (int)SendMessageW(lv_win, LVM_HASGROUP, 1, 0);
    int has9 = (int)SendMessageW(lv_win, LVM_HASGROUP, 9, 0);
    printf("groups %d has1 %d has9 %d\n", cnt, has1, has9);
    const WCHAR *names[] = { L"Carrot", L"Apple", L"Leek", L"Pear" };
    int groups[] = { 2, 1, 2, 1 };
    for (int i = 0; i < 4; i++) {
        LVITEMW it;
        memset(&it, 0, sizeof it);
        it.mask = LVIF_TEXT | LVIF_GROUPID;
        it.iItem = i;
        it.pszText = (LPWSTR)names[i];
        it.iGroupId = groups[i];
        printf("item %d\n", ListView_InsertItem(lv_win, &it));
    }
    for (int i = 0; i < 4; i++) {
        LVITEMW it;
        memset(&it, 0, sizeof it);
        it.mask = LVIF_GROUPID;
        it.iItem = i;
        ListView_GetItem(lv_win, &it);
        printf("group of %d: %d\n", i, it.iGroupId);
    }
    /* Items stay in their index; groups place them on screen. Apple (1) is
     * in the first group, above Carrot (0) in the second. */
    pump();
    RECT r0, r1, r2, r3;
    ListView_GetItemRect(lv_win, 0, &r0, LVIR_BOUNDS);
    ListView_GetItemRect(lv_win, 1, &r1, LVIR_BOUNDS);
    ListView_GetItemRect(lv_win, 2, &r2, LVIR_BOUNDS);
    ListView_GetItemRect(lv_win, 3, &r3, LVIR_BOUNDS);
    printf("order: apple above pear %d, pear above carrot %d, carrot above leek %d\n", r1.top < r3.top, r3.top < r0.top, r0.top < r2.top);
    printf("gap between groups %d\n", r0.top - r3.bottom > 0);
    WCHAR t[32];
    memset(&g, 0, sizeof g);
    g.cbSize = sizeof g;
    g.mask = LVGF_HEADER | LVGF_GROUPID;
    g.pszHeader = t;
    g.cchHeader = 32;
    printf("get %d\n", (int)SendMessageW(lv_win, LVM_GETGROUPINFO, 2, (LPARAM)&g));
    wprint("group", t);
    int id = g.iGroupId;
    printf("id %d, bad %d\n", id, (int)SendMessageW(lv_win, LVM_GETGROUPINFO, 44, (LPARAM)&g));
    memset(&g, 0, sizeof g);
    g.cbSize = sizeof g;
    g.mask = LVGF_HEADER;
    g.pszHeader = L"Veg";
    printf("set %d\n", (int)SendMessageW(lv_win, LVM_SETGROUPINFO, 2, (LPARAM)&g));
    g.mask = LVGF_HEADER;
    g.pszHeader = t;
    g.cchHeader = 32;
    SendMessageW(lv_win, LVM_GETGROUPINFO, 2, (LPARAM)&g);
    wprint("renamed", t);
    printf("remove bad %d\n", (int)SendMessageW(lv_win, LVM_REMOVEGROUP, 44, 0));
}

static void test_draw_and_pictures(HWND p)
{
    /* Custom draw: counts from a real paint. */
    tv_win = CreateWindowExW(0, WC_TREEVIEWW, NULL, WS_CHILD | WS_VISIBLE | WS_BORDER,
        320, 250, 150, 170, p, (HMENU)50, NULL, NULL);
    TVINSERTSTRUCTW ins;
    memset(&ins, 0, sizeof ins);
    ins.hInsertAfter = TVI_LAST;
    ins.item.mask = TVIF_TEXT;
    ins.item.pszText = L"One";
    TreeView_InsertItem(tv_win, &ins);
    ins.item.pszText = L"Two";
    TreeView_InsertItem(tv_win, &ins);

    tb_win = CreateWindowExW(0, TOOLBARCLASSNAMEW, NULL, WS_CHILD | WS_VISIBLE | TBSTYLE_FLAT | CCS_NODIVIDER | CCS_NORESIZE,
        10, 430, 460, 30, p, (HMENU)51, NULL, NULL);
    SendMessageW(tb_win, TB_BUTTONSTRUCTSIZE, sizeof(TBBUTTON), 0);
    int first = (int)SendMessageW(tb_win, TB_LOADIMAGES, IDB_STD_SMALL_COLOR, (LPARAM)HINST_COMMCTRL);
    printf("std first %d\n", first);
    HIMAGELIST il = (HIMAGELIST)SendMessageW(tb_win, TB_GETIMAGELIST, 0, 0);
    printf("has list %d, images %d\n", il != NULL, il ? ImageList_GetImageCount(il) : 0);
    int second = (int)SendMessageW(tb_win, TB_LOADIMAGES, IDB_VIEW_SMALL_COLOR, (LPARAM)HINST_COMMCTRL);
    printf("view first %d\n", second);
    il = (HIMAGELIST)SendMessageW(tb_win, TB_GETIMAGELIST, 0, 0);
    int third = (int)SendMessageW(tb_win, TB_LOADIMAGES, IDB_HIST_SMALL_COLOR, (LPARAM)HINST_COMMCTRL);
    printf("hist first %d, images %d\n", third, il ? ImageList_GetImageCount(il) : 0);
    int cx = 0, cy = 0;
    if (il) ImageList_GetIconSize(il, &cx, &cy);
    printf("size %d %d\n", cx, cy);
    TBADDBITMAP ab = { HINST_COMMCTRL, IDB_STD_SMALL_COLOR };
    printf("add bitmap %d\n", (int)SendMessageW(tb_win, TB_ADDBITMAP, 0, (LPARAM)&ab));
    TBBUTTON bs[6];
    int imgs[] = { STD_FILENEW, STD_FILEOPEN, STD_FILESAVE, STD_CUT, STD_COPY, STD_PASTE };
    memset(bs, 0, sizeof bs);
    for (int i = 0; i < 6; i++) {
        bs[i].iBitmap = first + imgs[i];
        bs[i].idCommand = 100 + i;
        bs[i].fsState = TBSTATE_ENABLED;
        bs[i].fsStyle = BTNS_BUTTON;
    }
    printf("buttons %d\n", (int)SendMessageW(tb_win, TB_ADDBUTTONSW, 6, (LPARAM)bs));

    /* A picture really has pixels in it, over a background that still shows. */
    HDC screen = GetDC(NULL);
    HDC dc = CreateCompatibleDC(screen);
    HBITMAP bm = CreateCompatibleBitmap(screen, 16, 16);
    HGDIOBJ old = SelectObject(dc, bm);
    RECT rr = { 0, 0, 16, 16 };
    HBRUSH green = CreateSolidBrush(RGB(0, 255, 0));
    FillRect(dc, &rr, green);
    DeleteObject(green);
    if (il) ImageList_Draw(il, first + STD_FILESAVE, dc, 0, 0, ILD_TRANSPARENT);
    int bg = 0, ink = 0;
    for (int y = 0; y < 16; y++)
        for (int x = 0; x < 16; x++) {
            if (GetPixel(dc, x, y) == RGB(0, 255, 0)) bg++;
            else ink++;
        }
    printf("save picture: ink %d, background %d\n", ink > 20, bg > 0);
    SelectObject(dc, old);
    DeleteObject(bm);
    DeleteDC(dc);
    ReleaseDC(NULL, screen);
}

int main(int argc, char **argv)
{
    INITCOMMONCONTROLSEX icc = { sizeof icc, ICC_WIN95_CLASSES | ICC_COOL_CLASSES | ICC_USEREX_CLASSES | ICC_DATE_CLASSES };
    printf("init %d\n", InitCommonControlsEx(&icc));
    WNDCLASSW wc;
    memset(&wc, 0, sizeof wc);
    wc.lpfnWndProc = win_proc;
    wc.lpszClassName = L"W15Win";
    wc.hbrBackground = (HBRUSH)(COLOR_BTNFACE + 1);
    wc.hCursor = LoadCursorW(NULL, (LPCWSTR)IDC_ARROW);
    RegisterClassW(&wc);
    HWND w = CreateWindowExW(0, L"W15Win", L"W15 controls", WS_OVERLAPPEDWINDOW | WS_VISIBLE,
        30, 30, 500, 520, NULL, NULL, NULL, NULL);
    printf("window %d\n", w != NULL);
    test_rebar(w);
    test_comboex(w);
    test_dates(w);
    test_groups(w);
    test_draw_and_pictures(w);
    pump();
    InvalidateRect(lv_win, NULL, TRUE);
    InvalidateRect(tv_win, NULL, TRUE);
    InvalidateRect(tb_win, NULL, TRUE);
    UpdateWindow(lv_win);
    UpdateWindow(tv_win);
    UpdateWindow(tb_win);
    pump();
    printf("list prepaint %d, items %d, post %d\n", cd_list_pre > 0, cd_list_item >= 4, cd_list_post > 0);
    printf("tree items %d, toolbar items %d\n", cd_tree_item >= 2, cd_tool_item >= 6);
    if (argc > 1 && strcmp(argv[1], "wait") == 0) {
        MSG msg;
        while (GetMessageW(&msg, NULL, 0, 0) > 0) {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    } else {
        DestroyWindow(w);
    }
    printf("done\n");
    return 7;
}
