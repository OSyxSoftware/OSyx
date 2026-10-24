/* Target E: a small Win32 GUI program. Registers a class, creates a
 * window (a PRISM window on OSyx), paints text and a filled rectangle
 * with GDI in WM_PAINT, uses a timer, invalidates itself, handles a
 * resize, and closes itself through WM_CLOSE -> DestroyWindow ->
 * PostQuitMessage. Every step is printed so the test can follow it. */
#include "common.h"

static int paints, timers, sizes, created;
static HWND main_window;

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
    case WM_CREATE:
        created = 1;
        put("gui: WM_CREATE\r\n");
        return 0;
    case WM_SIZE:
        sizes++;
        put("gui: WM_SIZE ");
        put_uint(LOWORD(lp));
        put("x");
        put_uint(HIWORD(lp));
        put("\r\n");
        return 0;
    case WM_PAINT: {
        PAINTSTRUCT ps;
        HDC dc = BeginPaint(hwnd, &ps);
        RECT rc;
        GetClientRect(hwnd, &rc);
        HBRUSH blue = CreateSolidBrush(RGB(40, 90, 200));
        RECT band = {10, 10, rc.right - 10, 60};
        FillRect(dc, &band, blue);
        DeleteObject(blue);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, RGB(255, 255, 255));
        static const char title[] = "Hello from Win32 on OSyx";
        TextOutA(dc, 20, 26, title, sizeof title - 1);
        SetTextColor(dc, RGB(20, 20, 20));
        static const char line[] = "GDI text and rectangles drawn into a PRISM surface";
        TextOutA(dc, 20, 80, line, sizeof line - 1);
        /* Exercise the same memory-DC -> PRISM path Gecko's software renderer
         * needs, and read the window surface back before it is presented. */
        BITMAPINFO bi = {0};
        bi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
        bi.bmiHeader.biWidth = 16; bi.bmiHeader.biHeight = -16;
        bi.bmiHeader.biPlanes = 1; bi.bmiHeader.biBitCount = 32;
        DWORD *pixels = NULL;
        HBITMAP bitmap = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, (void **)&pixels, NULL, 0);
        HDC memory = CreateCompatibleDC(dc);
        check("PRISM raster source allocated", bitmap && memory && pixels);
        if (bitmap && memory && pixels) {
            for (int i = 0; i < 256; i++) pixels[i] = 0x0012ab34;
            HGDIOBJ saved = SelectObject(memory, bitmap);
            check("BitBlt memory DIB into PRISM", BitBlt(dc, 20, 110, 16, 16, memory, 0, 0, SRCCOPY));
            check_eq("PRISM surface contains copied pixel", GetPixel(dc, 24, 114), RGB(0x12, 0xab, 0x34));
            SelectObject(memory, saved);
        }
        if (memory) DeleteDC(memory);
        if (bitmap) DeleteObject(bitmap);
        EndPaint(hwnd, &ps);
        paints++;
        put("gui: WM_PAINT ");
        put_uint(paints);
        put("\r\n");
        if (paints == 1) SetTimer(hwnd, 1, 100, NULL);
        return 0;
    }
    case WM_TIMER:
        timers++;
        if (timers == 1) {
            InvalidateRect(hwnd, NULL, TRUE); /* second paint */
        } else if (timers == 2) {
            SetWindowPos(hwnd, NULL, 0, 0, 520, 300, SWP_NOMOVE | SWP_NOZORDER);
        } else if (timers == 4) {
            KillTimer(hwnd, 1);
            put("gui: closing\r\n");
            PostMessageA(hwnd, WM_CLOSE, 0, 0);
        }
        return 0;
    case WM_CLOSE:
        DestroyWindow(hwnd);
        return 0;
    case WM_DESTROY:
        put("gui: WM_DESTROY\r\n");
        PostQuitMessage(7);
        return 0;
    }
    return DefWindowProcA(hwnd, msg, wp, lp);
}

void __stdcall start(void) {
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = wndproc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "OSyxWin32Test";
    wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    check("RegisterClassA", RegisterClassA(&wc) != 0);
    main_window = CreateWindowExA(0, "OSyxWin32Test", "Win32 on OSyx", WS_OVERLAPPEDWINDOW, 120, 90, 480, 260, NULL, NULL, wc.hInstance, NULL);
    check("CreateWindowExA", main_window != NULL && created);
    char title[64];
    check("GetWindowTextA", GetWindowTextA(main_window, title, sizeof title) == 13);
    ShowWindow(main_window, SW_SHOW);
    UpdateWindow(main_window);
    MSG m;
    BOOL r;
    while ((r = GetMessageA(&m, NULL, 0, 0)) > 0) {
        TranslateMessage(&m);
        DispatchMessageA(&m);
    }
    check_eq("WM_QUIT carries PostQuitMessage's code", m.wParam, 7);
    check("painted at least twice (initial + InvalidateRect)", paints >= 2);
    check("timer messages arrived", timers >= 4);
    check("WM_SIZE after SetWindowPos", sizes >= 2);
    check("window handle is gone", !IsWindow(main_window));
    finish("gui");
}
