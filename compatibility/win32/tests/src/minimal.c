/* Target A: the smallest useful PE32+ program. Imports only ExitProcess;
 * its exit code goes through an absolute pointer, so it is only right if
 * the loader applied the DIR64 base relocation. No C runtime. */
#include <windows.h>

static int value = 21;
static int *volatile pointer = &value; /* needs a base relocation */

void __stdcall start(void) {
    ExitProcess(*pointer * 2); /* 42 */
}
