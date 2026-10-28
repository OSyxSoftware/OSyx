/* winmm multimedia timers: timeGetTime follows the monotonic clock, and
 * timeBeginPeriod/timeEndPeriod accept exactly the periods timeGetDevCaps
 * reports (on OSyx the 1 ms kernel tick; a finer request is refused).
 * The same PE runs on Windows. */
#include "common.h"
#include <mmsystem.h>

void __stdcall start(void) {
    char marker[8];
    int native = GetEnvironmentVariableA("OSYX", marker, sizeof marker) > 0;

    TIMECAPS caps = {0, 0};
    check("timeGetDevCaps", timeGetDevCaps(&caps, sizeof caps) == TIMERR_NOERROR && caps.wPeriodMin >= 1 && caps.wPeriodMax >= caps.wPeriodMin);
    check("timeGetDevCaps without a buffer", timeGetDevCaps(NULL, sizeof caps) != TIMERR_NOERROR);
    if (native) check_eq("OSyx period minimum is the kernel tick", caps.wPeriodMin, 1);

    DWORD t1 = timeGetTime(), tick = GetTickCount();
    check("timeGetTime agrees with GetTickCount", (DWORD)(t1 - tick) < 100 || (DWORD)(tick - t1) < 100);
    Sleep(50);
    DWORD elapsed = timeGetTime() - t1;
    check("timeGetTime advances with Sleep", elapsed >= 40 && elapsed < 1000);

    check("timeBeginPeriod at the minimum", timeBeginPeriod(caps.wPeriodMin) == TIMERR_NOERROR);
    check("timeEndPeriod matches it", timeEndPeriod(caps.wPeriodMin) == TIMERR_NOERROR);
    check_eq("timeBeginPeriod(0)", timeBeginPeriod(0), TIMERR_NOCANDO);
    if (native) {
        check("the finest period the tick provides is accepted", timeBeginPeriod(1) == TIMERR_NOERROR);
        check("its timeEndPeriod matches", timeEndPeriod(1) == TIMERR_NOERROR);
        check_eq("an unmatched timeEndPeriod", timeEndPeriod(caps.wPeriodMin), TIMERR_NOCANDO);
    }
    finish("winmm");
}
