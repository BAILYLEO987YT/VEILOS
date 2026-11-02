/* W13 (G4b): list view, tree view, header, toolbar, tab control, property
 * sheets and wizards, DPA/DSA, rich edit, DrawEdge. What's printed is logic and
 * structure rather than font-dependent pixels. "w13ctl wait" also leaves a
 * window with all of them on screen. */
#include <windows.h>
#include <commctrl.h>
#include <richedit.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>

static void wprint(const char *label, const WCHAR *s)
{
    char buf[512];
    WideCharToMultiByte(CP_UTF8, 0, s, -1, buf, sizeof buf, NULL, NULL);
    printf("%s: \"%s\"\n", label, buf);
}

static HWND main_win;
static int log_notify;

static const char *nm_name(int code)
{
    static char buf[32];
    switch (code) {
    case LVN_ITEMCHANGING: return "LVN_ITEMCHANGING";
    case LVN_ITEMCHANGED: return "LVN_ITEMCHANGED";
    case LVN_INSERTITEM: return "LVN_INSERTITEM";
    case LVN_DELETEITEM: return "LVN_DELETEITEM";
    case LVN_DELETEALLITEMS: return "LVN_DELETEALLITEMS";
    case LVN_COLUMNCLICK: return "LVN_COLUMNCLICK";
    case TVN_SELCHANGINGW: return "TVN_SELCHANGING";
    case TVN_SELCHANGEDW: return "TVN_SELCHANGED";
    case TVN_ITEMEXPANDINGW: return "TVN_ITEMEXPANDING";
    case TVN_ITEMEXPANDEDW: return "TVN_ITEMEXPANDED";
    case TVN_DELETEITEMW: return "TVN_DELETEITEM";
    case TCN_SELCHANGING: return "TCN_SELCHANGING";
    case TCN_SELCHANGE: return "TCN_SELCHANGE";
    }
    sprintf(buf, "%d", code);
    return buf;
}

static WCHAR tv_name_buf[64];

static LRESULT CALLBACK win_proc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    if (m == WM_NOTIFY) {
        NMHDR *n = (NMHDR *)l;
        if (n->code == LVN_GETDISPINFOW) {
            NMLVDISPINFOW *d = (NMLVDISPINFOW *)l;
            if (d->item.mask & LVIF_TEXT)
                swprintf(d->item.pszText, d->item.cchTextMax, L"cb%d.%d", d->item.iItem, d->item.iSubItem);
            return 0;
        }
        if (n->code == TVN_GETDISPINFOW) {
            NMTVDISPINFOW *d = (NMTVDISPINFOW *)l;
            if (d->item.mask & TVIF_TEXT) {
                swprintf(tv_name_buf, 64, L"tv%d", (int)d->item.lParam);
                d->item.pszText = tv_name_buf;
            }
            if (d->item.mask & TVIF_CHILDREN)
                d->item.cChildren = 1;
            return 0;
        }
        if (!log_notify)
            return 0;
        switch (n->code) {
        case LVN_ITEMCHANGING:
        case LVN_ITEMCHANGED:
        case LVN_INSERTITEM:
        case LVN_DELETEITEM:
        case LVN_DELETEALLITEMS:
        case LVN_COLUMNCLICK: {
            NMLISTVIEW *lv = (NMLISTVIEW *)l;
            printf("  %s item %d sub %d new %x old %x changed %x param %d\n", nm_name(n->code), lv->iItem, lv->iSubItem,
                   lv->uNewState, lv->uOldState, lv->uChanged, (int)lv->lParam);
            return 0;
        }
        case TVN_SELCHANGINGW:
        case TVN_SELCHANGEDW:
        case TVN_ITEMEXPANDINGW:
        case TVN_ITEMEXPANDEDW:
        case TVN_DELETEITEMW: {
            NMTREEVIEWW *tv = (NMTREEVIEWW *)l;
            printf("  %s action %u old %d new %d\n", nm_name(n->code), tv->action, (int)tv->itemOld.lParam, (int)tv->itemNew.lParam);
            return 0;
        }
        case TCN_SELCHANGING:
        case TCN_SELCHANGE:
            printf("  %s\n", nm_name(n->code));
            return 0;
        }
        return 0;
    }
    if (m == WM_COMMAND && log_notify) {
        printf("  WM_COMMAND id %d code %d\n", LOWORD(w), HIWORD(w));
        return 0;
    }
    if (m == WM_DESTROY)
        PostQuitMessage(0);
    return DefWindowProcW(h, m, w, l);
}

/* -----| list view |----- */

static int CALLBACK by_param_desc(LPARAM a, LPARAM b, LPARAM sort)
{
    return (int)(b - a) * (int)sort;
}

static void lv_dump(HWND lv)
{
    int n = (int)SendMessageW(lv, LVM_GETITEMCOUNT, 0, 0);
    printf("items %d:", n);
    for (int i = 0; i < n; i++) {
        WCHAR t[32];
        LVITEMW it = {0};
        it.mask = LVIF_TEXT | LVIF_PARAM | LVIF_STATE | LVIF_IMAGE;
        it.iItem = i;
        it.pszText = t;
        it.cchTextMax = 32;
        it.stateMask = LVIS_SELECTED | LVIS_FOCUSED;
        SendMessageW(lv, LVM_GETITEMW, 0, (LPARAM)&it);
        char a[32];
        WideCharToMultiByte(CP_UTF8, 0, t, -1, a, 32, NULL, NULL);
        printf(" [%s p%d s%x i%d]", a, (int)it.lParam, it.state, it.iImage);
    }
    printf("\n");
}

static HWND listview(HWND parent, DWORD extra)
{
    return CreateWindowExW(0, WC_LISTVIEWW, L"", WS_CHILD | WS_VISIBLE | WS_BORDER | LVS_REPORT | LVS_SHOWSELALWAYS | extra,
                           0, 0, 400, 200, parent, (HMENU)101, NULL, NULL);
}

static void test_listview(void)
{
    printf("--- list view\n");
    HWND lv = listview(main_win, 0);
    printf("view %d, extended %#x\n", (int)SendMessageW(lv, LVM_GETVIEW, 0, 0), (unsigned)SendMessageW(lv, LVM_GETEXTENDEDLISTVIEWSTYLE, 0, 0));
    LVCOLUMNW col = {0};
    col.mask = LVCF_TEXT | LVCF_WIDTH | LVCF_FMT;
    col.pszText = L"Name";
    col.cx = 100;
    printf("insert column %d", (int)SendMessageW(lv, LVM_INSERTCOLUMNW, 0, (LPARAM)&col));
    col.pszText = L"Size";
    col.cx = 60;
    col.fmt = LVCFMT_RIGHT;
    printf(" %d", (int)SendMessageW(lv, LVM_INSERTCOLUMNW, 1, (LPARAM)&col));
    col.pszText = L"Kind";
    col.cx = 80;
    col.fmt = LVCFMT_LEFT;
    printf(" %d\n", (int)SendMessageW(lv, LVM_INSERTCOLUMNW, 2, (LPARAM)&col));
    HWND hdr = (HWND)SendMessageW(lv, LVM_GETHEADER, 0, 0);
    printf("header: %d, items %d\n", hdr != NULL, (int)SendMessageW(hdr, HDM_GETITEMCOUNT, 0, 0));
    WCHAR tb[32];
    LVCOLUMNW got = {0};
    got.mask = LVCF_TEXT | LVCF_WIDTH | LVCF_FMT | LVCF_SUBITEM | LVCF_ORDER;
    got.pszText = tb;
    got.cchTextMax = 32;
    SendMessageW(lv, LVM_GETCOLUMNW, 1, (LPARAM)&got);
    printf("column 1: width %d fmt %d sub %d order %d\n", got.cx, got.fmt, got.iSubItem, got.iOrder);
    wprint("  text", tb);
    printf("width of column 2: %d\n", (int)SendMessageW(lv, LVM_GETCOLUMNWIDTH, 2, 0));

    log_notify = 1;
    const WCHAR *names[] = {L"gamma", L"alpha", L"delta", L"beta"};
    for (int i = 0; i < 4; i++) {
        LVITEMW it = {0};
        it.mask = LVIF_TEXT | LVIF_PARAM | LVIF_IMAGE;
        it.iItem = i;
        it.pszText = (WCHAR *)names[i];
        it.lParam = (i + 1) * 10;
        it.iImage = i;
        printf("insert: %d\n", (int)SendMessageW(lv, LVM_INSERTITEMW, 0, (LPARAM)&it));
    }
    LVITEMW sub = {0};
    sub.iSubItem = 1;
    sub.pszText = L"1024";
    SendMessageW(lv, LVM_SETITEMTEXTW, 0, (LPARAM)&sub);
    sub.iSubItem = 2;
    sub.pszText = LPSTR_TEXTCALLBACKW;
    SendMessageW(lv, LVM_SETITEMTEXTW, 0, (LPARAM)&sub);
    lv_dump(lv);
    WCHAR cell[32];
    LVITEMW q = {0};
    q.iSubItem = 1;
    q.pszText = cell;
    q.cchTextMax = 32;
    int len = (int)SendMessageW(lv, LVM_GETITEMTEXTW, 0, (LPARAM)&q);
    printf("cell 0.1 length %d\n", len);
    wprint("  text", cell);
    q.iSubItem = 2;
    len = (int)SendMessageW(lv, LVM_GETITEMTEXTW, 0, (LPARAM)&q);
    printf("callback cell length %d\n", len);
    wprint("  text", cell);
    q.iSubItem = 1;
    q.cchTextMax = 3;
    len = (int)SendMessageW(lv, LVM_GETITEMTEXTW, 0, (LPARAM)&q);
    printf("cut to fit: length %d\n", len);
    wprint("  text", cell);

    printf("select 1:\n");
    LVITEMW st = {0};
    st.state = LVIS_SELECTED | LVIS_FOCUSED;
    st.stateMask = LVIS_SELECTED | LVIS_FOCUSED;
    SendMessageW(lv, LVM_SETITEMSTATE, 1, (LPARAM)&st);
    printf("select 3 too:\n");
    st.state = LVIS_SELECTED;
    st.stateMask = LVIS_SELECTED;
    SendMessageW(lv, LVM_SETITEMSTATE, 3, (LPARAM)&st);
    printf("selected %d, focus %d, first %d, next %d, none after %d\n", (int)SendMessageW(lv, LVM_GETSELECTEDCOUNT, 0, 0),
           (int)SendMessageW(lv, LVM_GETNEXTITEM, (WPARAM)-1, LVNI_FOCUSED), (int)SendMessageW(lv, LVM_GETNEXTITEM, (WPARAM)-1, LVNI_SELECTED),
           (int)SendMessageW(lv, LVM_GETNEXTITEM, 1, LVNI_SELECTED), (int)SendMessageW(lv, LVM_GETNEXTITEM, 3, LVNI_SELECTED));
    printf("state of 1: %x, of 0: %x\n", (unsigned)SendMessageW(lv, LVM_GETITEMSTATE, 1, LVIS_SELECTED | LVIS_FOCUSED), (unsigned)SendMessageW(lv, LVM_GETITEMSTATE, 0, LVIS_SELECTED));
    printf("deselect all:\n");
    st.state = 0;
    st.stateMask = LVIS_SELECTED;
    SendMessageW(lv, LVM_SETITEMSTATE, (WPARAM)-1, (LPARAM)&st);

    LVFINDINFOW fi = {0};
    fi.flags = LVFI_STRING;
    fi.psz = L"DELTA";
    printf("find \"DELTA\" %d", (int)SendMessageW(lv, LVM_FINDITEMW, (WPARAM)-1, (LPARAM)&fi));
    fi.flags = LVFI_PARTIAL | LVFI_STRING;
    fi.psz = L"be";
    printf(", partial \"be\" %d", (int)SendMessageW(lv, LVM_FINDITEMW, (WPARAM)-1, (LPARAM)&fi));
    fi.flags = LVFI_PARAM;
    fi.lParam = 30;
    printf(", param 30 %d", (int)SendMessageW(lv, LVM_FINDITEMW, (WPARAM)-1, (LPARAM)&fi));
    fi.flags = LVFI_STRING;
    fi.psz = L"gamma";
    printf(", after its own place %d", (int)SendMessageW(lv, LVM_FINDITEMW, 0, (LPARAM)&fi));
    fi.flags = LVFI_STRING | LVFI_WRAP;
    printf(", wrapping %d\n", (int)SendMessageW(lv, LVM_FINDITEMW, 0, (LPARAM)&fi));

    log_notify = 0;
    SendMessageW(lv, LVM_SORTITEMS, 1, (LPARAM)by_param_desc);
    printf("sorted by param, descending: ");
    lv_dump(lv);
    log_notify = 1;
    printf("delete 1: %d\n", (int)SendMessageW(lv, LVM_DELETEITEM, 1, 0));
    printf("delete 9: %d\n", (int)SendMessageW(lv, LVM_DELETEITEM, 9, 0));
    lv_dump(lv);
    printf("delete column 1: %d, header items %d\n", (int)SendMessageW(lv, LVM_DELETECOLUMN, 1, 0), (int)SendMessageW(hdr, HDM_GETITEMCOUNT, 0, 0));
    printf("delete all: %d\n", (int)SendMessageW(lv, LVM_DELETEALLITEMS, 0, 0));
    lv_dump(lv);
    log_notify = 0;

    printf("extended: was %#x", (unsigned)SendMessageW(lv, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, LVS_EX_FULLROWSELECT | LVS_EX_GRIDLINES));
    printf(", now %#x\n", (unsigned)SendMessageW(lv, LVM_GETEXTENDEDLISTVIEWSTYLE, 0, 0));
    printf("set view list: %d, view %d, style low bits %d\n", (int)SendMessageW(lv, LVM_SETVIEW, LV_VIEW_LIST, 0),
           (int)SendMessageW(lv, LVM_GETVIEW, 0, 0), (int)(GetWindowLongW(lv, GWL_STYLE) & LVS_TYPEMASK));
    DestroyWindow(lv);

    /* Single selection; item-rect structure; subitem hit test. */
    lv = listview(main_win, LVS_SINGLESEL);
    col.mask = LVCF_TEXT | LVCF_WIDTH;
    col.pszText = L"A";
    col.cx = 100;
    SendMessageW(lv, LVM_INSERTCOLUMNW, 0, (LPARAM)&col);
    col.pszText = L"B";
    col.cx = 50;
    SendMessageW(lv, LVM_INSERTCOLUMNW, 1, (LPARAM)&col);
    for (int i = 0; i < 3; i++) {
        LVITEMW it = {0};
        it.mask = LVIF_TEXT;
        it.iItem = i;
        it.pszText = L"row";
        SendMessageW(lv, LVM_INSERTITEMW, 0, (LPARAM)&it);
    }
    st.state = LVIS_SELECTED;
    st.stateMask = LVIS_SELECTED;
    SendMessageW(lv, LVM_SETITEMSTATE, 0, (LPARAM)&st);
    SendMessageW(lv, LVM_SETITEMSTATE, 2, (LPARAM)&st);
    printf("single selection: count %d, is %d\n", (int)SendMessageW(lv, LVM_GETSELECTEDCOUNT, 0, 0), (int)SendMessageW(lv, LVM_GETNEXTITEM, (WPARAM)-1, LVNI_SELECTED));
    RECT r0, r1;
    r0.left = LVIR_BOUNDS;
    r1.left = LVIR_BOUNDS;
    SendMessageW(lv, LVM_GETITEMRECT, 0, (LPARAM)&r0);
    SendMessageW(lv, LVM_GETITEMRECT, 1, (LPARAM)&r1);
    printf("row 0 spans x %d..%d, rows stack %d\n", r0.left, r0.right, r0.bottom == r1.top);
    RECT sr;
    sr.top = 1;
    sr.left = LVIR_BOUNDS;
    SendMessageW(lv, LVM_GETSUBITEMRECT, 0, (LPARAM)&sr);
    printf("subitem 1 spans x %d..%d, same row %d\n", sr.left, sr.right, sr.top == r0.top);
    LVHITTESTINFO ht = {0};
    ht.pt.x = 120;
    ht.pt.y = r1.top + 1;
    int who = (int)SendMessageW(lv, LVM_SUBITEMHITTEST, 0, (LPARAM)&ht);
    printf("subitem hit: %d item %d sub %d\n", who, ht.iItem, ht.iSubItem);
    ht.pt.x = 350;
    who = (int)SendMessageW(lv, LVM_HITTEST, 0, (LPARAM)&ht);
    printf("past the columns: %d flags %#x\n", who, ht.flags);
    DestroyWindow(lv);

    /* Owner data. */
    lv = listview(main_win, LVS_OWNERDATA);
    col.pszText = L"A";
    SendMessageW(lv, LVM_INSERTCOLUMNW, 0, (LPARAM)&col);
    SendMessageW(lv, LVM_SETITEMCOUNT, 1000, 0);
    printf("owner data: count %d\n", (int)SendMessageW(lv, LVM_GETITEMCOUNT, 0, 0));
    q.iSubItem = 0;
    q.cchTextMax = 32;
    SendMessageW(lv, LVM_GETITEMTEXTW, 500, (LPARAM)&q);
    wprint("  500", cell);
    SendMessageW(lv, LVM_SETITEMSTATE, 7, (LPARAM)&st);
    SendMessageW(lv, LVM_SETITEMSTATE, 9, (LPARAM)&st);
    printf("selected %d, first %d, next %d\n", (int)SendMessageW(lv, LVM_GETSELECTEDCOUNT, 0, 0), (int)SendMessageW(lv, LVM_GETNEXTITEM, (WPARAM)-1, LVNI_SELECTED),
           (int)SendMessageW(lv, LVM_GETNEXTITEM, 7, LVNI_SELECTED));
    DestroyWindow(lv);
}

/* -----| header |----- */

static void test_header(void)
{
    printf("--- header\n");
    HWND h = CreateWindowExW(0, WC_HEADERW, L"", WS_CHILD | WS_VISIBLE | HDS_BUTTONS | HDS_HORZ, 0, 0, 300, 20, main_win, (HMENU)102, NULL, NULL);
    HDITEMW it = {0};
    it.mask = HDI_TEXT | HDI_WIDTH | HDI_LPARAM | HDI_FORMAT;
    it.pszText = L"One";
    it.cxy = 50;
    it.lParam = 11;
    printf("insert %d", (int)SendMessageW(h, HDM_INSERTITEMW, 0, (LPARAM)&it));
    it.pszText = L"Two";
    it.cxy = 70;
    it.lParam = 22;
    printf(" %d", (int)SendMessageW(h, HDM_INSERTITEMW, 1, (LPARAM)&it));
    it.pszText = L"Zero";
    it.cxy = 30;
    it.lParam = 33;
    printf(" %d\n", (int)SendMessageW(h, HDM_INSERTITEMW, 0, (LPARAM)&it));
    printf("count %d\n", (int)SendMessageW(h, HDM_GETITEMCOUNT, 0, 0));
    for (int i = 0; i < 3; i++) {
        WCHAR t[16];
        HDITEMW g = {0};
        g.mask = HDI_TEXT | HDI_WIDTH | HDI_LPARAM | HDI_ORDER;
        g.pszText = t;
        g.cchTextMax = 16;
        SendMessageW(h, HDM_GETITEMW, i, (LPARAM)&g);
        RECT r;
        SendMessageW(h, HDM_GETITEMRECT, i, (LPARAM)&r);
        printf("  %d: width %d param %d order %d spans %d..%d", i, g.cxy, (int)g.lParam, g.iOrder, r.left, r.right);
        wprint(" text", t);
    }
    int order[3] = {2, 0, 1};
    printf("set order %d\n", (int)SendMessageW(h, HDM_SETORDERARRAY, 3, (LPARAM)order));
    RECT r;
    SendMessageW(h, HDM_GETITEMRECT, 0, (LPARAM)&r);
    printf("item 0 now spans %d..%d, order 0 is item %d\n", r.left, r.right, (int)SendMessageW(h, HDM_ORDERTOINDEX, 0, 0));
    HDHITTESTINFO ht = {0};
    ht.pt.x = 5;
    ht.pt.y = 5;
    int hit = (int)SendMessageW(h, HDM_HITTEST, 0, (LPARAM)&ht);
    printf("hit at x 5: %d item %d\n", hit, ht.iItem);
    printf("delete %d, then %d items\n", (int)SendMessageW(h, HDM_DELETEITEM, 1, 0), (int)SendMessageW(h, HDM_GETITEMCOUNT, 0, 0));
    DestroyWindow(h);
}

/* -----| tree view |----- */

static HTREEITEM tv_add(HWND tv, HTREEITEM parent, HTREEITEM after, const WCHAR *name, int param)
{
    TVINSERTSTRUCTW ins = {0};
    ins.hParent = parent;
    ins.hInsertAfter = after;
    ins.item.mask = TVIF_TEXT | TVIF_PARAM;
    ins.item.pszText = (WCHAR *)name;
    ins.item.lParam = param;
    return (HTREEITEM)SendMessageW(tv, TVM_INSERTITEMW, 0, (LPARAM)&ins);
}

static int tv_param(HWND tv, HTREEITEM h)
{
    if (!h)
        return 0;
    TVITEMW it = {0};
    it.mask = TVIF_PARAM;
    it.hItem = h;
    SendMessageW(tv, TVM_GETITEMW, 0, (LPARAM)&it);
    return (int)it.lParam;
}

static HTREEITEM tv_next(HWND tv, UINT how, HTREEITEM h)
{
    return (HTREEITEM)SendMessageW(tv, TVM_GETNEXTITEM, how, (LPARAM)h);
}

static void tv_walk(HWND tv, HTREEITEM h, int depth)
{
    for (; h; h = tv_next(tv, TVGN_NEXT, h)) {
        WCHAR t[32];
        TVITEMW it = {0};
        it.mask = TVIF_TEXT | TVIF_PARAM | TVIF_CHILDREN | TVIF_STATE;
        it.hItem = h;
        it.pszText = t;
        it.cchTextMax = 32;
        it.stateMask = TVIS_EXPANDED | TVIS_SELECTED | TVIS_EXPANDEDONCE;
        SendMessageW(tv, TVM_GETITEMW, 0, (LPARAM)&it);
        char a[32];
        WideCharToMultiByte(CP_UTF8, 0, t, -1, a, 32, NULL, NULL);
        printf("%*s%s p%d children %d state %x\n", depth * 2 + 2, "", a, (int)it.lParam, it.cChildren, it.state);
        tv_walk(tv, tv_next(tv, TVGN_CHILD, h), depth + 1);
    }
}

static int CALLBACK tv_desc(LPARAM a, LPARAM b, LPARAM sort)
{
    return (int)(b - a);
}

static void test_tree(void)
{
    printf("--- tree view\n");
    HWND tv = CreateWindowExW(0, WC_TREEVIEWW, L"", WS_CHILD | WS_VISIBLE | WS_BORDER | TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT | TVS_SHOWSELALWAYS,
                              0, 0, 300, 300, main_win, (HMENU)103, NULL, NULL);
    printf("empty: count %d, root %d, caret %d\n", (int)SendMessageW(tv, TVM_GETCOUNT, 0, 0), tv_next(tv, TVGN_ROOT, NULL) != NULL, tv_next(tv, TVGN_CARET, NULL) != NULL);
    HTREEITEM a = tv_add(tv, TVI_ROOT, TVI_LAST, L"a", 1);
    HTREEITEM b = tv_add(tv, TVI_ROOT, TVI_LAST, L"b", 2);
    HTREEITEM a1 = tv_add(tv, a, TVI_LAST, L"a1", 11);
    HTREEITEM a2 = tv_add(tv, a, TVI_LAST, L"a2", 12);
    HTREEITEM a0 = tv_add(tv, a, TVI_FIRST, L"a0", 10);
    HTREEITEM a15 = tv_add(tv, a, a1, L"a15", 15);
    HTREEITEM a11 = tv_add(tv, a1, TVI_LAST, L"a11", 111);
    tv_add(tv, b, TVI_SORT, L"zed", 23);
    tv_add(tv, b, TVI_SORT, L"Mid", 22);
    tv_add(tv, b, TVI_SORT, L"abc", 21);
    printf("count %d\n", (int)SendMessageW(tv, TVM_GETCOUNT, 0, 0));
    tv_walk(tv, tv_next(tv, TVGN_ROOT, NULL), 0);
    printf("relatives of a1: parent %d, prev %d, next %d, child %d\n", tv_param(tv, tv_next(tv, TVGN_PARENT, a1)), tv_param(tv, tv_next(tv, TVGN_PREVIOUS, a1)),
           tv_param(tv, tv_next(tv, TVGN_NEXT, a1)), tv_param(tv, tv_next(tv, TVGN_CHILD, a1)));
    printf("root's parent %d, null child is the root %d\n", tv_param(tv, tv_next(tv, TVGN_PARENT, a)), tv_param(tv, tv_next(tv, TVGN_CHILD, NULL)));
    printf("visible: first %d, next of a %d, last %d\n", tv_param(tv, tv_next(tv, TVGN_FIRSTVISIBLE, NULL)), tv_param(tv, tv_next(tv, TVGN_NEXTVISIBLE, a)),
           tv_param(tv, tv_next(tv, TVGN_LASTVISIBLE, NULL)));
    RECT r;
    *(HTREEITEM *)&r = a1;
    printf("rect of hidden a1: %d\n", (int)SendMessageW(tv, TVM_GETITEMRECT, FALSE, (LPARAM)&r));

    log_notify = 1;
    printf("expand a:\n");
    printf("  = %d\n", (int)SendMessageW(tv, TVM_EXPAND, TVE_EXPAND, (LPARAM)a));
    printf("expand a again:\n");
    printf("  = %d\n", (int)SendMessageW(tv, TVM_EXPAND, TVE_EXPAND, (LPARAM)a));
    printf("collapse a:\n");
    printf("  = %d\n", (int)SendMessageW(tv, TVM_EXPAND, TVE_COLLAPSE, (LPARAM)a));
    printf("expand a after once:\n");
    printf("  = %d\n", (int)SendMessageW(tv, TVM_EXPAND, TVE_EXPAND, (LPARAM)a));
    printf("toggle b:\n");
    printf("  = %d\n", (int)SendMessageW(tv, TVM_EXPAND, TVE_TOGGLE, (LPARAM)b));
    printf("state of a: %x\n", (unsigned)SendMessageW(tv, TVM_GETITEMSTATE, (WPARAM)a, TVIS_EXPANDED | TVIS_EXPANDEDONCE));
    printf("visible after a: %d, then %d\n", tv_param(tv, tv_next(tv, TVGN_NEXTVISIBLE, a)), tv_param(tv, tv_next(tv, TVGN_NEXTVISIBLE, a0)));
    *(HTREEITEM *)&r = a1;
    printf("rect of a1: %d\n", (int)SendMessageW(tv, TVM_GETITEMRECT, FALSE, (LPARAM)&r));

    printf("select a2:\n");
    printf("  = %d\n", (int)SendMessageW(tv, TVM_SELECTITEM, TVGN_CARET, (LPARAM)a2));
    printf("caret %d\n", tv_param(tv, tv_next(tv, TVGN_CARET, NULL)));
    printf("select a2 again:\n");
    printf("  = %d\n", (int)SendMessageW(tv, TVM_SELECTITEM, TVGN_CARET, (LPARAM)a2));
    printf("select hidden a11:\n");
    printf("  = %d\n", (int)SendMessageW(tv, TVM_SELECTITEM, TVGN_CARET, (LPARAM)a11));
    printf("state of a1 (opened for it): %x\n", (unsigned)SendMessageW(tv, TVM_GETITEMSTATE, (WPARAM)a1, TVIS_EXPANDED));
    printf("collapse a1, which holds the selection:\n");
    printf("  = %d\n", (int)SendMessageW(tv, TVM_EXPAND, TVE_COLLAPSE, (LPARAM)a1));
    printf("caret %d\n", tv_param(tv, tv_next(tv, TVGN_CARET, NULL)));
    printf("delete selected a1 (with a11):\n");
    printf("  = %d\n", (int)SendMessageW(tv, TVM_DELETEITEM, 0, (LPARAM)a1));
    printf("caret %d\n", tv_param(tv, tv_next(tv, TVGN_CARET, NULL)));
    log_notify = 0;

    TVITEMW it = {0};
    it.mask = TVIF_TEXT | TVIF_PARAM | TVIF_CHILDREN;
    it.hItem = a15;
    it.pszText = L"renamed";
    it.lParam = 99;
    it.cChildren = 1;
    printf("set: %d\n", (int)SendMessageW(tv, TVM_SETITEMW, 0, (LPARAM)&it));
    it.mask = TVIF_TEXT;
    it.pszText = LPSTR_TEXTCALLBACKW;
    SendMessageW(tv, TVM_SETITEMW, 0, (LPARAM)&it);
    tv_walk(tv, tv_next(tv, TVGN_CHILD, a), 0);
    printf("sort children of a by name: %d\n", (int)SendMessageW(tv, TVM_SORTCHILDREN, 0, (LPARAM)a));
    tv_walk(tv, tv_next(tv, TVGN_CHILD, a), 0);
    TVSORTCB cb = {a, tv_desc, 0};
    printf("sort by callback: %d\n", (int)SendMessageW(tv, TVM_SORTCHILDRENCB, 0, (LPARAM)&cb));
    tv_walk(tv, tv_next(tv, TVGN_CHILD, a), 0);
    printf("indent is positive %d\n", (int)SendMessageW(tv, TVM_GETINDENT, 0, 0) > 0);
    SendMessageW(tv, TVM_SETINDENT, 30, 0);
    printf("indent %d\n", (int)SendMessageW(tv, TVM_GETINDENT, 0, 0));
    int old = (int)SendMessageW(tv, TVM_SETITEMHEIGHT, 24, 0);
    printf("height was positive %d, now %d\n", old > 0, (int)SendMessageW(tv, TVM_GETITEMHEIGHT, 0, 0));
    SendMessageW(tv, TVM_EXPAND, TVE_COLLAPSE, (LPARAM)a);
    TVHITTESTINFO ht = {0};
    ht.pt.x = 280;
    ht.pt.y = 2;
    HTREEITEM hit = (HTREEITEM)SendMessageW(tv, TVM_HITTEST, 0, (LPARAM)&ht);
    printf("hit right of the first row: %d flags %#x\n", tv_param(tv, hit), ht.flags);
    ht.pt.y = 280;
    hit = (HTREEITEM)SendMessageW(tv, TVM_HITTEST, 0, (LPARAM)&ht);
    printf("hit below the rows: %d flags %#x\n", hit != NULL, ht.flags);
    log_notify = 1;
    printf("delete all:\n");
    printf("  = %d\n", (int)SendMessageW(tv, TVM_DELETEITEM, 0, (LPARAM)TVI_ROOT));
    log_notify = 0;
    printf("count %d\n", (int)SendMessageW(tv, TVM_GETCOUNT, 0, 0));
    DestroyWindow(tv);
}

/* -----| tabs |----- */

static void test_tabs(void)
{
    printf("--- tabs\n");
    HWND t = CreateWindowExW(0, WC_TABCONTROLW, L"", WS_CHILD | WS_VISIBLE, 0, 0, 400, 200, main_win, (HMENU)104, NULL, NULL);
    printf("empty: count %d, selection %d\n", (int)SendMessageW(t, TCM_GETITEMCOUNT, 0, 0), (int)SendMessageW(t, TCM_GETCURSEL, 0, 0));
    TCITEMW it = {0};
    it.mask = TCIF_TEXT | TCIF_PARAM;
    it.pszText = L"First";
    it.lParam = 1;
    printf("insert %d", (int)SendMessageW(t, TCM_INSERTITEMW, 0, (LPARAM)&it));
    printf(", selection %d", (int)SendMessageW(t, TCM_GETCURSEL, 0, 0));
    it.pszText = L"Second";
    it.lParam = 2;
    printf(", insert %d", (int)SendMessageW(t, TCM_INSERTITEMW, 1, (LPARAM)&it));
    it.pszText = L"Zero";
    it.lParam = 3;
    printf(", insert %d", (int)SendMessageW(t, TCM_INSERTITEMW, 0, (LPARAM)&it));
    printf(", selection %d\n", (int)SendMessageW(t, TCM_GETCURSEL, 0, 0));
    for (int i = 0; i < 3; i++) {
        WCHAR b[16];
        TCITEMW g = {0};
        g.mask = TCIF_TEXT | TCIF_PARAM;
        g.pszText = b;
        g.cchTextMax = 16;
        SendMessageW(t, TCM_GETITEMW, i, (LPARAM)&g);
        printf("  %d param %d", i, (int)g.lParam);
        wprint(" text", b);
    }
    log_notify = 1;
    printf("set selection 2 (no notifications): was %d\n", (int)SendMessageW(t, TCM_SETCURSEL, 2, 0));
    printf("set focus 0 (user's way):\n");
    SendMessageW(t, TCM_SETCURFOCUS, 0, 0);
    printf("selection %d, focus %d\n", (int)SendMessageW(t, TCM_GETCURSEL, 0, 0), (int)SendMessageW(t, TCM_GETCURFOCUS, 0, 0));
    log_notify = 0;
    printf("set selection out of range: %d\n", (int)SendMessageW(t, TCM_SETCURSEL, 9, 0));
    RECT r0, r1;
    SendMessageW(t, TCM_GETITEMRECT, 0, (LPARAM)&r0);
    SendMessageW(t, TCM_GETITEMRECT, 1, (LPARAM)&r1);
    printf("tabs side by side %d, same row %d\n", r0.right == r1.left, r0.top == r1.top);
    TCHITTESTINFO ht = {0};
    ht.pt.x = r1.left + 2;
    ht.pt.y = r1.top + 2;
    printf("hit: %d", (int)SendMessageW(t, TCM_HITTEST, 0, (LPARAM)&ht));
    printf(" flags %#x", ht.flags);
    ht.pt.x = 390;
    printf(", beyond %d", (int)SendMessageW(t, TCM_HITTEST, 0, (LPARAM)&ht));
    printf(" flags %#x\n", ht.flags);
    RECT a = {0, 0, 400, 200}, g;
    g = a;
    SendMessageW(t, TCM_ADJUSTRECT, FALSE, (LPARAM)&g);
    RECT back = g;
    SendMessageW(t, TCM_ADJUSTRECT, TRUE, (LPARAM)&back);
    printf("page inside the control %d, and back again %d\n", g.left > 0 && g.top > 0 && g.right < 400 && g.bottom < 200, back.left == a.left && back.top == a.top && back.right == a.right && back.bottom == a.bottom);
    printf("delete selected 0: %d, selection %d, count %d\n", (int)SendMessageW(t, TCM_DELETEITEM, 0, 0), (int)SendMessageW(t, TCM_GETCURSEL, 0, 0), (int)SendMessageW(t, TCM_GETITEMCOUNT, 0, 0));
    printf("delete all: %d, count %d\n", (int)SendMessageW(t, TCM_DELETEALLITEMS, 0, 0), (int)SendMessageW(t, TCM_GETITEMCOUNT, 0, 0));
    DestroyWindow(t);
}

/* -----| toolbar |----- */

static void test_toolbar(void)
{
    printf("--- toolbar\n");
    HWND tb = CreateWindowExW(0, TOOLBARCLASSNAMEW, L"", WS_CHILD | WS_VISIBLE | CCS_NORESIZE | CCS_NOPARENTALIGN | TBSTYLE_FLAT, 0, 0, 400, 30, main_win, (HMENU)105, NULL, NULL);
    SendMessageW(tb, TB_BUTTONSTRUCTSIZE, sizeof(TBBUTTON), 0);
    int s0 = (int)SendMessageW(tb, TB_ADDSTRINGW, 0, (LPARAM)L"Open\0Save\0");
    int s2 = (int)SendMessageW(tb, TB_ADDSTRINGW, 0, (LPARAM)L"Print\0");
    printf("strings at %d, %d\n", s0, s2);
    TBBUTTON b[5] = {0};
    b[0].iBitmap = 0;
    b[0].idCommand = 100;
    b[0].fsState = TBSTATE_ENABLED;
    b[0].fsStyle = BTNS_BUTTON;
    b[0].iString = 0;
    b[1].fsStyle = BTNS_SEP;
    b[1].iString = -1;
    b[2].iBitmap = 1;
    b[2].idCommand = 101;
    b[2].fsState = TBSTATE_ENABLED;
    b[2].fsStyle = BTNS_CHECK;
    b[2].iString = 1;
    b[3].iBitmap = 2;
    b[3].idCommand = 102;
    b[3].fsState = TBSTATE_ENABLED;
    b[3].fsStyle = BTNS_CHECKGROUP;
    b[3].iString = -1;
    b[4].iBitmap = 3;
    b[4].idCommand = 103;
    b[4].fsState = TBSTATE_ENABLED | TBSTATE_CHECKED;
    b[4].fsStyle = BTNS_CHECKGROUP;
    b[4].iString = -1;
    printf("add: %d, count %d\n", (int)SendMessageW(tb, TB_ADDBUTTONSW, 5, (LPARAM)b), (int)SendMessageW(tb, TB_BUTTONCOUNT, 0, 0));
    printf("index of 102: %d, of 999: %d\n", (int)SendMessageW(tb, TB_COMMANDTOINDEX, 102, 0), (int)SendMessageW(tb, TB_COMMANDTOINDEX, 999, 0));
    TBBUTTON g;
    SendMessageW(tb, TB_GETBUTTON, 2, (LPARAM)&g);
    printf("button 2: bitmap %d id %d state %x style %x string %d\n", g.iBitmap, g.idCommand, g.fsState, g.fsStyle, (int)g.iString);
    WCHAR t[32] = {0};
    int n = (int)SendMessageW(tb, TB_GETBUTTONTEXTW, 101, (LPARAM)t);
    printf("text of 101: %d", n);
    wprint("", t);
    printf("states: enabled %d, checked 103 %d, checked 101 %d, hidden %d, missing %d\n", (int)SendMessageW(tb, TB_ISBUTTONENABLED, 100, 0), (int)SendMessageW(tb, TB_ISBUTTONCHECKED, 103, 0),
           (int)SendMessageW(tb, TB_ISBUTTONCHECKED, 101, 0), (int)SendMessageW(tb, TB_ISBUTTONHIDDEN, 100, 0), (int)SendMessageW(tb, TB_ISBUTTONENABLED, 999, 0));
    SendMessageW(tb, TB_ENABLEBUTTON, 100, FALSE);
    SendMessageW(tb, TB_CHECKBUTTON, 101, TRUE);
    printf("state of 100 %x, 101 %x, is enabled %d\n", (unsigned)SendMessageW(tb, TB_GETSTATE, 100, 0), (unsigned)SendMessageW(tb, TB_GETSTATE, 101, 0), (int)SendMessageW(tb, TB_ISBUTTONENABLED, 100, 0));
    SendMessageW(tb, TB_SETSTATE, 100, TBSTATE_ENABLED);
    printf("set state %x\n", (unsigned)SendMessageW(tb, TB_GETSTATE, 100, 0));
    TBBUTTONINFOW bi = {sizeof bi};
    bi.dwMask = TBIF_IMAGE | TBIF_STATE | TBIF_STYLE | TBIF_COMMAND | TBIF_LPARAM;
    int at = (int)SendMessageW(tb, TB_GETBUTTONINFOW, 102, (LPARAM)&bi);
    printf("info of 102: index %d command %d image %d state %x style %x\n", at, bi.idCommand, bi.iImage, bi.fsState, bi.fsStyle);
    bi.dwMask = TBIF_TEXT | TBIF_COMMAND;
    bi.pszText = L"Renamed";
    bi.idCommand = 202;
    printf("set info: %d, index of 202 %d\n", (int)SendMessageW(tb, TB_SETBUTTONINFOW, 102, (LPARAM)&bi), (int)SendMessageW(tb, TB_COMMANDTOINDEX, 202, 0));
    n = (int)SendMessageW(tb, TB_GETBUTTONTEXTW, 202, (LPARAM)t);
    printf("text of 202: %d", n);
    wprint("", t);
    RECT r0, r2;
    SendMessageW(tb, TB_GETITEMRECT, 0, (LPARAM)&r0);
    SendMessageW(tb, TB_GETITEMRECT, 2, (LPARAM)&r2);
    printf("buttons left to right %d\n", r0.right <= r2.left);
    POINT p = {r2.left + 2, r2.top + 2};
    printf("hit test %d\n", (int)SendMessageW(tb, TB_HITTEST, 0, (LPARAM)&p));
    SendMessageW(tb, TB_HIDEBUTTON, 100, TRUE);
    printf("hidden %d\n", (int)SendMessageW(tb, TB_ISBUTTONHIDDEN, 100, 0));
    TBBUTTON ins = {0};
    ins.idCommand = 300;
    ins.fsState = TBSTATE_ENABLED;
    ins.iString = -1;
    printf("insert: %d, index of 300 %d\n", (int)SendMessageW(tb, TB_INSERTBUTTONW, 1, (LPARAM)&ins), (int)SendMessageW(tb, TB_COMMANDTOINDEX, 300, 0));
    printf("delete: %d, count %d\n", (int)SendMessageW(tb, TB_DELETEBUTTON, 0, 0), (int)SendMessageW(tb, TB_BUTTONCOUNT, 0, 0));
    SendMessageW(tb, TB_SETEXTENDEDSTYLE, 0, TBSTYLE_EX_DRAWDDARROWS);
    printf("extended %#x\n", (unsigned)SendMessageW(tb, TB_GETEXTENDEDSTYLE, 0, 0));
    log_notify = 1;
    printf("a click on 300:\n");
    SendMessageW(tb, TB_GETITEMRECT, 0, (LPARAM)&r0);
    LPARAM at_btn = MAKELPARAM(r0.left + 2, r0.top + 2);
    SendMessageW(tb, WM_LBUTTONDOWN, MK_LBUTTON, at_btn);
    SendMessageW(tb, WM_LBUTTONUP, 0, at_btn);
    log_notify = 0;
    DestroyWindow(tb);
}

/* -----| property sheets |----- */

static DWORD page_tmpl[2][32];
static int auto_mode;
static HWND sheet_of(HWND page) { return GetParent(page); }

static int psn_stay_on;

static INT_PTR CALLBACK page_proc(HWND d, UINT m, WPARAM w, LPARAM l)
{
    if (m == WM_INITDIALOG) {
        PROPSHEETPAGEW *p = (PROPSHEETPAGEW *)l;
        printf("  page %d WM_INITDIALOG\n", (int)p->lParam);
        SetWindowLongPtrW(d, DWLP_USER, p->lParam);
        return TRUE;
    }
    if (m == WM_NOTIFY) {
        NMHDR *n = (NMHDR *)l;
        int id = (int)GetWindowLongPtrW(d, DWLP_USER);
        const char *name = NULL;
        switch (n->code) {
        case PSN_SETACTIVE: name = "PSN_SETACTIVE"; break;
        case PSN_KILLACTIVE: name = "PSN_KILLACTIVE"; break;
        case PSN_APPLY: name = "PSN_APPLY"; break;
        case PSN_RESET: name = "PSN_RESET"; break;
        case PSN_WIZNEXT: name = "PSN_WIZNEXT"; break;
        case PSN_WIZBACK: name = "PSN_WIZBACK"; break;
        case PSN_WIZFINISH: name = "PSN_WIZFINISH"; break;
        case PSN_QUERYCANCEL: name = "PSN_QUERYCANCEL"; break;
        }
        if (!name)
            return FALSE;
        printf("  page %d %s lparam %d\n", id, name, n->code == PSN_APPLY ? (int)((PSHNOTIFY *)l)->lParam : 0);
        if (n->code == PSN_SETACTIVE && auto_mode == 2) {
            int last = id == 2;
            PropSheet_SetWizButtons(sheet_of(d), id == 1 ? PSWIZB_NEXT : last ? (PSWIZB_BACK | PSWIZB_FINISH) : (PSWIZB_BACK | PSWIZB_NEXT));
        }
        if (n->code == PSN_KILLACTIVE && psn_stay_on == id) {
            psn_stay_on = 0;
            SetWindowLongPtrW(d, DWLP_MSGRESULT, TRUE);
            return TRUE;
        }
        SetWindowLongPtrW(d, DWLP_MSGRESULT, 0);
        return TRUE;
    }
    return FALSE;
}

static int CALLBACK sheet_cb(HWND h, UINT m, LPARAM l)
{
    if (m == PSCB_INITIALIZED) {
        printf("  callback PSCB_INITIALIZED\n");
        if (auto_mode)
            PostMessageW(main_win, WM_APP + auto_mode, (WPARAM)h, 0);
    }
    return 0;
}

static void make_templates(void)
{
    for (int i = 0; i < 2; i++) {
        DLGTEMPLATE *t = (DLGTEMPLATE *)page_tmpl[i];
        t->style = WS_CHILD | DS_SETFONT;
        t->cx = 200;
        t->cy = 120;
        WORD *w = (WORD *)(t + 1);
        *w++ = 0;
        *w++ = 0;
        *w++ = 0;
        *w++ = 8;
        for (const WCHAR *f = L"MS Shell Dlg"; *f; f++)
            *w++ = *f;
        *w++ = 0;
    }
}

static void sheet_page(PROPSHEETPAGEW *p, int id, const WCHAR *title)
{
    memset(p, 0, sizeof *p);
    p->dwSize = sizeof *p;
    p->dwFlags = PSP_DLGINDIRECT | PSP_USETITLE;
    p->pResource = (DLGTEMPLATE *)page_tmpl[id - 1];
    p->pszTitle = title;
    p->pfnDlgProc = page_proc;
    p->lParam = id;
}

static void pump_once(void)
{
    MSG m;
    while (PeekMessageW(&m, NULL, 0, 0, PM_REMOVE)) {
        TranslateMessage(&m);
        DispatchMessageW(&m);
    }
}

/* Drives the modal sheets, from the owner's window procedure. */
static WNDPROC plain_win_proc;

static LRESULT CALLBACK sheet_driver(HWND h, UINT m, WPARAM w, LPARAM l)
{
    HWND s = (HWND)w;
    if (m == WM_APP + 1) {
        printf("tab control %d, buttons OK %d Cancel %d Apply %d\n", PropSheet_GetTabControl(s) != NULL, GetDlgItem(s, IDOK) != NULL, GetDlgItem(s, IDCANCEL) != NULL, GetDlgItem(s, 0x3021) != NULL);
        printf("apply enabled %d\n", IsWindowEnabled(GetDlgItem(s, 0x3021)));
        HWND cur = PropSheet_GetCurrentPageHwnd(s);
        printf("current page %d, index %d\n", (int)GetWindowLongPtrW(cur, DWLP_USER), (int)PropSheet_HwndToIndex(s, cur));
        printf("page 1 hwnd known %d\n", PropSheet_IndexToHwnd(s, 1) != NULL);
        PropSheet_Changed(s, cur);
        printf("after PSM_CHANGED apply enabled %d\n", IsWindowEnabled(GetDlgItem(s, 0x3021)));
        printf("go to page 1:\n");
        printf("  = %d\n", (int)PropSheet_SetCurSel(s, NULL, 1));
        cur = PropSheet_GetCurrentPageHwnd(s);
        printf("current page %d, visible %d\n", (int)GetWindowLongPtrW(cur, DWLP_USER), IsWindowVisible(cur));
        psn_stay_on = 2;
        printf("page 2 refuses to leave:\n");
        printf("  = %d\n", (int)PropSheet_SetCurSel(s, NULL, 0));
        printf("still page %d\n", (int)GetWindowLongPtrW(PropSheet_GetCurrentPageHwnd(s), DWLP_USER));
        printf("apply:\n");
        SendMessageW(s, WM_COMMAND, 0x3021, (LPARAM)GetDlgItem(s, 0x3021));
        printf("apply enabled %d\n", IsWindowEnabled(GetDlgItem(s, 0x3021)));
        printf("OK:\n");
        PropSheet_PressButton(s, PSBTN_OK);
        return 0;
    }
    if (m == WM_APP + 2) {
        printf("wizard buttons: back %d (%d) next %d (%d) finish %d, no tabs %d\n", GetDlgItem(s, 0x3023) != NULL, IsWindowEnabled(GetDlgItem(s, 0x3023)), GetDlgItem(s, 0x3024) != NULL, IsWindowEnabled(GetDlgItem(s, 0x3024)),
               GetDlgItem(s, 0x3025) != NULL, PropSheet_GetTabControl(s) == NULL);
        printf("next:\n");
        PropSheet_PressButton(s, PSBTN_NEXT);
        printf("page %d, back enabled %d\n", (int)GetWindowLongPtrW(PropSheet_GetCurrentPageHwnd(s), DWLP_USER), IsWindowEnabled(GetDlgItem(s, 0x3023)));
        printf("back:\n");
        PropSheet_PressButton(s, PSBTN_BACK);
        printf("page %d\n", (int)GetWindowLongPtrW(PropSheet_GetCurrentPageHwnd(s), DWLP_USER));
        printf("next:\n");
        PropSheet_PressButton(s, PSBTN_NEXT);
        printf("finish:\n");
        PropSheet_PressButton(s, PSBTN_FINISH);
        return 0;
    }
    if (m == WM_APP + 3) {
        printf("cancel:\n");
        PropSheet_PressButton(s, PSBTN_CANCEL);
        return 0;
    }
    return CallWindowProcW(plain_win_proc, h, m, w, l);
}

static void test_sheets(void)
{
    printf("--- property sheets\n");
    make_templates();
    plain_win_proc = (WNDPROC)SetWindowLongPtrW(main_win, GWLP_WNDPROC, (LONG_PTR)sheet_driver);
    PROPSHEETPAGEW pages[2];
    sheet_page(&pages[0], 1, L"General");
    sheet_page(&pages[1], 2, L"Details");
    PROPSHEETHEADERW h = {0};
    h.dwSize = sizeof h;
    h.dwFlags = PSH_PROPSHEETPAGE | PSH_USECALLBACK;
    h.hwndParent = main_win;
    h.pszCaption = L"Test sheet";
    h.nPages = 2;
    h.ppsp = pages;
    h.pfnCallback = sheet_cb;
    auto_mode = 1;
    INT_PTR r = PropertySheetW(&h);
    printf("PropertySheet returned %d\n", (int)r);
    auto_mode = 3;
    r = PropertySheetW(&h);
    printf("cancelled: returned %d\n", (int)r);

    printf("handles:\n");
    HPROPSHEETPAGE hp[2] = {CreatePropertySheetPageW(&pages[0]), CreatePropertySheetPageW(&pages[1])};
    printf("made %d %d\n", hp[0] != NULL, hp[1] != NULL);
    h.dwFlags = PSH_USECALLBACK | PSH_WIZARD;
    h.phpage = hp;
    auto_mode = 2;
    r = PropertySheetW(&h);
    printf("wizard returned %d\n", (int)r);
    auto_mode = 0;
    SetWindowLongPtrW(main_win, GWLP_WNDPROC, (LONG_PTR)plain_win_proc);
    pump_once();
}

/* -----| DPA and DSA |----- */

static int CALLBACK dpa_cmp(void *a, void *b, LPARAM l)
{
    return (int)((INT_PTR)a - (INT_PTR)b) * (int)l;
}

static int CALLBACK dpa_each(void *p, void *data)
{
    (*(int *)data) += (int)(INT_PTR)p;
    return 1;
}

static void test_arrays(void)
{
    printf("--- arrays\n");
    HDPA d = DPA_Create(4);
    printf("insert %d %d %d %d\n", DPA_InsertPtr(d, 0, (void *)30), DPA_InsertPtr(d, DA_LAST, (void *)10), DPA_InsertPtr(d, 1, (void *)20), DPA_InsertPtr(d, 99, (void *)40));
    printf("get %d %d %d, past the end %d\n", (int)(INT_PTR)DPA_GetPtr(d, 0), (int)(INT_PTR)DPA_GetPtr(d, 1), (int)(INT_PTR)DPA_GetPtr(d, 3), (int)(INT_PTR)DPA_GetPtr(d, 9));
    printf("index of 20: %d, of 7: %d\n", DPA_GetPtrIndex(d, (void *)20), DPA_GetPtrIndex(d, (void *)7));
    DPA_Sort(d, dpa_cmp, 1);
    printf("sorted: %d %d %d %d\n", (int)(INT_PTR)DPA_GetPtr(d, 0), (int)(INT_PTR)DPA_GetPtr(d, 1), (int)(INT_PTR)DPA_GetPtr(d, 2), (int)(INT_PTR)DPA_GetPtr(d, 3));
    printf("search sorted 30: %d, missing 25: %d, where 25 goes: %d\n", DPA_Search(d, (void *)30, 0, dpa_cmp, 1, DPAS_SORTED), DPA_Search(d, (void *)25, 0, dpa_cmp, 1, DPAS_SORTED),
           DPA_Search(d, (void *)25, 0, dpa_cmp, 1, DPAS_SORTED | DPAS_INSERTBEFORE));
    printf("set 6: %d\n", DPA_SetPtr(d, 6, (void *)60));
    printf("deleted %d, then at 0: %d\n", (int)(INT_PTR)DPA_DeletePtr(d, 0), (int)(INT_PTR)DPA_GetPtr(d, 0));
    int sum = 0;
    DPA_EnumCallback(d, dpa_each, &sum);
    printf("sum %d\n", sum);
    HDPA c = DPA_Clone(d, NULL);
    DPA_DeleteAllPtrs(d);
    printf("clone keeps %d, original %d\n", (int)(INT_PTR)DPA_GetPtr(c, 0), (int)(INT_PTR)DPA_GetPtr(d, 0));
    DPA_Destroy(c);
    DPA_Destroy(d);

    HDSA s = DSA_Create(sizeof(RECT), 2);
    RECT r = {1, 2, 3, 4};
    printf("dsa insert %d", DSA_InsertItem(s, 0, &r));
    r.left = 9;
    printf(" %d\n", DSA_InsertItem(s, DA_LAST, &r));
    RECT o = {0};
    printf("get %d: %d", DSA_GetItem(s, 1, &o), o.left);
    printf(", ptr %d, past the end %d\n", ((RECT *)DSA_GetItemPtr(s, 0))->left, DSA_GetItemPtr(s, 5) == NULL);
    r.left = 7;
    printf("set %d, delete %d, now first %d\n", DSA_SetItem(s, 0, &r), DSA_DeleteItem(s, 1), ((RECT *)DSA_GetItemPtr(s, 0))->left);
    DSA_DeleteAllItems(s);
    printf("after delete all, first is null %d\n", DSA_GetItemPtr(s, 0) == NULL);
    DSA_Destroy(s);

    LPWSTR str = NULL;
    printf("Str_SetPtr %d", Str_SetPtrW(&str, L"text"));
    wprint(" ", str);
    printf("clear %d, null %d\n", Str_SetPtrW(&str, NULL), str == NULL);
}

/* -----| rich edit |----- */

static DWORD CALLBACK out_cb(DWORD_PTR cookie, LPBYTE b, LONG n, LONG *done)
{
    char *dst = (char *)cookie;
    size_t at = strlen(dst);
    memcpy(dst + at, b, n);
    dst[at + n] = 0;
    *done = n;
    return 0;
}

static DWORD CALLBACK in_cb(DWORD_PTR cookie, LPBYTE b, LONG n, LONG *done)
{
    const char **src = (const char **)cookie;
    LONG k = (LONG)strlen(*src);
    if (k > n)
        k = n;
    memcpy(b, *src, k);
    *src += k;
    *done = k;
    return 0;
}

static void test_rich(void)
{
    printf("--- rich edit\n");
    HMODULE m = LoadLibraryW(L"msftedit.dll");
    printf("msftedit loaded %d\n", m != NULL);
    HWND e = CreateWindowExW(0, MSFTEDIT_CLASS, L"Hello rich world", WS_CHILD | WS_VISIBLE | ES_MULTILINE, 0, 0, 300, 100, main_win, (HMENU)106, NULL, NULL);
    printf("created %d\n", e != NULL);
    WCHAR t[64] = {0};
    GetWindowTextW(e, t, 64);
    wprint("text", t);
    CHARRANGE cr = {6, 10};
    printf("exsetsel %d\n", (int)SendMessageW(e, EM_EXSETSEL, 0, (LPARAM)&cr));
    CHARRANGE g = {0};
    SendMessageW(e, EM_EXGETSEL, 0, (LPARAM)&g);
    printf("exgetsel %d..%d, type %d\n", g.cpMin, g.cpMax, (int)SendMessageW(e, EM_SELECTIONTYPE, 0, 0));
    int n = (int)SendMessageW(e, EM_GETSELTEXT, 0, (LPARAM)t);
    printf("sel text %d", n);
    wprint("", t);
    TEXTRANGEW tr;
    tr.chrg.cpMin = 0;
    tr.chrg.cpMax = 5;
    tr.lpstrText = t;
    n = (int)SendMessageW(e, EM_GETTEXTRANGE, 0, (LPARAM)&tr);
    printf("range %d", n);
    wprint("", t);
    FINDTEXTEXW f = {0};
    f.chrg.cpMin = 0;
    f.chrg.cpMax = -1;
    f.lpstrText = L"WORLD";
    n = (int)SendMessageW(e, EM_FINDTEXTEXW, FR_DOWN, (LPARAM)&f);
    printf("find %d at %d..%d", n, f.chrgText.cpMin, f.chrgText.cpMax);
    n = (int)SendMessageW(e, EM_FINDTEXTEXW, FR_DOWN | FR_MATCHCASE, (LPARAM)&f);
    printf(", matching case %d\n", n);
    GETTEXTLENGTHEX gl = {GTL_DEFAULT, 1200};
    printf("length %d\n", (int)SendMessageW(e, EM_GETTEXTLENGTHEX, (WPARAM)&gl, 0));
    GETTEXTEX ge = {sizeof t, GT_DEFAULT, 1200, NULL, NULL};
    n = (int)SendMessageW(e, EM_GETTEXTEX, (WPARAM)&ge, (LPARAM)t);
    printf("gettextex %d", n);
    wprint("", t);
    SETTEXTEX se = {ST_DEFAULT, 1200};
    printf("settextex %d\n", (int)SendMessageW(e, EM_SETTEXTEX, (WPARAM)&se, (LPARAM)L"Replaced"));
    GetWindowTextW(e, t, 64);
    wprint("text", t);
    static char outbuf[128];
    EDITSTREAM es = {(DWORD_PTR)outbuf, 0, out_cb};
    n = (int)SendMessageW(e, EM_STREAMOUT, SF_TEXT, (LPARAM)&es);
    printf("stream out %d \"%s\" error %u\n", n, outbuf, es.dwError);
    const char *src = "From a stream";
    EDITSTREAM ei = {(DWORD_PTR)&src, 0, in_cb};
    n = (int)SendMessageW(e, EM_STREAMIN, SF_TEXT, (LPARAM)&ei);
    GetWindowTextW(e, t, 64);
    printf("stream in %d", n);
    wprint("", t);
    DestroyWindow(e);
}

/* -----| drawing helpers |----- */

static void test_draw(void)
{
    printf("--- edges\n");
    HDC dc = CreateCompatibleDC(NULL);
    HBITMAP bm = CreateCompatibleBitmap(GetDC(NULL), 16, 16);
    HGDIOBJ old = SelectObject(dc, bm);
    RECT r = {2, 2, 14, 14};
    printf("DrawEdge %d, unchanged %d..%d\n", DrawEdge(dc, &r, EDGE_SUNKEN, BF_RECT), r.left, r.right);
    printf("adjusted %d", DrawEdge(dc, &r, EDGE_SUNKEN, BF_RECT | BF_ADJUST));
    printf(" to %d %d %d %d\n", r.left, r.top, r.right, r.bottom);
    RECT l = {0, 0, 16, 16};
    DrawEdge(dc, &l, BDR_RAISEDINNER, BF_LEFT | BF_ADJUST);
    printf("left only: %d %d %d %d\n", l.left, l.top, l.right, l.bottom);
    RECT fc = {0, 0, 16, 16};
    printf("DrawFrameControl %d\n", DrawFrameControl(dc, &fc, DFC_BUTTON, DFCS_BUTTONCHECK | DFCS_CHECKED));
    SelectObject(dc, old);
    DeleteObject(bm);
    DeleteDC(dc);
    HDWP dwp = BeginDeferWindowPos(1);
    printf("deferred %d\n", dwp != NULL);
    HWND c = CreateWindowExW(0, L"STATIC", L"", WS_CHILD, 1, 2, 3, 4, main_win, NULL, NULL, NULL);
    dwp = DeferWindowPos(dwp, c, NULL, 10, 20, 30, 40, SWP_NOZORDER);
    RECT w;
    GetClientRect(c, &w);
    printf("before the end %d x %d\n", w.right, w.bottom);
    printf("end %d\n", EndDeferWindowPos(dwp));
    GetClientRect(c, &w);
    printf("after %d x %d\n", w.right, w.bottom);
    DestroyWindow(c);
    printf("notify format of a Unicode window: %d\n", (int)DefWindowProcW(main_win, WM_NOTIFYFORMAT, 0, NF_QUERY));
}

/* A window with them all, to look at. */
static void showcase(void)
{
    HWND tv = CreateWindowExW(0, WC_TREEVIEWW, L"", WS_CHILD | WS_VISIBLE | WS_BORDER | TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT | TVS_SHOWSELALWAYS, 4, 34, 180, 240, main_win, (HMENU)201, NULL, NULL);
    HTREEITEM a = tv_add(tv, TVI_ROOT, TVI_LAST, L"Computer", 1);
    HTREEITEM d = tv_add(tv, a, TVI_LAST, L"Disks", 2);
    tv_add(tv, d, TVI_LAST, L"C:", 3);
    tv_add(tv, d, TVI_LAST, L"D:", 4);
    HTREEITEM n = tv_add(tv, a, TVI_LAST, L"Network", 5);
    tv_add(tv, n, TVI_LAST, L"Adapter", 6);
    tv_add(tv, TVI_ROOT, TVI_LAST, L"Settings", 7);
    SendMessageW(tv, TVM_EXPAND, TVE_EXPAND, (LPARAM)a);
    SendMessageW(tv, TVM_EXPAND, TVE_EXPAND, (LPARAM)d);
    SendMessageW(tv, TVM_SELECTITEM, TVGN_CARET, (LPARAM)d);

    HWND lv = CreateWindowExW(0, WC_LISTVIEWW, L"", WS_CHILD | WS_VISIBLE | WS_BORDER | LVS_REPORT | LVS_SHOWSELALWAYS, 190, 34, 340, 240, main_win, (HMENU)202, NULL, NULL);
    SendMessageW(lv, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, LVS_EX_FULLROWSELECT | LVS_EX_GRIDLINES);
    LVCOLUMNW col = {0};
    col.mask = LVCF_TEXT | LVCF_WIDTH;
    col.pszText = L"Name";
    col.cx = 140;
    SendMessageW(lv, LVM_INSERTCOLUMNW, 0, (LPARAM)&col);
    col.pszText = L"Type";
    col.cx = 90;
    SendMessageW(lv, LVM_INSERTCOLUMNW, 1, (LPARAM)&col);
    col.pszText = L"Size";
    col.cx = 70;
    SendMessageW(lv, LVM_INSERTCOLUMNW, 2, (LPARAM)&col);
    const WCHAR *rows[][3] = {{L"kernel32.dll", L"Library", L"212 KB"}, {L"notepad.exe", L"Program", L"352 KB"}, {L"readme.md", L"Document", L"1 KB"}, {L"system", L"Folder", L""}};
    for (int i = 0; i < 4; i++) {
        LVITEMW it = {0};
        it.mask = LVIF_TEXT;
        it.iItem = i;
        it.pszText = (WCHAR *)rows[i][0];
        SendMessageW(lv, LVM_INSERTITEMW, 0, (LPARAM)&it);
        for (int k = 1; k < 3; k++) {
            it.iSubItem = k;
            it.pszText = (WCHAR *)rows[i][k];
            SendMessageW(lv, LVM_SETITEMTEXTW, i, (LPARAM)&it);
        }
    }
    LVITEMW st = {0};
    st.state = st.stateMask = LVIS_SELECTED | LVIS_FOCUSED;
    SendMessageW(lv, LVM_SETITEMSTATE, 1, (LPARAM)&st);

    HWND tb = CreateWindowExW(0, TOOLBARCLASSNAMEW, L"", WS_CHILD | WS_VISIBLE | TBSTYLE_FLAT | TBSTYLE_LIST, 0, 0, 0, 0, main_win, (HMENU)203, NULL, NULL);
    SendMessageW(tb, TB_BUTTONSTRUCTSIZE, sizeof(TBBUTTON), 0);
    SendMessageW(tb, TB_SETBITMAPSIZE, 0, MAKELPARAM(0, 0));
    SendMessageW(tb, TB_ADDSTRINGW, 0, (LPARAM)L"Back\0Forward\0Refresh\0");
    TBBUTTON b[4] = {0};
    for (int i = 0; i < 4; i++) {
        b[i].iBitmap = I_IMAGENONE;
        b[i].idCommand = 300 + i;
        b[i].fsState = TBSTATE_ENABLED;
        b[i].fsStyle = i == 2 ? BTNS_SEP : BTNS_AUTOSIZE;
        b[i].iString = i < 2 ? i : 2;
    }
    b[1].fsState = TBSTATE_ENABLED | TBSTATE_CHECKED;
    b[1].fsStyle = BTNS_CHECK | BTNS_AUTOSIZE;
    SendMessageW(tb, TB_ADDBUTTONSW, 4, (LPARAM)b);
    SendMessageW(tb, TB_AUTOSIZE, 0, 0);

    HWND t = CreateWindowExW(0, WC_TABCONTROLW, L"", WS_CHILD | WS_VISIBLE, 4, 280, 526, 90, main_win, (HMENU)204, NULL, NULL);
    TCITEMW ti = {0};
    ti.mask = TCIF_TEXT;
    ti.pszText = L"General";
    SendMessageW(t, TCM_INSERTITEMW, 0, (LPARAM)&ti);
    ti.pszText = L"Sharing";
    SendMessageW(t, TCM_INSERTITEMW, 1, (LPARAM)&ti);
    ti.pszText = L"Security";
    SendMessageW(t, TCM_INSERTITEMW, 2, (LPARAM)&ti);
    SendMessageW(t, TCM_SETCURSEL, 1, 0);
    HFONT font = (HFONT)GetStockObject(DEFAULT_GUI_FONT);
    SendMessageW(t, WM_SETFONT, (WPARAM)font, 0);
    SendMessageW(tv, WM_SETFONT, (WPARAM)font, 0);
    SendMessageW(lv, WM_SETFONT, (WPARAM)font, 0);
    SendMessageW(tb, WM_SETFONT, (WPARAM)font, 0);
    ShowWindow(main_win, SW_SHOW);
}

int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    INITCOMMONCONTROLSEX icc = {sizeof icc, ICC_WIN95_CLASSES | ICC_TAB_CLASSES | ICC_BAR_CLASSES | ICC_LISTVIEW_CLASSES | ICC_TREEVIEW_CLASSES};
    printf("InitCommonControlsEx: %d\n", InitCommonControlsEx(&icc));
    WNDCLASSW wc = {0};
    wc.lpfnWndProc = win_proc;
    wc.hInstance = GetModuleHandleW(NULL);
    wc.lpszClassName = L"W13Main";
    wc.hbrBackground = (HBRUSH)(COLOR_BTNFACE + 1);
    RegisterClassW(&wc);
    main_win = CreateWindowExW(0, L"W13Main", L"w13 controls", WS_OVERLAPPEDWINDOW, 20, 20, 560, 420, NULL, NULL, wc.hInstance, NULL);

    if (argc > 1 && strcmp(argv[1], "wait") == 0) {
        showcase();
        MSG m;
        while (GetMessageW(&m, NULL, 0, 0) > 0) {
            TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        return 0;
    }
    test_listview();
    test_header();
    test_tree();
    test_tabs();
    test_toolbar();
    test_sheets();
    test_arrays();
    test_rich();
    test_draw();
    DestroyWindow(main_win);
    return 0;
}
