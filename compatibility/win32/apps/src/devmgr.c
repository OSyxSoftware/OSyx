/* OSyx Device Manager (devmgr.exe): the devices present in the computer,
 * grouped by class in an expandable tree, with the selected device's
 * properties (description, class, manufacturer, location, driver service,
 * hardware id). Uses the documented SetupAPI enumeration
 * (SetupDiGetClassDevs / SetupDiEnumDeviceInfo /
 * SetupDiGetDeviceRegistryProperty), so it runs on Windows and on OSyx
 * (winrun), where the devices come from the OSyx kernel (PCI functions,
 * USB devices). No C runtime. */
#include "app.h"
#include <setupapi.h>

#define MAX_DEV 512
#define MAX_CLASS 48

typedef struct {
    WCHAR desc[128], cls[64], mfg[96], location[128], service[64], hwid[128];
} Dev;

typedef struct {
    WCHAR name[64];
    int open;
    int count;
} Class;

static Dev devs[MAX_DEV];
static int ndev;
static Class classes[MAX_CLASS];
static int nclass;
static int sel_dev = -1, top_row;
static HFONT f_ui, f_bold, f_big;

/* One visible row of the tree: a class (dev = -1) or a device. */
typedef struct {
    int cls, dev;
} Row;
static Row rows[MAX_DEV + MAX_CLASS];
static int nrows;

#define ROW_H 26
#define TREE_TOP 70

static void prop(HDEVINFO set, SP_DEVINFO_DATA *d, DWORD p, WCHAR *out, int cap) {
    DWORD type, need = 0;
    out[0] = 0;
    if (!SetupDiGetDeviceRegistryPropertyW(set, d, p, &type, (BYTE *)out, (DWORD)(cap * sizeof(WCHAR)), &need)) out[0] = 0;
    out[cap - 1] = 0;
}

static int wieq(const WCHAR *a, const WCHAR *b) {
    for (;; a++, b++) {
        WCHAR x = *a >= L'A' && *a <= L'Z' ? *a + 32 : *a;
        WCHAR y = *b >= L'A' && *b <= L'Z' ? *b + 32 : *b;
        if (x != y) return 0;
        if (!x) return 1;
    }
}

static int wless(const WCHAR *a, const WCHAR *b) {
    for (;; a++, b++) {
        WCHAR x = *a >= L'A' && *a <= L'Z' ? *a + 32 : *a;
        WCHAR y = *b >= L'A' && *b <= L'Z' ? *b + 32 : *b;
        if (x != y) return x < y;
        if (!x) return 0;
    }
}

static void build_rows(void) {
    nrows = 0;
    for (int c = 0; c < nclass; c++) {
        rows[nrows].cls = c;
        rows[nrows++].dev = -1;
        if (!classes[c].open) continue;
        for (int i = 0; i < ndev; i++) {
            if (wieq(devs[i].cls, classes[c].name)) {
                rows[nrows].cls = c;
                rows[nrows++].dev = i;
            }
        }
    }
}

static void scan(void) {
    ndev = nclass = 0;
    HDEVINFO set = SetupDiGetClassDevsW(NULL, NULL, NULL, DIGCF_ALLCLASSES | DIGCF_PRESENT);
    if (set == INVALID_HANDLE_VALUE) return;
    SP_DEVINFO_DATA d;
    d.cbSize = sizeof d;
    for (DWORD i = 0; ndev < MAX_DEV && SetupDiEnumDeviceInfo(set, i, &d); i++) {
        Dev *v = &devs[ndev];
        prop(set, &d, SPDRP_FRIENDLYNAME, v->desc, 128);
        if (!v->desc[0]) prop(set, &d, SPDRP_DEVICEDESC, v->desc, 128);
        prop(set, &d, SPDRP_CLASS, v->cls, 64);
        prop(set, &d, SPDRP_MFG, v->mfg, 96);
        prop(set, &d, SPDRP_LOCATION_INFORMATION, v->location, 128);
        prop(set, &d, SPDRP_SERVICE, v->service, 64);
        prop(set, &d, SPDRP_HARDWAREID, v->hwid, 128);
        if (!v->desc[0]) continue;
        if (!v->cls[0]) wcopy(v->cls, L"Other devices", 64);
        ndev++;
        int c = 0;
        while (c < nclass && !wieq(classes[c].name, v->cls)) c++;
        if (c == nclass && nclass < MAX_CLASS) {
            /* Insert sorted by name. */
            int at = nclass;
            while (at > 0 && wless(v->cls, classes[at - 1].name)) at--;
            memmove(&classes[at + 1], &classes[at], (nclass - at) * sizeof(Class));
            memset(&classes[at], 0, sizeof(Class));
            wcopy(classes[at].name, v->cls, 64);
            nclass++;
            c = at;
        }
        classes[c].count++;
    }
    SetupDiDestroyDeviceInfoList(set);
    build_rows();
}

static void paint(HWND hwnd) {
    PAINTSTRUCT ps;
    HDC dc = BeginPaint(hwnd, &ps);
    RECT r;
    GetClientRect(hwnd, &r);
    int w = r.right, h = r.bottom, split = w * 55 / 100;
    fill(dc, 0, 0, w, h, C_BG);
    SelectObject(dc, f_big);
    text(dc, 20, 14, L"Device Manager", C_TEXT);
    SelectObject(dc, f_ui);
    WCHAR sum[64];
    wuint(sum, ndev);
    wcat(sum, L" devices in ", 64);
    wuint(sum + wlen(sum), nclass);
    wcat(sum, L" classes  ·  F5 to rescan", 64);
    text(dc, 22, 46, sum, C_DIM);
    /* Tree. */
    fill(dc, 10, TREE_TOP, split - 20, h - TREE_TOP - 10, C_PANEL);
    int vis = (h - TREE_TOP - 20) / ROW_H;
    for (int i = 0; i < vis && top_row + i < nrows; i++) {
        Row *rw = &rows[top_row + i];
        int y = TREE_TOP + 6 + i * ROW_H;
        if (rw->dev < 0) {
            Class *c = &classes[rw->cls];
            text(dc, 22, y + 4, c->open ? L"▾" : L"▸", C_DIM);
            SelectObject(dc, f_bold);
            text(dc, 40, y + 4, c->name, C_TEXT);
            SelectObject(dc, f_ui);
            WCHAR n[16];
            wuint(n, c->count);
            text_right(dc, split - 30, y + 4, n, C_FAINT);
        } else {
            if (rw->dev == sel_dev) fill(dc, 14, y, split - 28, ROW_H, C_SEL);
            fill(dc, 46, y + 9, 8, 8, C_ACCENT);
            text(dc, 62, y + 4, devs[rw->dev].desc, C_TEXT);
        }
    }
    /* Properties. */
    int x = split + 6, cw = w - x - 16;
    fill(dc, x, TREE_TOP, cw, h - TREE_TOP - 10, C_CARD);
    if (sel_dev >= 0) {
        Dev *v = &devs[sel_dev];
        SelectObject(dc, f_bold);
        text(dc, x + 16, TREE_TOP + 14, v->desc, C_TEXT);
        SelectObject(dc, f_ui);
        text(dc, x + 16, TREE_TOP + 36, L"This device is working properly.", RGB(110, 210, 140));
        const WCHAR *keys[] = {L"Class", L"Manufacturer", L"Location", L"Driver service", L"Hardware ID"};
        const WCHAR *vals[] = {v->cls, v->mfg, v->location, v->service, v->hwid};
        for (int i = 0; i < 5; i++) {
            int y = TREE_TOP + 72 + i * 48;
            text(dc, x + 16, y, keys[i], C_DIM);
            text(dc, x + 16, y + 18, vals[i][0] ? vals[i] : L"-", C_TEXT);
        }
    } else {
        text(dc, x + 16, TREE_TOP + 14, L"Select a device to see its properties.", C_DIM);
    }
    EndPaint(hwnd, &ps);
}

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
    case WM_CREATE:
        f_ui = ui_font(14, 0);
        f_bold = ui_font(14, 1);
        f_big = ui_font(24, 0);
        scan();
        /* Open the first classes so the tree is not empty-looking. */
        for (int c = 0; c < nclass && c < 4; c++) classes[c].open = 1;
        build_rows();
        return 0;
    case WM_PAINT:
        paint(hwnd);
        return 0;
    case WM_ERASEBKGND:
        return 1;
    case WM_SIZE:
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    case WM_MOUSEWHEEL:
        top_row -= (short)HIWORD(wp) / 120 * 3;
        if (top_row > nrows - 1) top_row = nrows - 1;
        if (top_row < 0) top_row = 0;
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    case WM_LBUTTONDOWN: {
        RECT r;
        GetClientRect(hwnd, &r);
        int x = (short)LOWORD(lp), y = (short)HIWORD(lp);
        if (x < r.right * 55 / 100 && y >= TREE_TOP + 6) {
            int i = top_row + (y - TREE_TOP - 6) / ROW_H;
            if (i < nrows) {
                if (rows[i].dev < 0) {
                    classes[rows[i].cls].open = !classes[rows[i].cls].open;
                    build_rows();
                } else {
                    sel_dev = rows[i].dev;
                }
            }
            InvalidateRect(hwnd, NULL, FALSE);
        }
        return 0;
    }
    case WM_KEYDOWN:
        if (wp == VK_F5) {
            sel_dev = -1;
            scan();
            InvalidateRect(hwnd, NULL, FALSE);
        }
        return 0;
    case WM_DESTROY:
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcW(hwnd, msg, wp, lp);
}

void __stdcall start(void) {
    WNDCLASSW wc = {0};
    wc.lpfnWndProc = wndproc;
    wc.hInstance = GetModuleHandleW(NULL);
    wc.hCursor = LoadCursorW(NULL, IDC_ARROW);
    wc.lpszClassName = L"OSyxDeviceManager";
    RegisterClassW(&wc);
    HWND hwnd = CreateWindowExW(0, L"OSyxDeviceManager", L"OSyx Device Manager", WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, 980, 640, NULL, NULL, wc.hInstance, NULL);
    if (!hwnd) ExitProcess(1);
    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);
    MSG m;
    while (GetMessageW(&m, NULL, 0, 0) > 0) {
        TranslateMessage(&m);
        DispatchMessageW(&m);
    }
    ExitProcess((UINT)m.wParam);
}
