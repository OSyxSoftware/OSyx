/* Target C: Windows file I/O on OSyx file systems: drive letters and
 * backslashes, create/write/read/seek, sizes and attributes,
 * case-insensitive names, sharing modes, delete-on-close, directories,
 * error codes. C: is the OSyx root; \tmp is a tmpfs. */
#include "common.h"

static const char text[] = "Windows file I/O on OSyx: ext4, FAT32 and tmpfs behind C:\\\r\n";

void __stdcall start(void) {
    const char *name = "C:\\tmp\\WinTest.txt";
    HANDLE h = CreateFileA(name, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    check("CreateFileA(CREATE_ALWAYS)", h != INVALID_HANDLE_VALUE);
    DWORD w = 0;
    check("WriteFile", WriteFile(h, text, sizeof text - 1, &w, NULL) && w == sizeof text - 1);
    check("CloseHandle", CloseHandle(h));

    /* CREATE_NEW on an existing file fails with ERROR_FILE_EXISTS. */
    SetLastError(0);
    h = CreateFileA(name, GENERIC_WRITE, 0, NULL, CREATE_NEW, 0, NULL);
    check("CREATE_NEW on an existing file fails", h == INVALID_HANDLE_VALUE);
    check_eq("... with ERROR_FILE_EXISTS", GetLastError(), ERROR_FILE_EXISTS);

    /* Different case and forward slashes reach the same file. */
    h = CreateFileA("c:/TMP/wintest.TXT", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    check("case-insensitive open with / separators", h != INVALID_HANDLE_VALUE);
    check_eq("GetFileSize", GetFileSize(h, NULL), sizeof text - 1);
    char buf[128] = {0};
    DWORD r = 0;
    check("ReadFile returns the data", ReadFile(h, buf, sizeof buf, &r, NULL) && r == sizeof text - 1 && mem_eq(buf, text, r));
    check("ReadFile at end of file reads 0 bytes", ReadFile(h, buf, sizeof buf, &r, NULL) && r == 0);
    check_eq("SetFilePointer(FILE_BEGIN, 8)", SetFilePointer(h, 8, NULL, FILE_BEGIN), 8);
    check("ReadFile after seeking", ReadFile(h, buf, 4, &r, NULL) && r == 4 && mem_eq(buf, "file", 4));
    LARGE_INTEGER pos, zero = {0};
    check("SetFilePointerEx(FILE_CURRENT)", SetFilePointerEx(h, zero, &pos, FILE_CURRENT) && pos.QuadPart == 12);

    /* A second opener asking for write access while we only share read. */
    SetLastError(0);
    HANDLE h2 = CreateFileA(name, GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    check("sharing mode blocks a conflicting open", h2 == INVALID_HANDLE_VALUE);
    check_eq("... with ERROR_SHARING_VIOLATION", GetLastError(), ERROR_SHARING_VIOLATION);
    h2 = CreateFileA(name, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    check("a compatible second open succeeds", h2 != INVALID_HANDLE_VALUE);
    BY_HANDLE_FILE_INFORMATION i1, i2;
    check("GetFileInformationByHandle: same file identity",
          GetFileInformationByHandle(h, &i1) && GetFileInformationByHandle(h2, &i2) && i1.nFileIndexLow == i2.nFileIndexLow &&
              i1.nFileIndexHigh == i2.nFileIndexHigh && i1.dwVolumeSerialNumber == i2.dwVolumeSerialNumber && i1.nFileSizeLow == sizeof text - 1);
    CloseHandle(h2);
    CloseHandle(h);

    DWORD attr = GetFileAttributesA(name);
    check("GetFileAttributesA: a normal file", attr != INVALID_FILE_ATTRIBUTES && !(attr & FILE_ATTRIBUTE_DIRECTORY));
    check("GetFileAttributesA: \\tmp is a directory", GetFileAttributesA("C:\\tmp") & FILE_ATTRIBUTE_DIRECTORY);
    check("CreateDirectoryA", CreateDirectoryA("C:\\tmp\\WinDir", NULL));
    SetLastError(0);
    check("CreateDirectoryA again fails", !CreateDirectoryA("C:\\tmp\\windir", NULL));
    check_eq("... with ERROR_ALREADY_EXISTS", GetLastError(), ERROR_ALREADY_EXISTS);
    check("MoveFileA into the directory", MoveFileA(name, "C:\\tmp\\WinDir\\moved.txt"));
    check("RemoveDirectoryA on a non-empty directory fails", !RemoveDirectoryA("C:\\tmp\\WinDir"));
    check_eq("... with ERROR_DIR_NOT_EMPTY", GetLastError(), ERROR_DIR_NOT_EMPTY);
    check("DeleteFileA", DeleteFileA("C:\\tmp\\WinDir\\MOVED.TXT"));
    check("RemoveDirectoryA", RemoveDirectoryA("C:\\tmp\\WinDir"));
    SetLastError(0);
    check("GetFileAttributesA of a deleted file fails", GetFileAttributesA(name) == INVALID_FILE_ATTRIBUTES);
    check_eq("... with ERROR_FILE_NOT_FOUND", GetLastError(), ERROR_FILE_NOT_FOUND);
    SetLastError(0);
    CreateFileA("C:\\tmp\\no\\such\\dir\\x.txt", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
    check_eq("missing directory: ERROR_PATH_NOT_FOUND", GetLastError(), ERROR_PATH_NOT_FOUND);

    /* Delete-on-close: the file disappears with its last handle. */
    h = CreateFileA("C:\\tmp\\temp.bin", GENERIC_WRITE, FILE_SHARE_DELETE, NULL, CREATE_ALWAYS, FILE_FLAG_DELETE_ON_CLOSE, NULL);
    check("CreateFileA(FILE_FLAG_DELETE_ON_CLOSE)", h != INVALID_HANDLE_VALUE);
    check("file exists while open", GetFileAttributesA("C:\\tmp\\temp.bin") != INVALID_FILE_ATTRIBUTES);
    CloseHandle(h);
    check("file is gone after CloseHandle", GetFileAttributesA("C:\\tmp\\temp.bin") == INVALID_FILE_ATTRIBUTES);

    /* Wide-character API on an ext4 file written by OSyx's build. */
    h = CreateFileW(L"C:\\home\\user\\Documents\\Welcome.txt", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    check("CreateFileW reads an ext4 file", h != INVALID_HANDLE_VALUE && GetFileSize(h, NULL) > 0);
    CloseHandle(h);
    char cwd[MAX_PATH];
    check("GetCurrentDirectoryA is a drive path", GetCurrentDirectoryA(MAX_PATH, cwd) >= 3 && cwd[1] == ':' && cwd[2] == '\\');
    finish("fileio");
}
