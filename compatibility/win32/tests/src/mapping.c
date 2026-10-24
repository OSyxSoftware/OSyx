/* File mappings: page-file sections shared between views, read-only and
 * copy-on-write file views at 64 KiB offsets, VirtualQuery of views and
 * the error codes Windows reports. The same PE runs on Windows. */
#include "common.h"

#define FILE_BYTES (3 * 65536 + 1000)

static BYTE pattern(DWORD i) { return (BYTE)(i * 7 + (i >> 16)); }

static void writable_file_views(const WCHAR *name) {
    HANDLE f=CreateFileW(name,GENERIC_READ|GENERIC_WRITE,FILE_SHARE_READ|FILE_SHARE_WRITE,NULL,CREATE_ALWAYS,0,NULL);
    check("create writable mapping file",f!=INVALID_HANDLE_VALUE);
    if(f==INVALID_HANDLE_VALUE) return;
    HANDLE small=CreateFileMappingW(f,NULL,PAGE_READWRITE,0,100,NULL);
    BYTE *a=small?MapViewOfFile(small,FILE_MAP_WRITE,0,0,0):NULL;
    check("writable section grows an empty file",a && GetFileSize(f,NULL)==100);
    if(!a){if(small) CloseHandle(small);CloseHandle(f);return;}
    a[3]=42;
    HANDLE big=CreateFileMappingW(f,NULL,PAGE_READWRITE,0,90000,NULL);
    BYTE *b=big?MapViewOfFile(big,FILE_MAP_WRITE,0,0,0):NULL;
    check("growing a file section preserves an earlier live view",b && b[3]==42 && GetFileSize(f,NULL)==90000);
    if(b) b[4]=43;
    check("independent file sections alias the same prefix",b && a[4]==43);
    HANDLE ro=CreateFileMappingW(f,NULL,PAGE_READONLY,0,0,NULL);
    BYTE *r=ro?MapViewOfFile(ro,FILE_MAP_READ,0,0,0):NULL;
    check("read-only file sections see shared mapped writes",r && r[3]==42 && r[4]==43);
    BYTE *tail=big?MapViewOfFile(big,FILE_MAP_WRITE,0,65536,1000):NULL;
    if(tail) tail[14]=91;
    check("offset writable views alias the whole-file view",b && tail && b[65550]==91);
    BYTE *copy=big?MapViewOfFile(big,FILE_MAP_COPY,0,0,4096):NULL;
    if(copy) copy[3]=99;
    check("copy views of writable file sections stay private",copy && a[3]==42 && copy[3]==99 && FlushViewOfFile(copy,0));
    if(copy) UnmapViewOfFile(copy);
    SetFilePointer(f,10,NULL,FILE_BEGIN);
    check("flush writes mapped ranges without moving the file pointer",FlushViewOfFile(a+3,1) && tail && FlushViewOfFile(tail+14,1) && SetFilePointer(f,0,NULL,FILE_CURRENT)==10 && FlushFileBuffers(f));
    BYTE value=0; DWORD read=0;
    check("flushed prefix reaches file I/O",SetFilePointer(f,3,NULL,FILE_BEGIN)==3 && ReadFile(f,&value,1,&read,NULL) && read==1 && value==42);
    check("flushed offset reaches file I/O",SetFilePointer(f,65550,NULL,FILE_BEGIN)==65550 && ReadFile(f,&value,1,&read,NULL) && read==1 && value==91);
    if(ro) CloseHandle(ro); if(big) CloseHandle(big); CloseHandle(small); CloseHandle(f);
    if(r) { check("views keep the file section alive after handle close",r[3]==42 && r[65550]==91); UnmapViewOfFile(r); }
    if(tail) UnmapViewOfFile(tail); if(b) UnmapViewOfFile(b); UnmapViewOfFile(a);
}

void __stdcall start(void) {
    /* Page-file memory: two views of one section see the same bytes. */
    HANDLE anon = CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 8192, NULL);
    check("page-file section created", anon != NULL);
    BYTE *a = MapViewOfFile(anon, FILE_MAP_ALL_ACCESS, 0, 0, 0);
    BYTE *b = MapViewOfFile(anon, FILE_MAP_READ, 0, 0, 0);
    check("two views of the section", a && b && a != b);
    if (a && b) {
        a[0] = 0x5a; a[8191] = 0xa5;
        check("views share page-file memory", b[0] == 0x5a && b[8191] == 0xa5);
    }
    check("unmap writable view", UnmapViewOfFile(a));
    check("unmap read view", UnmapViewOfFile(b));
    check("close page-file section", CloseHandle(anon));
    SetLastError(0);
    check("page-file section needs a size",
          !CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 0, NULL) && GetLastError() == ERROR_INVALID_PARAMETER);

    /* A file with a recognisable pattern over several granules. */
    WCHAR name[MAX_PATH];
    DWORD n = GetTempPathW(MAX_PATH, name);
    const WCHAR *leaf = L"osyx-mapping.bin";
    for (DWORD i = 0; leaf[i]; i++) name[n++] = leaf[i];
    name[n] = 0;
    HANDLE f = CreateFileW(name, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ, NULL, CREATE_ALWAYS, 0, NULL);
    check("create mapping test file", f != INVALID_HANDLE_VALUE);
    static BYTE data[FILE_BYTES];
    for (DWORD i = 0; i < FILE_BYTES; i++) data[i] = pattern(i);
    DWORD w = 0;
    check("write mapping test file", WriteFile(f, data, FILE_BYTES, &w, NULL) && w == FILE_BYTES);
    check("rewind file", SetFilePointer(f, 10, NULL, FILE_BEGIN) == 10);

    HANDLE ro = CreateFileMappingW(f, NULL, PAGE_READONLY, 0, 0, NULL);
    check("read-only file section of the whole file", ro != NULL);
    BYTE *v = MapViewOfFile(ro, FILE_MAP_READ, 0, 0, 0);
    check("whole-file view has the file bytes", v && mem_eq(v, data, FILE_BYTES));
    MEMORY_BASIC_INFORMATION mbi;
    check("VirtualQuery reports a mapped view",
          v && VirtualQuery(v + 5000, &mbi, sizeof mbi) == sizeof mbi && mbi.AllocationBase == v &&
              mbi.Type == MEM_MAPPED && mbi.State == MEM_COMMIT && mbi.Protect == PAGE_READONLY);
    check("unmap by an address inside the view", UnmapViewOfFile(v + 4096));
    LARGE_INTEGER pos, zero = {0};
    check("mapping leaves the file pointer alone", SetFilePointerEx(f, zero, &pos, FILE_CURRENT) && pos.QuadPart == 10);

    BYTE *mid = MapViewOfFile(ro, FILE_MAP_READ, 0, 2 * 65536, 0);
    check("view at a 128 KiB offset to the end", mid && mem_eq(mid, data + 2 * 65536, FILE_BYTES - 2 * 65536));
    check("unmap offset view", UnmapViewOfFile(mid));
    BYTE *part = MapViewOfFile(ro, FILE_MAP_READ, 0, 65536, 100);
    check("100-byte view at 64 KiB", part && mem_eq(part, data + 65536, 100));
    check("unmap partial view", UnmapViewOfFile(part));

    SetLastError(0);
    check("unaligned offset rejected",
          !MapViewOfFile(ro, FILE_MAP_READ, 0, 4096, 0) && GetLastError() == ERROR_MAPPED_ALIGNMENT);
    SetLastError(0);
    check("view past the section rejected",
          !MapViewOfFile(ro, FILE_MAP_READ, 0, 65536, FILE_BYTES) && GetLastError() == ERROR_ACCESS_DENIED);
    SetLastError(0);
    check("write view of a read-only section rejected",
          !MapViewOfFile(ro, FILE_MAP_WRITE, 0, 0, 0) && GetLastError() == ERROR_ACCESS_DENIED);

    /* Copy-on-write: writes stay private to the view. */
    BYTE *cow = MapViewOfFile(ro, FILE_MAP_COPY, 0, 0, 4096);
    check("copy-on-write view", cow && cow[1] == pattern(1));
    if (cow) cow[1] ^= 0xff;
    BYTE *again = MapViewOfFile(ro, FILE_MAP_READ, 0, 0, 4096);
    check("copy-on-write changes stay private", again && cow && again[1] == pattern(1) && cow[1] == (BYTE)~pattern(1));
    BYTE first = 0; DWORD r = 0;
    check("file is unchanged by copy-on-write",
          SetFilePointer(f, 1, NULL, FILE_BEGIN) == 1 && ReadFile(f, &first, 1, &r, NULL) && r == 1 && first == pattern(1));
    UnmapViewOfFile(again);
    UnmapViewOfFile(cow);
    check("close read-only section", CloseHandle(ro));

    SetLastError(0);
    check("section larger than a read-only file rejected",
          !CreateFileMappingW(f, NULL, PAGE_READONLY, 0, FILE_BYTES + 1, NULL) && GetLastError() == ERROR_NOT_ENOUGH_MEMORY);
    HANDLE small = CreateFileMappingW(f, NULL, PAGE_WRITECOPY, 0, 70000, NULL);
    check("write-copy section of a file prefix", small != NULL);
    SetLastError(0);
    check("view beyond a prefix section rejected",
          !MapViewOfFile(small, FILE_MAP_READ, 0, 65536, 5000) && GetLastError() == ERROR_ACCESS_DENIED);
    BYTE *tail = MapViewOfFile(small, FILE_MAP_COPY, 0, 65536, 0);
    check("prefix section view ends at its size", tail && mem_eq(tail, data + 65536, 70000 - 65536));
    UnmapViewOfFile(tail);
    CloseHandle(small);

    /* Process-local names resolve to the same section. */
    HANDLE named = CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 4096, L"osyx-mapping-test");
    SetLastError(0);
    HANDLE same = CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 4096, L"osyx-mapping-test");
    check("named section opened again", named && same && GetLastError() == ERROR_ALREADY_EXISTS);
    BYTE *n1 = MapViewOfFile(named, FILE_MAP_WRITE, 0, 0, 0), *n2 = MapViewOfFile(same, FILE_MAP_READ, 0, 0, 0);
    if (n1 && n2) n1[9] = 42;
    check("named handles share memory", n1 && n2 && n2[9] == 42);
    UnmapViewOfFile(n1); UnmapViewOfFile(n2);
    CloseHandle(same); CloseHandle(named);

    CloseHandle(f);
    HANDLE empty = CreateFileW(name, GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    SetLastError(0);
    check("empty file cannot be mapped",
          !CreateFileMappingW(empty, NULL, PAGE_READONLY, 0, 0, NULL) && GetLastError() == ERROR_FILE_INVALID);
    CloseHandle(empty);
    writable_file_views(name);
    SetLastError(0);
    /* (The image itself is a view on Windows; use private memory.) */
    BYTE *priv = VirtualAlloc(NULL, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    check("unmapping private memory fails", priv && !UnmapViewOfFile(priv) && GetLastError() == ERROR_INVALID_ADDRESS);
    VirtualFree(priv, 0, MEM_RELEASE);
    DeleteFileW(name);
    finish("mapping");
}
