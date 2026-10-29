/* Named pipes across processes, as Chromium/Firefox IPC uses them: an
 * overlapped server instance on a completion port, a client in a child
 * process (this program run with "client"), message mode with
 * ERROR_MORE_DATA, cancellation and a broken pipe. */
#include "common.h"

static const WCHAR NAME[] = L"\\\\.\\pipe\\LOCAL\\osyx-wintest-pipes";
static const WCHAR MSGNAME[] = L"\\\\.\\pipe\\osyx-wintest-messages";

static int is_client(void) {
    const WCHAR *c = GetCommandLineW();
    for (; *c; c++)
        if (c[0] == L' ' && c[1] == L'c' && c[2] == L'l' && c[3] == L'i') return 1;
    return 0;
}

/* The child: connects, sends "ping", expects "pong", reports the server pid. */
static void client(void) {
    HANDLE h = INVALID_HANDLE_VALUE;
    for (int i = 0; i < 200 && h == INVALID_HANDLE_VALUE; i++) {
        h = CreateFileW(NAME, GENERIC_READ | GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
        if (h == INVALID_HANDLE_VALUE) Sleep(10);
    }
    DWORD n = 0;
    char buf[8];
    ULONG server = 0;
    int ok = h != INVALID_HANDLE_VALUE && WriteFile(h, "ping", 4, &n, NULL) && n == 4 && ReadFile(h, buf, 8, &n, NULL) && n == 4 &&
             mem_eq(buf, "pong", 4) && GetNamedPipeServerProcessId(h, &server) && server != GetCurrentProcessId();
    CloseHandle(h);
    ExitProcess(ok ? 0 : 3);
}

static DWORD WINAPI message_client(LPVOID unused) {
    (void)unused;
    HANDLE h = CreateFileW(MSGNAME, GENERIC_READ | GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
    DWORD n;
    WriteFile(h, "0123456789", 10, &n, NULL);
    WriteFile(h, "ab", 2, &n, NULL);
    Sleep(200);
    CloseHandle(h);
    return 0;
}

void __stdcall start(void) {
    if (is_client()) client();

    HANDLE srv = CreateNamedPipeW(NAME, PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                                  PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, 1, 4096, 4096, 5000, NULL);
    check("CreateNamedPipeW (overlapped, first instance)", srv != INVALID_HANDLE_VALUE);
    check("a second instance beyond the limit is busy", CreateNamedPipeW(NAME, PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE, 0, 1, 0, 0, 0, NULL) ==
                                                            INVALID_HANDLE_VALUE && GetLastError() == ERROR_PIPE_BUSY);
    check("GetFileType is FILE_TYPE_PIPE", GetFileType(srv) == FILE_TYPE_PIPE);
    HANDLE port = CreateIoCompletionPort(srv, NULL, 77, 1);
    check("CreateIoCompletionPort associates the pipe", port != NULL);
    OVERLAPPED oc = {0};
    BOOL connected = ConnectNamedPipe(srv, &oc);
    check("overlapped ConnectNamedPipe is pending", !connected && GetLastError() == ERROR_IO_PENDING);

    WCHAR exe[MAX_PATH], line[MAX_PATH + 16] = L"\"";
    GetModuleFileNameW(NULL, exe, MAX_PATH);
    lstrcatW(line, exe);
    lstrcatW(line, L"\" client");
    STARTUPINFOW si = {0};
    si.cb = sizeof si;
    PROCESS_INFORMATION pi;
    check("start the client process", CreateProcessW(NULL, line, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi));

    DWORD bytes = 0;
    ULONG_PTR key = 0;
    OVERLAPPED *done = NULL;
    check("the connection completes on the port", GetQueuedCompletionStatus(port, &bytes, &key, &done, 5000) && key == 77 && done == &oc);
    char buf[16];
    OVERLAPPED orr = {0};
    BOOL r = ReadFile(srv, buf, sizeof buf, NULL, &orr);
    check("overlapped ReadFile", r || GetLastError() == ERROR_IO_PENDING);
    check("... completes with the client's bytes", GetQueuedCompletionStatus(port, &bytes, &key, &done, 5000) && done == &orr && bytes == 4 &&
                                                       mem_eq(buf, "ping", 4));
    OVERLAPPED ow = {0};
    ow.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    ow.hEvent = (HANDLE)((ULONG_PTR)ow.hEvent | 1); /* no completion packet */
    r = WriteFile(srv, "pong", 4, NULL, &ow);
    check("overlapped WriteFile with a flagged event", (r || GetLastError() == ERROR_IO_PENDING) && GetOverlappedResult(srv, &ow, &bytes, TRUE) && bytes == 4);
    WaitForSingleObject(pi.hProcess, 5000);
    DWORD code = 99;
    check("the client saw pong and the server's pid", GetExitCodeProcess(pi.hProcess, &code) && code == 0);
    CloseHandle(pi.hThread), CloseHandle(pi.hProcess);
    OVERLAPPED ob = {0};
    r = ReadFile(srv, buf, sizeof buf, NULL, &ob);
    DWORD err = r ? 0 : GetLastError();
    if (err == ERROR_IO_PENDING) err = GetOverlappedResult(srv, &ob, &bytes, TRUE) ? 0 : GetLastError();
    check("reading after the client closed: ERROR_BROKEN_PIPE", err == ERROR_BROKEN_PIPE);
    check("DisconnectNamedPipe", DisconnectNamedPipe(srv));

    /* Cancellation of a pending read on a connected instance. */
    HANDLE cl = INVALID_HANDLE_VALUE;
    OVERLAPPED oc2 = {0};
    oc2.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (!ConnectNamedPipe(srv, &oc2) && GetLastError() == ERROR_IO_PENDING) {
        cl = CreateFileW(NAME, GENERIC_READ | GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
        GetOverlappedResult(srv, &oc2, &bytes, TRUE);
    }
    /* Exercise repeated overlapped operations and two simultaneous reads.
     * Suppress completion-port packets so this stress does not fill it. */
    HANDLE ev1 = CreateEventW(NULL, TRUE, FALSE, NULL), ev2 = CreateEventW(NULL, TRUE, FALSE, NULL);
    int stress_ok = ev1 && ev2 && cl != INVALID_HANDLE_VALUE;
    for (int i = 0; stress_ok && i < 256; i++) {
        OVERLAPPED a = {0}, b = {0};
        char x = 0, y = 0;
        a.hEvent = (HANDLE)((ULONG_PTR)ev1 | 1); b.hEvent = (HANDLE)((ULONG_PTR)ev2 | 1);
        BOOL ar = ReadFile(srv, &x, 1, NULL, &a);
        if (!ar && GetLastError() != ERROR_IO_PENDING) { stress_ok = 0; break; }
        BOOL br = ReadFile(srv, &y, 1, NULL, &b);
        if (!br && GetLastError() != ERROR_IO_PENDING) {
            CancelIoEx(srv, &a); GetOverlappedResult(srv, &a, &bytes, TRUE);
            stress_ok = 0; break;
        }
        DWORD sent = 0, got_a = 0, got_b = 0;
        if (!WriteFile(cl, "xy", 2, &sent, NULL) || sent != 2) {
            CancelIoEx(srv, &a); CancelIoEx(srv, &b);
            GetOverlappedResult(srv, &a, &bytes, TRUE); GetOverlappedResult(srv, &b, &bytes, TRUE);
            stress_ok = 0; break;
        }
        BOOL aok = GetOverlappedResult(srv, &a, &got_a, TRUE);
        BOOL bok = GetOverlappedResult(srv, &b, &got_b, TRUE);
        stress_ok = aok && bok && got_a == 1 && got_b == 1 &&
                    ((x == 'x' && y == 'y') || (x == 'y' && y == 'x'));
    }
    check("repeated and concurrent overlapped pipe reads", stress_ok);
    CloseHandle(ev1); CloseHandle(ev2);
    OVERLAPPED ox = {0};
    ox.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    r = ReadFile(srv, buf, sizeof buf, NULL, &ox);
    check("a pending read on a quiet pipe", !r && GetLastError() == ERROR_IO_PENDING);
    check("CancelIoEx", CancelIoEx(srv, &ox));
    check("... the read ends with ERROR_OPERATION_ABORTED", !GetOverlappedResult(srv, &ox, &bytes, TRUE) && GetLastError() == ERROR_OPERATION_ABORTED);
    GetQueuedCompletionStatus(port, &bytes, &key, &done, 0); /* drain the packets */
    GetQueuedCompletionStatus(port, &bytes, &key, &done, 0);
    CloseHandle(cl);
    CloseHandle(srv);
    CloseHandle(port);

    /* Message mode, synchronous, client in a thread. */
    HANDLE ms = CreateNamedPipeW(MSGNAME, PIPE_ACCESS_DUPLEX, PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT, 1, 4096, 4096, 0, NULL);
    HANDLE t = CreateThread(NULL, 0, message_client, NULL, 0, NULL);
    BOOL c = ConnectNamedPipe(ms, NULL);
    check("synchronous ConnectNamedPipe", c || GetLastError() == ERROR_PIPE_CONNECTED);
    DWORD n = 0;
    check("a short read of a message: ERROR_MORE_DATA", !ReadFile(ms, buf, 4, &n, NULL) && GetLastError() == ERROR_MORE_DATA && n == 4 && mem_eq(buf, "0123", 4));
    check("... the rest of it", ReadFile(ms, buf, 16, &n, NULL) && n == 6 && mem_eq(buf, "456789", 6));
    check("the next message", ReadFile(ms, buf, 16, &n, NULL) && n == 2 && mem_eq(buf, "ab", 2));
    WaitForSingleObject(t, 2000);
    CloseHandle(t);
    CloseHandle(ms);
    check("opening a pipe nobody serves", CreateFileW(L"\\\\.\\pipe\\osyx-no-such-pipe", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL) ==
                                             INVALID_HANDLE_VALUE && GetLastError() == ERROR_FILE_NOT_FOUND);
    finish("pipes");
}
