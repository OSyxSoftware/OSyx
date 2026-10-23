#define _WIN32_WINNT 0x0602
#define PSAPI_VERSION 2
#include "common.h"
#include <psapi.h>
static SRWLOCK rw = SRWLOCK_INIT;
static CONDITION_VARIABLE cv = CONDITION_VARIABLE_INIT;
static INIT_ONCE once = INIT_ONCE_STATIC_INIT;
static LONG calls, payload = 42;
static LONG retry_calls;
static volatile LONG predicate, address;
static HANDLE reader_entered, reader_leave;
static BOOL CALLBACK initialize(PINIT_ONCE o, PVOID p, PVOID *context) {
    (void)o; (void)p; InterlockedIncrement(&calls); Sleep(20); *context=&payload; return TRUE;
}
static BOOL CALLBACK retry_initialize(PINIT_ONCE o, PVOID p, PVOID *context) {
    (void)o; (void)p;
    if (InterlockedIncrement(&retry_calls)==1) { SetLastError(ERROR_GEN_FAILURE); return FALSE; }
    *context=&payload; return TRUE;
}
static DWORD WINAPI initialize_thread(void *arg) {
    (void)arg; void *context=NULL;
    return InitOnceExecuteOnce(&once,initialize,NULL,&context) && context==&payload ? 11 : 90;
}
static DWORD WINAPI reader_thread(void *arg) {
    (void)arg; AcquireSRWLockShared(&rw); SetEvent(reader_entered);
    WaitForSingleObject(reader_leave,INFINITE); ReleaseSRWLockShared(&rw); return 12;
}
static DWORD WINAPI cv_thread(void *arg) {
    (void)arg; AcquireSRWLockExclusive(&rw);
    while (!predicate) { if (!SleepConditionVariableSRW(&cv,&rw,1000,0)) { ReleaseSRWLockExclusive(&rw);return 91; } }
    ReleaseSRWLockExclusive(&rw); return 13;
}
static DWORD WINAPI address_thread(void *arg) {
    (void)arg; LONG zero=0;
    while (!InterlockedCompareExchange(&address,0,0)) { if (!WaitOnAddress(&address,&zero,sizeof zero,1000)) return 92; }
    return 14;
}
static DWORD joined(HANDLE h) {
    DWORD code=0; if (WaitForSingleObject(h,2000)!=WAIT_OBJECT_0) return 99;
    GetExitCodeThread(h,&code);CloseHandle(h);return code;
}
void __stdcall start(void) {
    check("exclusive SRW try acquire",TryAcquireSRWLockExclusive(&rw));
    check("reader blocked by writer",!TryAcquireSRWLockShared(&rw));
    ReleaseSRWLockExclusive(&rw);
    reader_entered=CreateEventW(NULL,TRUE,FALSE,NULL);reader_leave=CreateEventW(NULL,TRUE,FALSE,NULL);
    AcquireSRWLockShared(&rw);
    HANDLE reader=CreateThread(NULL,0,reader_thread,NULL,0,NULL);
    check("independent readers can hold SRW simultaneously",WaitForSingleObject(reader_entered,1000)==WAIT_OBJECT_0);
    check("writer blocked by readers",!TryAcquireSRWLockExclusive(&rw));
    SetEvent(reader_leave);ReleaseSRWLockShared(&rw);check("shared reader exits",joined(reader)==12);
    CloseHandle(reader_entered);CloseHandle(reader_leave);
    HANDLE workers[4];for(int i=0;i<4;i++) workers[i]=CreateThread(NULL,0,initialize_thread,NULL,0,NULL);
    int all=1;for(int i=0;i<4;i++) all &= joined(workers[i])==11;
    check("InitOnce returns same context to four threads",all);
    check("InitOnce invokes callback exactly once",calls==1);
    INIT_ONCE retry=INIT_ONCE_STATIC_INIT;void *context=NULL;
    check("failed once initialization reports failure",!InitOnceExecuteOnce(&retry,retry_initialize,NULL,&context)&&GetLastError()==ERROR_GEN_FAILURE);
    check("failed once initialization can retry",InitOnceExecuteOnce(&retry,retry_initialize,NULL,&context)&&context==&payload&&retry_calls==2);
    /* InitOnceBeginInitialize / InitOnceComplete (codes checked on Windows). */
    INIT_ONCE bo=INIT_ONCE_STATIC_INIT,ao=INIT_ONCE_STATIC_INIT;BOOL once_pending=7;void *got=NULL;
    check("InitOnce check-only before initialization",!InitOnceBeginInitialize(&bo,INIT_ONCE_CHECK_ONLY,&once_pending,&got)&&GetLastError()==ERROR_GEN_FAILURE);
    check("InitOnce check-only with async is invalid",!InitOnceBeginInitialize(&bo,INIT_ONCE_CHECK_ONLY|INIT_ONCE_ASYNC,&once_pending,&got)&&GetLastError()==ERROR_INVALID_PARAMETER);
    check("InitOnceBeginInitialize makes the caller initialize",InitOnceBeginInitialize(&bo,0,&once_pending,NULL)&&once_pending);
    check("InitOnceComplete rejects reserved context bits",!InitOnceComplete(&bo,0,(void*)3)&&GetLastError()==ERROR_INVALID_PARAMETER);
    check("InitOnceComplete(INIT_FAILED) lets a later caller retry",InitOnceComplete(&bo,INIT_ONCE_INIT_FAILED,NULL)&&InitOnceBeginInitialize(&bo,0,&once_pending,&got)&&once_pending);
    check("InitOnceComplete stores the context",InitOnceComplete(&bo,0,(void*)&payload));
    check("a second InitOnceComplete fails",!InitOnceComplete(&bo,0,(void*)&payload)&&GetLastError()==ERROR_GEN_FAILURE);
    check("InitOnce check-only returns the context",InitOnceBeginInitialize(&bo,INIT_ONCE_CHECK_ONLY,&once_pending,&got)&&!once_pending&&got==&payload);
    check("two async initializers both proceed",InitOnceBeginInitialize(&ao,INIT_ONCE_ASYNC,&once_pending,&got)&&once_pending&&InitOnceBeginInitialize(&ao,INIT_ONCE_ASYNC,&once_pending,&got)&&once_pending);
    check("async completion cannot report failure",!InitOnceComplete(&ao,INIT_ONCE_ASYNC|INIT_ONCE_INIT_FAILED,NULL)&&GetLastError()==ERROR_INVALID_PARAMETER);
    check("the first async completion wins",InitOnceComplete(&ao,INIT_ONCE_ASYNC,(void*)&payload)&&!InitOnceComplete(&ao,INIT_ONCE_ASYNC,(void*)&calls)&&GetLastError()==ERROR_GEN_FAILURE);
    check("later callers see the winner",InitOnceBeginInitialize(&ao,INIT_ONCE_ASYNC,&once_pending,&got)&&!once_pending&&got==&payload);
    AcquireSRWLockExclusive(&rw);
    check("condition variable timeout reports ERROR_TIMEOUT",!SleepConditionVariableSRW(&cv,&rw,20,0)&&GetLastError()==ERROR_TIMEOUT);
    check("condition timeout reacquires lock",!TryAcquireSRWLockExclusive(&rw));
    ReleaseSRWLockExclusive(&rw);
    AcquireSRWLockShared(&rw);
    check("shared condition timeout reacquires a reader lock",!SleepConditionVariableSRW(&cv,&rw,10,CONDITION_VARIABLE_LOCKMODE_SHARED)&&GetLastError()==ERROR_TIMEOUT&&!TryAcquireSRWLockExclusive(&rw));
    ReleaseSRWLockShared(&rw);
    HANDLE waiter=CreateThread(NULL,0,cv_thread,NULL,0,NULL);Sleep(30);
    AcquireSRWLockExclusive(&rw);predicate=1;WakeConditionVariable(&cv);ReleaseSRWLockExclusive(&rw);
    check("condition variable wakes and rechecks predicate",joined(waiter)==13);
    LONG different=1;check("WaitOnAddress mismatched value returns immediately",WaitOnAddress(&address,&different,4,0));
    LONG zero=0;check("WaitOnAddress zero timeout",!WaitOnAddress(&address,&zero,4,0)&&GetLastError()==ERROR_TIMEOUT);
    check("unsupported address size fails explicitly",!WaitOnAddress(&address,&zero,1,10)&&GetLastError()==ERROR_NOT_SUPPORTED);
    waiter=CreateThread(NULL,0,address_thread,NULL,0,NULL);Sleep(30);InterlockedExchange(&address,1);WakeByAddressAll((void *)&address);
    check("WaitOnAddress uses real futex wakeup",joined(waiter)==14);
    DWORD needed=0;check("module enumeration size query",K32EnumProcessModules(GetCurrentProcess(),NULL,0,&needed)&&needed>=16);
    HMODULE modules[64];int main_seen=0;
    BOOL enumerated=K32EnumProcessModules(GetCurrentProcess(),modules,sizeof modules,&needed);
    for(DWORD i=0;i<needed/sizeof(HMODULE)&&i<64;i++) if(modules[i]==GetModuleHandleW(NULL))main_seen=1;
    check("module list contains actual executable handle",enumerated&&main_seen);
    modules[1]=(HMODULE)123;
    check("module enumeration honors byte capacity",K32EnumProcessModules(GetCurrentProcess(),modules,sizeof(HMODULE),&needed)&&modules[1]==(HMODULE)123&&needed>sizeof(HMODULE));
    check("remote process handles are not fabricated",!K32EnumProcessModules((HANDLE)123,modules,sizeof modules,&needed)&&GetLastError()==ERROR_INVALID_HANDLE);
    LARGE_INTEGER due; due.QuadPart=-200000; /* relative 20 ms */
    SECURITY_ATTRIBUTES security={sizeof(SECURITY_ATTRIBUTES),NULL,FALSE};
    check("timer security attributes fail explicitly",!CreateWaitableTimerExW(&security,NULL,0,TIMER_ALL_ACCESS)&&GetLastError()==ERROR_NOT_SUPPORTED);
    check("restricted timer access fails explicitly",!CreateWaitableTimerExW(NULL,NULL,0,TIMER_MODIFY_STATE)&&GetLastError()==ERROR_NOT_SUPPORTED);
    HANDLE timer=CreateWaitableTimerExW(NULL,NULL,0,TIMER_ALL_ACCESS);
    check("create waitable timer",timer!=NULL);
    ULONGLONG started=GetTickCount64();
    check("arm real relative timer",SetWaitableTimerEx(timer,&due,0,NULL,NULL,NULL,0));
    DWORD waited=WaitForSingleObject(timer,1000);
    check("wait blocks until timer deadline",waited==WAIT_OBJECT_0&&GetTickCount64()-started>=20);
    check("automatic timer signal is consumed",WaitForSingleObject(timer,0)==WAIT_TIMEOUT);
    FILETIME wall; GetSystemTimeAsFileTime(&wall);
    due.QuadPart=(LONGLONG)(((ULONGLONG)wall.dwHighDateTime<<32)|wall.dwLowDateTime)+300000;
    HANDLE pending=CreateEventW(NULL,TRUE,FALSE,NULL), handles[2]={pending,timer};
    started=GetTickCount64();
    check("absolute FILETIME timer participates in wait-multiple",SetWaitableTimerEx(timer,&due,0,NULL,NULL,NULL,0)&&WaitForMultipleObjects(2,handles,FALSE,1000)==WAIT_OBJECT_0+1&&GetTickCount64()-started>=20);
    CloseHandle(pending); due.QuadPart=-200000;
    check("periodic timer can be armed",SetWaitableTimer(timer,&due,20,NULL,NULL,FALSE));
    check("periodic timer fires twice",WaitForSingleObject(timer,1000)==WAIT_OBJECT_0&&WaitForSingleObject(timer,1000)==WAIT_OBJECT_0);
    /* A period may elapse before the cancel; cancelling keeps that signal. */
    BOOL cancelled=CancelWaitableTimer(timer); WaitForSingleObject(timer,0);
    check("cancel removes pending deadline",cancelled&&WaitForSingleObject(timer,60)==WAIT_TIMEOUT);
    check("timer APC is explicitly unsupported",!SetWaitableTimerEx(timer,&due,0,(PTIMERAPCROUTINE)1,NULL,NULL,0)&&GetLastError()==ERROR_NOT_SUPPORTED);
    CloseHandle(timer);
    timer=CreateWaitableTimerW(NULL,TRUE,NULL);
    check("manual reset timer stays signaled",SetWaitableTimer(timer,&due,0,NULL,NULL,FALSE)&&WaitForSingleObject(timer,1000)==WAIT_OBJECT_0&&WaitForSingleObject(timer,0)==WAIT_OBJECT_0);
    check("rearming manual reset timer clears signal",SetWaitableTimer(timer,&due,0,NULL,NULL,FALSE)&&WaitForSingleObject(timer,0)==WAIT_TIMEOUT);
    Sleep(30);
    check("cancellation preserves elapsed timer signal",CancelWaitableTimer(timer)&&WaitForSingleObject(timer,0)==WAIT_OBJECT_0);
    CloseHandle(timer);
    finish("synch");
}
