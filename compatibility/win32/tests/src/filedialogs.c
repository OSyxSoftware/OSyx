/* Interactive common-dialog ABI regression, driven by desktop-test.ps1. */
#include "common.h"
#include <commdlg.h>
#include <shlobj.h>

_Static_assert(sizeof(OPENFILENAMEW)==152, "OPENFILENAME x64 ABI");
_Static_assert(sizeof(BROWSEINFOW)==64, "BROWSEINFO x64 ABI");
static int wide_eq(const WCHAR *a,const WCHAR *b){while(*a && *a==*b){a++;b++;}return *a==*b;}
static int ansi_eq(const char *a,const char *b){while(*a && *a==*b){a++;b++;}return *a==*b;}
static void wide_copy(WCHAR *d,const WCHAR *s){while((*d++=*s++));}
static void ansi_copy(char *d,const char *s){while((*d++=*s++));}

void start(void){
    WNDCLASSW wc;memset(&wc,0,sizeof(wc));wc.lpfnWndProc=DefWindowProcW;wc.lpszClassName=L"FileDialogTest";
    RegisterClassW(&wc);
    HWND owner=CreateWindowExW(0,wc.lpszClassName,L"System file dialog test",WS_OVERLAPPEDWINDOW|WS_VISIBLE,60,60,500,300,NULL,NULL,NULL,NULL);
    OPENFILENAMEW w;memset(&w,0,sizeof(w));
    check("dialog rejects invalid structure size",!GetOpenFileNameW(&w) && CommDlgExtendedError()==CDERR_STRUCTSIZE);
    WCHAR file[512]=L"Welcome.txt",title[64]=L"",before[512],after[512];
    GetCurrentDirectoryW(512,before);
    w.lStructSize=sizeof(w);w.hwndOwner=owner;w.lpstrFile=file;w.nMaxFile=512;
    w.lpstrInitialDir=L"C:\\home\\user\\Documents";
    w.lpstrFileTitle=title;w.nMaxFileTitle=64;w.lpstrTitle=L"Test: Open UTF-16";
    w.lpstrFilter=L"Text files\0*.txt\0All files\0*.*\0\0";w.nFilterIndex=1;
    w.Flags=OFN_EXPLORER|OFN_FILEMUSTEXIST|OFN_NOCHANGEDIR;
    check("Open W returns selected file",GetOpenFileNameW(&w) && wide_eq(file,L"C:\\home\\user\\Documents\\Welcome.txt"));
    check("Open W returns filename offsets and title",wide_eq(file+w.nFileOffset,L"Welcome.txt") && wide_eq(file+w.nFileExtension,L"txt") && wide_eq(title,L"Welcome.txt"));
    GetCurrentDirectoryW(512,after);
    check("OFN_NOCHANGEDIR preserves working directory",wide_eq(before,after));
    check("modal picker restores owner enabled state",IsWindowEnabled(owner));
    OPENFILENAMEA a;memset(&a,0,sizeof(a));char name[512]="picker-save";
    a.lStructSize=sizeof(a);a.hwndOwner=owner;a.lpstrFile=name;a.nMaxFile=512;
    a.lpstrInitialDir="C:\\home\\user\\Documents";a.lpstrTitle="Test: Save ANSI";a.lpstrDefExt="txt";
    a.Flags=OFN_EXPLORER|OFN_OVERWRITEPROMPT|OFN_NOCHANGEDIR;
    check("Save A appends default extension",GetSaveFileNameA(&a) && ansi_eq(name,"C:\\home\\user\\Documents\\picker-save.txt"));
    HANDLE h=CreateFileA(name,GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);
    check("selected save path is writable",h!=INVALID_HANDLE_VALUE);if(h!=INVALID_HANDLE_VALUE){CloseHandle(h);}
    wide_copy(file,L"picker-save.txt");w.lpstrTitle=L"Test: Replace file";w.Flags=OFN_EXPLORER|OFN_OVERWRITEPROMPT|OFN_NOCHANGEDIR;
    check("Save W confirms replacement",GetSaveFileNameW(&w));
    BROWSEINFOW browse;memset(&browse,0,sizeof(browse));WCHAR display[260]=L"",folder[260];
    browse.hwndOwner=owner;browse.pszDisplayName=display;browse.lpszTitle=L"Test: Select folder";browse.ulFlags=BIF_RETURNONLYFSDIRS|BIF_NEWDIALOGSTYLE;
    PIDLIST_ABSOLUTE pidl=SHBrowseForFolderW(&browse);
    check("folder picker returns filesystem PIDL",pidl && SHGetPathFromIDListW(pidl,folder) && wide_eq(folder,L"C:\\home\\user"));
    check("folder picker returns display name",wide_eq(display,L"user"));CoTaskMemFree(pidl);
    wide_copy(file,L"unchanged.txt");w.lpstrTitle=L"Test: Cancel";
    check("cancel preserves file buffer and clears extended error",!GetOpenFileNameW(&w) && CommDlgExtendedError()==0 && wide_eq(file,L"unchanged.txt"));
    char small[12]="Welcome.txt";a.lpstrFile=small;a.nMaxFile=12;a.lpstrTitle="Test: Small buffer";a.Flags=OFN_EXPLORER|OFN_FILEMUSTEXIST|OFN_NOCHANGEDIR;
    check("small ANSI output reports required buffer length",!GetOpenFileNameA(&a) && CommDlgExtendedError()==FNERR_BUFFERTOOSMALL && *(WORD*)small>12);
    ansi_copy(name,"Welcome.txt");a.lpstrFile=name;a.nMaxFile=512;a.Flags=OFN_ALLOWMULTISELECT;
    check("unsupported multi-selection fails explicitly",!GetOpenFileNameA(&a) && CommDlgExtendedError()!=0);
    DestroyWindow(owner);finish("filedialogs");
}
