/* Power status: GetSystemPowerStatus, CallNtPowerInformation (battery state
 * and power capabilities), GetPwrCapabilities, PowerDeterminePlatformRole.
 * The values depend on the machine (QEMU has no battery unless it is given
 * scripts/acpi/qemu-battery.aml); the checks are the relations that hold on
 * any machine, with or without one. Also runs on Windows. */
#include "common.h"
#include <powrprof.h>

#ifndef STATUS_BUFFER_TOO_SMALL
#define STATUS_BUFFER_TOO_SMALL ((LONG)0xC0000023)
#endif

static void put_field(const char *name, long long v) {
    put("       "); put(name); put(" = ");
    if (v < 0) { put("-"); v = -v; }
    put_uint((unsigned long long)v); put("\r\n");
}

void __stdcall start(void) {
    SYSTEM_POWER_STATUS s;
    memset(&s, 0x5A, sizeof s);
    check("GetSystemPowerStatus", GetSystemPowerStatus(&s));
    put_field("ACLineStatus", s.ACLineStatus);
    put_field("BatteryFlag", s.BatteryFlag);
    put_field("BatteryLifePercent", s.BatteryLifePercent);
    put_field("BatteryLifeTime", (LONG)s.BatteryLifeTime);
    check("AC line is offline, online or unknown", s.ACLineStatus == 0 || s.ACLineStatus == 1 || s.ACLineStatus == 255);
    check("no battery saver", s.SystemStatusFlag == 0);
    int none = s.BatteryFlag == 128, unknown = s.BatteryFlag == 255;
    if (none) {
        check("without a battery: on AC power, percentage unknown", s.ACLineStatus == 1 && s.BatteryLifePercent == 255);
        check("without a battery: no remaining time", s.BatteryLifeTime == (DWORD)-1);
    } else if (!unknown) {
        check("with a battery: percentage 0 to 100", s.BatteryLifePercent <= 100);
        check("with a battery: no flag beyond high, low, critical, charging", (s.BatteryFlag & ~(1 | 2 | 4 | 8)) == 0);
        check("high and low exclude each other", (s.BatteryFlag & 3) != 3);
        check("high means above 66 %, low below 33 %",
              (!(s.BatteryFlag & 1) || s.BatteryLifePercent > 66) && (!(s.BatteryFlag & 2) || s.BatteryLifePercent < 33));
        check("critical means below 5 %", !(s.BatteryFlag & 4) || s.BatteryLifePercent < 5);
        check("a remaining time only on battery power", s.BatteryLifeTime == (DWORD)-1 || s.ACLineStatus == 0);
    }
    check("full lifetime unknown or positive", s.BatteryFullLifeTime == (DWORD)-1 || (LONG)s.BatteryFullLifeTime > 0);

    SYSTEM_BATTERY_STATE b;
    memset(&b, 0x5A, sizeof b);
    LONG st = CallNtPowerInformation(SystemBatteryState, NULL, 0, &b, sizeof b);
    check("CallNtPowerInformation(SystemBatteryState)", st == 0);
    put_field("BatteryPresent", b.BatteryPresent);
    put_field("Charging", b.Charging);
    put_field("Discharging", b.Discharging);
    put_field("MaxCapacity", b.MaxCapacity);
    put_field("RemainingCapacity", b.RemainingCapacity);
    put_field("Rate", (LONG)b.Rate);
    put_field("EstimatedTime", (LONG)b.EstimatedTime);
    check("battery presence agrees with GetSystemPowerStatus", none ? !b.BatteryPresent : (unknown || b.BatteryPresent));
    check("AC line agrees with GetSystemPowerStatus", s.ACLineStatus == 255 || (b.AcOnLine != 0) == (s.ACLineStatus == 1));
    check("not charging and discharging at once", !(b.Charging && b.Discharging));
    check("charging agrees with the charging flag", unknown || none || (b.Charging != 0) == ((s.BatteryFlag & 8) != 0));
    if (b.BatteryPresent) {
        check("remaining capacity within the full capacity", b.MaxCapacity == 0 || b.RemainingCapacity <= b.MaxCapacity);
        check("the rate is negative while discharging", !b.Discharging || (LONG)b.Rate <= 0);
        check("the rate is positive while charging", !b.Charging || (LONG)b.Rate >= 0);
        check("an estimated time only while discharging", b.EstimatedTime == (DWORD)-1 || b.Discharging);
    } else {
        check("no battery: no charge, no time", !b.Charging && !b.Discharging && b.EstimatedTime == (DWORD)-1);
    }
    char small[8];
    check("a short buffer is refused", CallNtPowerInformation(SystemBatteryState, NULL, 0, small, sizeof small) == STATUS_BUFFER_TOO_SMALL);

    SYSTEM_POWER_CAPABILITIES c, c2;
    memset(&c, 0x5A, sizeof c);
    memset(&c2, 0xA5, sizeof c2);
    check("GetPwrCapabilities", GetPwrCapabilities(&c));
    check("CallNtPowerInformation(SystemPowerCapabilities)", CallNtPowerInformation(SystemPowerCapabilities, NULL, 0, &c2, sizeof c2) == 0);
    check("a power button and soft off", c.PowerButtonPresent && c.SystemS5);
    check("batteries agree with the battery state", (c.SystemBatteriesPresent != 0) == (b.BatteryPresent != 0));
    check("both calls agree", c.PowerButtonPresent == c2.PowerButtonPresent && c.LidPresent == c2.LidPresent
          && c.SystemBatteriesPresent == c2.SystemBatteriesPresent && c.SystemS5 == c2.SystemS5);
    put_field("LidPresent", c.LidPresent);
    put_field("ThermalControl", c.ThermalControl);

    POWER_PLATFORM_ROLE role = PowerDeterminePlatformRole();
    put_field("platform role", role);
    check("a mobile platform has a battery, a desktop has none",
          b.BatteryPresent ? role == PlatformRoleMobile : role == PlatformRoleDesktop);
    finish("power");
}
