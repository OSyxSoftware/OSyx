/* OSyx Text Editor (textedit.exe): a plain-text editor for UTF-8 files.
 * A Win32 program without a C runtime, drawn with GDI: a tool bar with
 * New/Open/Save and a path field, line numbers, a blinking caret, keyboard
 * editing (arrows, Home/End, PgUp/PgDn, Backspace/Delete, Enter, Tab),
 * mouse positioning, wheel scrolling and a status bar. Runs on Windows
 * and on OSyx (winrun). Usage: textedit.exe [file] */
#include "app.h"
#include <commdlg.h>

typedef struct {
    WCHAR *s;
    int len, cap;
} Line;

static Line *lines;
static int nlines, caplines;
static int cur_line, cur_col, top_line;
static int modified, caret_on = 1;
static WCHAR path[MAX_PATH] = L"";
static WCHAR status[256] = L"Ready";
/* Path field editing (Open/Save As). */
static int path_mode; /* 0 off, 1 open, 2 save */
static int suppress_path_return;
static WCHAR path_edit[MAX_PATH];
static HFONT font_mono, font_ui;
static int line_h = 18;
static HWND main_wnd;

#define BAR_H 44
#define STATUS_H 26
#define GUTTER 56

static void line_reserve(Line *l, int n) {
    if (n + 1 > l->cap) {
        int c = l->cap ? l->cap * 2 : 32;
        while (c < n + 1) c *= 2;
        l->s = mem_realloc(l->s, c * sizeof(WCHAR));
        l->cap = c;
    }
}

static void lines_reserve(int n) {
    if (n > caplines) {
        int c = caplines ? caplines * 2 : 64;
        while (c < n) c *= 2;
        lines = mem_realloc(lines, c * sizeof(Line));
        caplines = c;
    }
}

static void clear_doc(void) {
    for (int i = 0; i < nlines; i++) mem_free(lines[i].s);
    nlines = 0;
    lines_reserve(1);
    memset(&lines[0], 0, sizeof(Line));
    line_reserve(&lines[0], 0);
    nlines = 1;
    cur_line = cur_col = top_line = 0;
    modified = 0;
}

static void insert_line(int at) {
    lines_reserve(nlines + 1);
    memmove(&lines[at + 1], &lines[at], (nlines - at) * sizeof(Line));
    memset(&lines[at], 0, sizeof(Line));
    line_reserve(&lines[at], 0);
    nlines++;
}

static void update_title(void) {
    WCHAR t[MAX_PATH + 64];
    t[0] = 0;
    if (modified) wcat(t, L"• ", MAX_PATH + 64);
    wcat(t, path[0] ? path : L"Untitled", MAX_PATH + 64);
    wcat(t, L" - OSyx Text Editor", MAX_PATH + 64);
    SetWindowTextW(main_wnd, t);
}

static int load_file(const WCHAR *p) {
    HANDLE h = CreateFileW(p, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    if (h == INVALID_HANDLE_VALUE) {
        wcopy(status, L"Cannot open the file (error ", 256);
        wuint(status + wlen(status), GetLastError());
        wcat(status, L")", 256);
        return 0;
    }
    DWORD size = GetFileSize(h, NULL);
    char *buf = mem_alloc(size + 1);
    DWORD got = 0, total = 0;
    while (total < size && ReadFile(h, buf + total, size - total, &got, NULL) && got) total += got;
    CloseHandle(h);
    int wn = MultiByteToWideChar(CP_UTF8, 0, buf, (int)total, NULL, 0);
    WCHAR *w = mem_alloc((wn + 1) * sizeof(WCHAR));
    MultiByteToWideChar(CP_UTF8, 0, buf, (int)total, w, wn);
    mem_free(buf);
    clear_doc();
    for (int i = 0; i < wn; i++) {
        Line *l = &lines[nlines - 1];
        if (w[i] == L'\r') continue;
        if (w[i] == L'\n') {
            insert_line(nlines);
            continue;
        }
        line_reserve(l, l->len + 1);
        l->s[l->len++] = w[i];
    }
    mem_free(w);
    wcopy(path, p, MAX_PATH);
    wcopy(status, L"Opened ", 256);
    wuint(status + wlen(status), nlines);
    wcat(status, L" lines", 256);
    update_title();
    return 1;
}

static int save_file(const WCHAR *p) {
    int bytes = 0;
    for (int i = 0; i < nlines; i++) bytes += WideCharToMultiByte(CP_UTF8, 0, lines[i].s, lines[i].len, NULL, 0, NULL, NULL) + 1;
    char *buf = mem_alloc(bytes + 1);
    int n = 0;
    for (int i = 0; i < nlines; i++) {
        n += WideCharToMultiByte(CP_UTF8, 0, lines[i].s, lines[i].len, buf + n, bytes - n, NULL, NULL);
        if (i + 1 < nlines) buf[n++] = '\n';
    }
    HANDLE h = CreateFileW(p, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (h == INVALID_HANDLE_VALUE) {
        mem_free(buf);
        wcopy(status, L"Cannot save the file (error ", 256);
        wuint(status + wlen(status), GetLastError());
        wcat(status, L")", 256);
        return 0;
    }
    DWORD w = 0;
    BOOL ok = WriteFile(h, buf, n, &w, NULL) && (int)w == n;
    CloseHandle(h);
    mem_free(buf);
    if (!ok) {
        wcopy(status, L"Write failed", 256);
        return 0;
    }
    wcopy(path, p, MAX_PATH);
    modified = 0;
    wcopy(status, L"Saved ", 256);
    wuint(status + wlen(status), n);
    wcat(status, L" bytes", 256);
    update_title();
    return 1;
}

static int visible_lines(void) {
    RECT r;
    GetClientRect(main_wnd, &r);
    int h = r.bottom - BAR_H - STATUS_H - 8;
    return h > line_h ? h / line_h : 1;
}

static void ensure_visible(void) {
    int vis = visible_lines();
    if (cur_line < top_line) top_line = cur_line;
    if (cur_line >= top_line + vis) top_line = cur_line - vis + 1;
    if (top_line < 0) top_line = 0;
}

static int text_width(HDC dc, const WCHAR *s, int n) {
    SIZE sz;
    if (n <= 0) return 0;
    GetTextExtentPoint32W(dc, s, n, &sz);
    return sz.cx;
}

static void type_char(WCHAR c) {
    Line *l = &lines[cur_line];
    line_reserve(l, l->len + 1);
    memmove(&l->s[cur_col + 1], &l->s[cur_col], (l->len - cur_col) * sizeof(WCHAR));
    l->s[cur_col++] = c;
    l->len++;
    modified = 1;
}

static void newline(void) {
    insert_line(cur_line + 1);
    Line *a = &lines[cur_line], *b = &lines[cur_line + 1];
    int rest = a->len - cur_col;
    line_reserve(b, rest);
    memcpy(b->s, a->s + cur_col, rest * sizeof(WCHAR));
    b->len = rest;
    a->len = cur_col;
    cur_line++;
    cur_col = 0;
    modified = 1;
}

static void backspace(void) {
    if (cur_col > 0) {
        Line *l = &lines[cur_line];
        memmove(&l->s[cur_col - 1], &l->s[cur_col], (l->len - cur_col) * sizeof(WCHAR));
        l->len--;
        cur_col--;
        modified = 1;
    } else if (cur_line > 0) {
        Line *p = &lines[cur_line - 1], *l = &lines[cur_line];
        int at = p->len;
        line_reserve(p, p->len + l->len);
        memcpy(p->s + p->len, l->s, l->len * sizeof(WCHAR));
        p->len += l->len;
        mem_free(l->s);
        memmove(&lines[cur_line], &lines[cur_line + 1], (nlines - cur_line - 1) * sizeof(Line));
        nlines--;
        cur_line--;
        cur_col = at;
        modified = 1;
    }
}

static void delete_forward(void) {
    Line *l = &lines[cur_line];
    if (cur_col < l->len) {
        cur_col++;
        backspace();
    } else if (cur_line + 1 < nlines) {
        cur_line++;
        cur_col = 0;
        backspace();
    }
}

static void start_path_mode(int mode) {
    WCHAR selected[MAX_PATH];
    wcopy(selected, path, MAX_PATH);
    OPENFILENAMEW dialog;
    memset(&dialog, 0, sizeof(dialog));
    dialog.lStructSize = sizeof(dialog);
    dialog.hwndOwner = main_wnd;
    dialog.lpstrFile = selected;
    dialog.nMaxFile = MAX_PATH;
    dialog.lpstrFilter = L"Text documents (*.txt)\0*.txt;*.md;*.ax\0All files\0*.*\0\0";
    dialog.nFilterIndex = 1;
    dialog.lpstrDefExt = L"txt";
    dialog.Flags = OFN_EXPLORER | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST |
                   (mode == 1 ? OFN_FILEMUSTEXIST : OFN_OVERWRITEPROMPT);
    BOOL ok = mode == 1 ? GetOpenFileNameW(&dialog) : GetSaveFileNameW(&dialog);
    if (ok) {
        if (mode == 1) load_file(selected);
        else save_file(selected);
    } else if (CommDlgExtendedError()) {
        wcopy(status, L"The system file dialog could not be opened", 256);
    }
    path_mode = 0;
    InvalidateRect(main_wnd, NULL, FALSE);
}

static void paint(HWND hwnd) {
    PAINTSTRUCT ps;
    HDC dc = BeginPaint(hwnd, &ps);
    RECT r;
    GetClientRect(hwnd, &r);
    int w = r.right, h = r.bottom;
    /* Tool bar. */
    fill(dc, 0, 0, w, BAR_H, C_PANEL);
    fill(dc, 0, BAR_H - 1, w, 1, C_BORDER);
    SelectObject(dc, font_ui);
    static const WCHAR *buttons[] = {L"New", L"Open", L"Save"};
    for (int i = 0; i < 3; i++) {
        fill(dc, 10 + i * 74, 8, 66, 28, C_CARD);
        text(dc, 22 + i * 74, 14, buttons[i], C_TEXT);
    }
    int px = 10 + 3 * 74 + 8;
    fill(dc, px, 8, w - px - 10, 28, path_mode ? RGB(20, 22, 28) : C_BG);
    if (path_mode) fill(dc, px, 34, w - px - 10, 2, C_ACCENT);
    text(dc, px + 8, 14, path_mode ? path_edit : (path[0] ? path : L"Untitled - Ctrl+O to open, Ctrl+S to save"), path_mode || path[0] ? C_TEXT : C_FAINT);
    if (path_mode) {
        int cx = px + 8 + text_width(dc, path_edit, wlen(path_edit));
        if (caret_on) fill(dc, cx, 13, 2, 18, C_ACCENT);
    }
    /* Text area. */
    int ty = BAR_H + 4, th = h - BAR_H - STATUS_H - 4;
    fill(dc, 0, BAR_H, w, h - BAR_H - STATUS_H, C_BG);
    fill(dc, 0, BAR_H, GUTTER, h - BAR_H - STATUS_H, RGB(28, 30, 38));
    SelectObject(dc, font_mono);
    int vis = th / line_h;
    for (int i = 0; i <= vis && top_line + i < nlines; i++) {
        int ln = top_line + i, y = ty + i * line_h;
        WCHAR num[16];
        wuint(num, ln + 1);
        if (ln == cur_line) fill(dc, GUTTER, y, w - GUTTER, line_h, RGB(30, 33, 42));
        text_right(dc, GUTTER - 10, y, num, ln == cur_line ? C_DIM : C_FAINT);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, C_TEXT);
        if (lines[ln].len) TextOutW(dc, GUTTER + 8, y, lines[ln].s, lines[ln].len);
        if (ln == cur_line && caret_on && !path_mode) {
            int cx = GUTTER + 8 + text_width(dc, lines[ln].s, cur_col);
            fill(dc, cx, y + 1, 2, line_h - 2, C_ACCENT);
        }
    }
    /* Status bar. */
    SelectObject(dc, font_ui);
    fill(dc, 0, h - STATUS_H, w, STATUS_H, C_PANEL);
    fill(dc, 0, h - STATUS_H, w, 1, C_BORDER);
    text(dc, 12, h - STATUS_H + 5, status, C_DIM);
    WCHAR pos[96] = L"Ln ";
    wuint(pos + wlen(pos), cur_line + 1);
    wcat(pos, L", Col ", 96);
    wuint(pos + wlen(pos), cur_col + 1);
    wcat(pos, L"    ", 96);
    wuint(pos + wlen(pos), nlines);
    wcat(pos, L" lines    UTF-8", 96);
    if (modified) wcat(pos, L"    Modified", 96);
    text_right(dc, w - 12, h - STATUS_H + 5, pos, C_DIM);
    EndPaint(hwnd, &ps);
}

static void click_text(HWND hwnd, int x, int y) {
    int ln = top_line + (y - BAR_H - 4) / line_h;
    if (ln < 0) ln = 0;
    if (ln >= nlines) ln = nlines - 1;
    HDC dc = GetDC(hwnd);
    SelectObject(dc, font_mono);
    int col = 0, best = 0x7fffffff;
    for (int c = 0; c <= lines[ln].len; c++) {
        int d = GUTTER + 8 + text_width(dc, lines[ln].s, c) - x;
        if (d < 0) d = -d;
        if (d < best) {
            best = d;
            col = c;
        }
    }
    ReleaseDC(hwnd, dc);
    cur_line = ln;
    cur_col = col;
}

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
    case WM_CREATE: {
        main_wnd = hwnd;
        font_mono = mono_font(15);
        font_ui = ui_font(14, 0);
        HDC dc = GetDC(hwnd);
        SelectObject(dc, font_mono);
        TEXTMETRICW tm;
        GetTextMetricsW(dc, &tm);
        line_h = tm.tmHeight + 2;
        ReleaseDC(hwnd, dc);
        SetTimer(hwnd, 1, 530, NULL);
        return 0;
    }
    case WM_TIMER:
        caret_on = !caret_on;
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    case WM_PAINT:
        paint(hwnd);
        return 0;
    case WM_ERASEBKGND:
        return 1;
    case WM_SIZE:
        ensure_visible();
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    case WM_MOUSEWHEEL: {
        int d = (short)HIWORD(wp) / 120;
        top_line -= d * 3;
        if (top_line > nlines - 1) top_line = nlines - 1;
        if (top_line < 0) top_line = 0;
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    }
    case WM_LBUTTONDOWN: {
        int x = (short)LOWORD(lp), y = (short)HIWORD(lp);
        if (y < BAR_H) {
            if (x >= 10 && x < 76) {
                clear_doc();
                path[0] = 0;
                wcopy(status, L"New document", 256);
                update_title();
            } else if (x >= 84 && x < 150) {
                start_path_mode(1);
            } else if (x >= 158 && x < 224) {
                if (path[0]) save_file(path);
                else start_path_mode(2);
            } else if (x > 232) {
                start_path_mode(path[0] ? 2 : 1);
            }
        } else if (y < BAR_H + 4 + visible_lines() * line_h + line_h) {
            path_mode = 0;
            click_text(hwnd, x, y);
        }
        caret_on = 1;
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    }
    case WM_KEYDOWN: {
        int ctrl = GetKeyState(VK_CONTROL) < 0;
        caret_on = 1;
        if (path_mode) {
            if (wp == VK_ESCAPE) {
                path_mode = 0;
                wcopy(status, L"Cancelled", 256);
            } else if (wp == VK_RETURN) {
                /* TranslateMessage also queues WM_CHAR for this Return.
                 * It belongs to the path prompt, not the document. */
                suppress_path_return = 1;
                int mode = path_mode;
                path_mode = 0;
                if (mode == 1) load_file(path_edit);
                else save_file(path_edit);
            } else if (wp == VK_BACK) {
                int n = wlen(path_edit);
                if (n) path_edit[n - 1] = 0;
            }
            InvalidateRect(hwnd, NULL, FALSE);
            return 0;
        }
        if (ctrl && wp == 'S') {
            if (path[0] && GetKeyState(VK_SHIFT) >= 0) save_file(path);
            else start_path_mode(2);
        } else if (ctrl && wp == 'O') {
            start_path_mode(1);
        } else if (ctrl && wp == 'N') {
            clear_doc();
            path[0] = 0;
            update_title();
        } else {
            Line *l = &lines[cur_line];
            switch (wp) {
            case VK_LEFT:
                if (cur_col > 0) cur_col--;
                else if (cur_line > 0) cur_col = lines[--cur_line].len;
                break;
            case VK_RIGHT:
                if (cur_col < l->len) cur_col++;
                else if (cur_line + 1 < nlines) { cur_line++; cur_col = 0; }
                break;
            case VK_UP:
                if (cur_line > 0) cur_line--;
                break;
            case VK_DOWN:
                if (cur_line + 1 < nlines) cur_line++;
                break;
            case VK_HOME: cur_col = 0; break;
            case VK_END: cur_col = l->len; break;
            case VK_PRIOR: cur_line -= visible_lines(); if (cur_line < 0) cur_line = 0; break;
            case VK_NEXT: cur_line += visible_lines(); if (cur_line >= nlines) cur_line = nlines - 1; break;
            case VK_DELETE: delete_forward(); break;
            default: return 0;
            }
            if (cur_col > lines[cur_line].len) cur_col = lines[cur_line].len;
        }
        ensure_visible();
        update_title();
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    }
    case WM_CHAR: {
        WCHAR c = (WCHAR)wp;
        if (c == L'\r' && suppress_path_return) {
            suppress_path_return = 0;
            return 0;
        }
        if (GetKeyState(VK_CONTROL) < 0 && c < 32) return 0;
        if (path_mode) {
            int n = wlen(path_edit);
            if (c >= 32 && n < MAX_PATH - 1) {
                path_edit[n] = c;
                path_edit[n + 1] = 0;
            }
        } else if (c == L'\r') {
            newline();
        } else if (c == 8) {
            backspace();
        } else if (c == L'\t') {
            for (int i = 0; i < 4; i++) type_char(L' ');
        } else if (c >= 32) {
            type_char(c);
        }
        ensure_visible();
        update_title();
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    }
    case WM_CLOSE:
        if (modified && MessageBoxW(hwnd, L"The document has unsaved changes. Close anyway?", L"OSyx Text Editor", MB_OKCANCEL | MB_ICONWARNING) != IDOK) return 0;
        DestroyWindow(hwnd);
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
    wc.hCursor = LoadCursorW(NULL, IDC_IBEAM);
    wc.lpszClassName = L"OSyxTextEditor";
    RegisterClassW(&wc);
    clear_doc();
    HWND hwnd = CreateWindowExW(0, L"OSyxTextEditor", L"OSyx Text Editor", WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, 900, 620, NULL, NULL, wc.hInstance, NULL);
    if (!hwnd) ExitProcess(1);
    WCHAR arg[MAX_PATH];
    if (first_arg(arg, MAX_PATH)) {
        if (!load_file(arg)) wcopy(path, arg, MAX_PATH); /* a new file of that name */
    }
    update_title();
    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);
    MSG m;
    while (GetMessageW(&m, NULL, 0, 0) > 0) {
        TranslateMessage(&m);
        DispatchMessageW(&m);
    }
    ExitProcess((UINT)m.wParam);
}
