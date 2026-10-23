/* OSyx Disk Utility (diskutil.exe): the volumes of the computer with their
 * file system, label, serial number, capacity, free space and cluster
 * size, and an analysis of the largest folders of the selected volume.
 * Documented Win32 calls only (GetLogicalDriveStrings, GetVolumeInformation,
 * GetDiskFreeSpace(Ex), GetDriveType, FindFirstFile/FindNextFile), so it
 * runs on Windows and on OSyx (winrun), where the drive letters are the
 * OSyx mounts (C: is the root). No C runtime. */
#include "app.h"

#define MAX_VOL 26
#define MAX_DIRS 12

typedef struct {
    WCHAR root[8];
    WCHAR label[64];
    WCHAR fs[32];
    DWORD serial, type;
    ULARGE_INTEGER total, freeb;
    DWORD sectors_per_cluster, bytes_per_sector;
    BOOL ok;
} Volume;

typedef struct {
    WCHAR name[MAX_PATH];
    unsigned long long bytes;
    unsigned files;
} DirStat;

static Volume vols[MAX_VOL];
static int nvols, sel;
static DirStat dirs[MAX_DIRS];
static int ndirs, analyzed = -1;
static unsigned scanned_files;
static HFONT f_ui, f_bold, f_big;
static WCHAR status[160] = L"";

static void refresh(void) {
    WCHAR buf[256];
    DWORD n = GetLogicalDriveStringsW(255, buf);
    nvols = 0;
    for (WCHAR *p = buf; n && *p && nvols < MAX_VOL; p += wlen(p) + 1) {
        Volume *v = &vols[nvols++];
        memset(v, 0, sizeof *v);
        wcopy(v->root, p, 8);
        v->type = GetDriveTypeW(p);
        DWORD maxlen, flags;
        GetVolumeInformationW(p, v->label, 64, &v->serial, &maxlen, &flags, v->fs, 32);
        ULARGE_INTEGER avail;
        v->ok = GetDiskFreeSpaceExW(p, &avail, &v->total, &v->freeb);
        DWORD freec, totalc;
        GetDiskFreeSpaceW(p, &v->sectors_per_cluster, &v->bytes_per_sector, &freec, &totalc);
    }
    if (sel >= nvols) sel = 0;
    wcopy(status, L"", 160);
}

/* Total size of a directory tree (bounded). */
static unsigned long long tree_size(const WCHAR *dir, int depth, unsigned *files) {
    if (depth > 12 || scanned_files > 200000) return 0;
    WCHAR pat[MAX_PATH];
    wcopy(pat, dir, MAX_PATH);
    wcat(pat, L"\\*", MAX_PATH);
    WIN32_FIND_DATAW fd;
    HANDLE h = FindFirstFileW(pat, &fd);
    if (h == INVALID_HANDLE_VALUE) return 0;
    unsigned long long sum = 0;
    do {
        if (fd.cFileName[0] == L'.' && (!fd.cFileName[1] || (fd.cFileName[1] == L'.' && !fd.cFileName[2]))) continue;
        if (fd.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) continue;
        if (fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) {
            WCHAR sub[MAX_PATH];
            wcopy(sub, dir, MAX_PATH);
            wcat(sub, L"\\", MAX_PATH);
            wcat(sub, fd.cFileName, MAX_PATH);
            sum += tree_size(sub, depth + 1, files);
        } else {
            sum += ((unsigned long long)fd.nFileSizeHigh << 32) | fd.nFileSizeLow;
            (*files)++;
            scanned_files++;
        }
    } while (FindNextFileW(h, &fd));
    FindClose(h);
    return sum;
}

static void analyze(HWND hwnd) {
    Volume *v = &vols[sel];
    WCHAR root[8];
    wcopy(root, v->root, 8);
    int n = wlen(root);
    if (n && root[n - 1] == L'\\') root[n - 1] = 0;
    WCHAR pat[MAX_PATH];
    wcopy(pat, root, MAX_PATH);
    wcat(pat, L"\\*", MAX_PATH);
    ndirs = 0;
    scanned_files = 0;
    SetCursor(LoadCursorW(NULL, IDC_WAIT));
    WIN32_FIND_DATAW fd;
    HANDLE h = FindFirstFileW(pat, &fd);
    if (h != INVALID_HANDLE_VALUE) {
        do {
            if (!(fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) || fd.cFileName[0] == L'.') continue;
            if (fd.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) continue;
            DirStat d;
            memset(&d, 0, sizeof d);
            wcopy(d.name, fd.cFileName, MAX_PATH);
            WCHAR sub[MAX_PATH];
            wcopy(sub, root, MAX_PATH);
            wcat(sub, L"\\", MAX_PATH);
            wcat(sub, fd.cFileName, MAX_PATH);
            d.bytes = tree_size(sub, 1, &d.files);
            /* Keep the largest MAX_DIRS, sorted. */
            int at = ndirs;
            while (at > 0 && dirs[at - 1].bytes < d.bytes) at--;
            if (at < MAX_DIRS) {
                int last = ndirs < MAX_DIRS ? ndirs : MAX_DIRS - 1;
                memmove(&dirs[at + 1], &dirs[at], (last - at) * sizeof(DirStat));
                dirs[at] = d;
                if (ndirs < MAX_DIRS) ndirs++;
            }
        } while (FindNextFileW(h, &fd));
        FindClose(h);
    }
    analyzed = sel;
    wcopy(status, L"Analyzed ", 160);
    wuint(status + wlen(status), scanned_files);
    wcat(status, L" files", 160);
    if (scanned_files > 200000) wcat(status, L" (stopped at the limit)", 160);
    InvalidateRect(hwnd, NULL, FALSE);
}

static const WCHAR *type_name(DWORD t) {
    switch (t) {
    case DRIVE_FIXED: return L"Local disk";
    case DRIVE_REMOVABLE: return L"Removable disk";
    case DRIVE_REMOTE: return L"Network drive";
    case DRIVE_CDROM: return L"Optical drive";
    case DRIVE_RAMDISK: return L"RAM disk";
    default: return L"Volume";
    }
}

static void row(HDC dc, int x, int y, int w, const WCHAR *k, const WCHAR *v) {
    text(dc, x, y, k, C_DIM);
    text_right(dc, x + w, y, v, C_TEXT);
}

static void bar(HDC dc, int x, int y, int w, int h, unsigned long long used, unsigned long long total, COLORREF c) {
    fill(dc, x, y, w, h, RGB(50, 54, 66));
    if (total) {
        int u = (int)((double)used / (double)total * w);
        COLORREF col = used * 100 / total >= 90 ? RGB(240, 84, 84) : c;
        fill(dc, x, y, u, h, col);
    }
}

static void paint(HWND hwnd) {
    PAINTSTRUCT ps;
    HDC dc = BeginPaint(hwnd, &ps);
    RECT r;
    GetClientRect(hwnd, &r);
    int w = r.right, h = r.bottom;
    fill(dc, 0, 0, w, h, C_BG);
    /* Volume list. */
    fill(dc, 0, 0, 270, h, C_PANEL);
    SelectObject(dc, f_big);
    text(dc, 20, 16, L"Disk Utility", C_TEXT);
    SelectObject(dc, f_ui);
    text(dc, 20, 56, L"VOLUMES", C_FAINT);
    for (int i = 0; i < nvols; i++) {
        Volume *v = &vols[i];
        int y = 80 + i * 64;
        if (i == sel) fill(dc, 10, y, 250, 58, C_SEL);
        SelectObject(dc, f_bold);
        WCHAR t[96];
        wcopy(t, v->label[0] ? v->label : type_name(v->type), 96);
        wcat(t, L" (", 96);
        wcat(t, v->root, 96);
        t[wlen(t) - 1] = 0;
        wcat(t, L")", 96);
        text(dc, 22, y + 6, t, C_TEXT);
        SelectObject(dc, f_ui);
        WCHAR s[64];
        wsize(s, v->freeb.QuadPart);
        wcat(s, L" free of ", 64);
        wsize(s + wlen(s), v->total.QuadPart);
        text(dc, 22, y + 26, v->ok ? s : L"not available", C_DIM);
        bar(dc, 22, y + 46, 226, 4, v->total.QuadPart - v->freeb.QuadPart, v->total.QuadPart, C_ACCENT);
    }
    /* Details. */
    if (nvols > 0) {
        Volume *v = &vols[sel];
        int x = 300, cw = w - x - 30;
        SelectObject(dc, f_big);
        WCHAR t[96];
        wcopy(t, v->label[0] ? v->label : type_name(v->type), 96);
        text(dc, x, 16, t, C_TEXT);
        SelectObject(dc, f_ui);
        WCHAR sub[96];
        wcopy(sub, type_name(v->type), 96);
        wcat(sub, L"  ·  ", 96);
        wcat(sub, v->fs[0] ? v->fs : L"unknown file system", 96);
        wcat(sub, L"  ·  ", 96);
        wcat(sub, v->root, 96);
        text(dc, x, 50, sub, C_DIM);
        unsigned long long used = v->total.QuadPart - v->freeb.QuadPart;
        bar(dc, x, 84, cw, 22, used, v->total.QuadPart, C_ACCENT);
        WCHAR a[64], b[64];
        wsize(a, used);
        wcat(a, L" used", 64);
        wsize(b, v->freeb.QuadPart);
        wcat(b, L" free", 64);
        text(dc, x, 112, a, C_TEXT);
        text_right(dc, x + cw, 112, b, C_TEXT);
        fill(dc, x, 146, cw, 172, C_CARD);
        WCHAR val[64];
        int ry = 156;
        row(dc, x + 16, ry, cw - 32, L"Capacity", (wsize(val, v->total.QuadPart), val)); ry += 26;
        row(dc, x + 16, ry, cw - 32, L"File system", v->fs[0] ? v->fs : L"-"); ry += 26;
        row(dc, x + 16, ry, cw - 32, L"Label", v->label[0] ? v->label : L"(none)"); ry += 26;
        wuint(val, v->sectors_per_cluster * v->bytes_per_sector);
        wcat(val, L" bytes", 64);
        row(dc, x + 16, ry, cw - 32, L"Cluster size", val); ry += 26;
        {
            static const WCHAR hex[] = L"0123456789ABCDEF";
            for (int i = 0; i < 8; i++) val[i < 4 ? i : i + 1] = hex[(v->serial >> (28 - i * 4)) & 15];
            val[4] = L'-';
            val[9] = 0;
        }
        row(dc, x + 16, ry, cw - 32, L"Serial number", val); ry += 26;
        row(dc, x + 16, ry, cw - 32, L"Type", type_name(v->type));
        /* Buttons. */
        fill(dc, x, 334, 120, 32, C_ACCENT);
        text(dc, x + 30, 341, L"Analyze", RGB(255, 255, 255));
        fill(dc, x + 132, 334, 120, 32, C_CARD);
        text(dc, x + 162, 341, L"Refresh", C_TEXT);
        text(dc, x + 270, 341, status, C_DIM);
        /* Folder analysis. */
        if (analyzed == sel) {
            SelectObject(dc, f_bold);
            text(dc, x, 384, L"Largest folders", C_TEXT);
            SelectObject(dc, f_ui);
            unsigned long long top = ndirs ? dirs[0].bytes : 1;
            for (int i = 0; i < ndirs && 410 + i * 24 < h - 10; i++) {
                int y = 410 + i * 24;
                text(dc, x, y, dirs[i].name, C_TEXT);
                int bx = x + 200, bw = cw - 320;
                bar(dc, bx, y + 6, bw, 8, dirs[i].bytes, top ? top : 1, RGB(90, 200, 120));
                wsize(val, dirs[i].bytes);
                text_right(dc, x + cw, y, val, C_DIM);
            }
            if (!ndirs) text(dc, x, 410, L"No folders", C_DIM);
        }
    }
    EndPaint(hwnd, &ps);
}

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
    case WM_CREATE:
        f_ui = ui_font(14, 0);
        f_bold = ui_font(14, 1);
        f_big = ui_font(24, 0);
        refresh();
        return 0;
    case WM_PAINT:
        paint(hwnd);
        return 0;
    case WM_ERASEBKGND:
        return 1;
    case WM_SIZE:
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    case WM_LBUTTONDOWN: {
        int x = (short)LOWORD(lp), y = (short)HIWORD(lp);
        if (x < 270 && y >= 80) {
            int i = (y - 80) / 64;
            if (i < nvols) sel = i;
        } else if (y >= 334 && y < 366 && x >= 300 && x < 420) {
            analyze(hwnd);
        } else if (y >= 334 && y < 366 && x >= 432 && x < 552) {
            refresh();
            wcopy(status, L"Refreshed", 160);
        }
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    }
    case WM_KEYDOWN:
        if (wp == VK_DOWN && sel + 1 < nvols) sel++;
        if (wp == VK_UP && sel > 0) sel--;
        if (wp == VK_F5) refresh();
        InvalidateRect(hwnd, NULL, FALSE);
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
    wc.lpszClassName = L"OSyxDiskUtility";
    RegisterClassW(&wc);
    HWND hwnd = CreateWindowExW(0, L"OSyxDiskUtility", L"OSyx Disk Utility", WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, 940, 640, NULL, NULL, wc.hInstance, NULL);
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
