/* Drawing on another process's window: a window handle is valid in every
 * process, and a child process's GetDC + BitBlt on the parent's window
 * draws there (Chromium's GPU process draws into the browser's windows
 * this way). The parent makes a window, the child (this program with
 * --draw <hwnd>) blits a green square into it, the parent reads it back. */
#include "common.h"

#define GREEN RGB(10, 200, 30)

static LRESULT CALLBACK proc(HWND w, UINT m, WPARAM wp, LPARAM lp) {
    if (m == WM_PAINT) {
        PAINTSTRUCT ps;
        BeginPaint(w, &ps);
        EndPaint(w, &ps);
        return 0;
    }
    if (m == WM_ERASEBKGND) return 1;
    return DefWindowProcW(w, m, wp, lp);
}

static void pump(DWORD ms) {
    DWORD until = GetTickCount() + ms;
    MSG msg;
    while ((LONG)(until - GetTickCount()) > 0) {
        while (PeekMessageW(&msg, NULL, 0, 0, PM_REMOVE)) {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Sleep(10);
    }
}

/* The child: exit code 0 when every step worked, else the failing step. */
static DWORD child(const WCHAR *at) {
    UINT_PTR v = 0;
    while (*at == L' ') at++;
    for (; *at; at++) {
        WCHAR c = *at;
        int d = c >= L'0' && c <= L'9' ? c - L'0' : c >= L'a' && c <= L'f' ? c - L'a' + 10 : -1;
        if (d < 0) break;
        v = v * 16 + (UINT_PTR)d;
    }
    HWND target = (HWND)v;
    if (!IsWindow(target)) return 1;
    HDC dc = GetDC(target);
    if (!dc) return 2;
    BITMAPINFO bi = {0};
    bi.bmiHeader.biSize = sizeof bi.bmiHeader;
    bi.bmiHeader.biWidth = 16;
    bi.bmiHeader.biHeight = -16;
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    void *bits = NULL;
    HBITMAP bmp = CreateDIBSection(NULL, &bi, DIB_RGB_COLORS, &bits, NULL, 0);
    if (!bmp || !bits) return 3;
    for (int i = 0; i < 16 * 16; i++) ((DWORD *)bits)[i] = 0xFF000000u | (10u << 16) | (200u << 8) | 30u;
    HDC mem = CreateCompatibleDC(NULL);
    HGDIOBJ old = SelectObject(mem, bmp);
    BOOL blitted = BitBlt(dc, 0, 0, 16, 16, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old);
    DeleteDC(mem);
    DeleteObject(bmp);
    if (!blitted) return 4;
    if (ReleaseDC(target, dc) != 1) return 5;
    return 0;
}

void __stdcall start(void) {
    const WCHAR *cmd = GetCommandLineW(), *at = cmd;
    for (; *at; at++)
        if (at[0] == L'-' && at[1] == L'-' && at[2] == L'd' && at[3] == L'r' && at[4] == L'a' && at[5] == L'w') ExitProcess(child(at + 6));

    WNDCLASSW wc = {0};
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleW(NULL);
    wc.lpszClassName = L"OSyxXWindow";
    check("RegisterClassW", RegisterClassW(&wc) != 0);
    HWND w = CreateWindowExW(WS_EX_TOPMOST, L"OSyxXWindow", L"xwindow", WS_POPUP | WS_VISIBLE, 100, 100, 64, 64, NULL, NULL, wc.hInstance, NULL);
    check("CreateWindowExW", w != NULL);
    if (!w) finish("xwindow");
    pump(300);

    WCHAR exe[MAX_PATH];
    GetModuleFileNameW(NULL, exe, MAX_PATH);
    WCHAR line[MAX_PATH + 40];
    int n = 0;
    for (int i = 0; exe[i]; i++) line[n++] = exe[i];
    const WCHAR *tag = L" --draw ";
    for (int i = 0; tag[i]; i++) line[n++] = tag[i];
    UINT_PTR hv = (UINT_PTR)w;
    WCHAR hex[17];
    int len = 0;
    do {
        hex[len++] = L"0123456789abcdef"[hv & 15];
        hv >>= 4;
    } while (hv);
    while (len) line[n++] = hex[--len];
    line[n] = 0;

    STARTUPINFOW si = {0};
    si.cb = sizeof si;
    PROCESS_INFORMATION pi = {0};
    BOOL ok = CreateProcessW(exe, line, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi);
    check("start the child", ok);
    DWORD code = 99;
    if (ok) {
        DWORD waited = WAIT_TIMEOUT;
        for (int i = 0; i < 100 && waited == WAIT_TIMEOUT; i++) {
            pump(50);
            waited = WaitForSingleObject(pi.hProcess, 0);
        }
        GetExitCodeProcess(pi.hProcess, &code);
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
    }
    check_eq("the child's GetDC, BitBlt and ReleaseDC on the parent's window", code, 0);
    Sleep(100);
    HDC dc = GetDC(w);
    COLORREF c = GetPixel(dc, 4, 4);
    COLORREF outside = GetPixel(dc, 40, 40);
    ReleaseDC(w, dc);
    check("the child's square is in the parent's window", c == GREEN);
    check("... and only there", outside != GREEN);
    DestroyWindow(w);
    finish("xwindow");
}
