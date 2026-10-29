//! Wireless chips by PCI id (Wi-Fi) and USB id (Bluetooth).
//!
//! Lists the chips of popular laptops, including both Lenovo ThinkPad
//! T495 options (Intel Wi-Fi 6 AX200 or Realtek RTL8822BE, each with its
//! Bluetooth part on internal USB). Bluetooth controllers not listed are
//! still recognised by their USB class (E0/01/01).

/// Who makes the chip; drivers are per family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Intel,
    Realtek,
    MediaTek,
    Qualcomm,
    Broadcom,
    Other,
}

impl Vendor {
    pub fn name(self) -> &'static str {
        match self {
            Vendor::Intel => "Intel",
            Vendor::Realtek => "Realtek",
            Vendor::MediaTek => "MediaTek",
            Vendor::Qualcomm => "Qualcomm Atheros",
            Vendor::Broadcom => "Broadcom",
            Vendor::Other => "Unknown vendor",
        }
    }
}

/// The driver family a chip belongs to (as Linux groups them), for status
/// messages and driver selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// iwlwifi "22000" generation: AX200, AX201 (firmware `iwlwifi-cc-a0`).
    Iwl22000,
    /// iwlwifi AX210/AX211 generation.
    IwlAx210,
    /// iwlwifi 9000 generation (9260, 9560).
    Iwl9000,
    /// iwlwifi 7000/8000 generations (mvm firmware, older transport).
    Iwl8000,
    /// rtw88: RTL8822BE/CE, RTL8821CE, RTL8723DE.
    Rtw88,
    /// rtw89: RTL8852AE/BE/CE.
    Rtw89,
    /// mt76: MT7921/MT7922.
    Mt7921,
    /// ath10k / ath11k / ath9k.
    Ath,
    /// brcmfmac / b43 / wl.
    Brcm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WifiChip {
    pub vendor: Vendor,
    pub family: Family,
    pub name: &'static str,
}

const fn w(vendor: Vendor, family: Family, name: &'static str) -> WifiChip {
    WifiChip { vendor, family, name }
}

/// The Wi-Fi chip behind PCI `vendor:device`, if it is a known one.
pub fn wifi_chip(vendor: u16, device: u16) -> Option<WifiChip> {
    use Family::*;
    use Vendor::*;
    Some(match (vendor, device) {
        (0x8086, 0x2723) => w(Intel, Iwl22000, "Intel Wi-Fi 6 AX200"),
        (0x8086, 0x02f0 | 0x06f0 | 0x34f0 | 0x43f0 | 0x4df0 | 0xa0f0) => w(Intel, Iwl22000, "Intel Wi-Fi 6 AX201"),
        (0x8086, 0x2725) => w(Intel, IwlAx210, "Intel Wi-Fi 6E AX210"),
        (0x8086, 0x51f0 | 0x51f1 | 0x54f0 | 0x7af0 | 0x7a70) => w(Intel, IwlAx210, "Intel Wi-Fi 6E AX211"),
        (0x8086, 0x2526) => w(Intel, Iwl9000, "Intel Wireless-AC 9260"),
        (0x8086, 0x9df0 | 0xa370 | 0x31dc | 0x30dc) => w(Intel, Iwl9000, "Intel Wireless-AC 9560"),
        (0x8086, 0x24fd) => w(Intel, Iwl8000, "Intel Wireless 8265"),
        (0x8086, 0x24f3) => w(Intel, Iwl8000, "Intel Wireless 8260"),
        (0x8086, 0x095a | 0x095b) => w(Intel, Iwl8000, "Intel Wireless 7265"),
        (0x8086, 0x08b1 | 0x08b2) => w(Intel, Iwl8000, "Intel Wireless 7260"),
        (0x8086, 0x3165 | 0x3166) => w(Intel, Iwl8000, "Intel Wireless 3165"),
        (0x8086, 0x24fb) => w(Intel, Iwl8000, "Intel Wireless 3168"),
        (0x10ec, 0xb822) => w(Realtek, Rtw88, "Realtek RTL8822BE"),
        (0x10ec, 0xc822) => w(Realtek, Rtw88, "Realtek RTL8822CE"),
        (0x10ec, 0xc821) => w(Realtek, Rtw88, "Realtek RTL8821CE"),
        (0x10ec, 0xd723) => w(Realtek, Rtw88, "Realtek RTL8723DE"),
        (0x10ec, 0x8852) => w(Realtek, Rtw89, "Realtek RTL8852AE"),
        (0x10ec, 0xb852) => w(Realtek, Rtw89, "Realtek RTL8852BE"),
        (0x10ec, 0xc852) => w(Realtek, Rtw89, "Realtek RTL8852CE"),
        (0x14c3, 0x7961) => w(MediaTek, Mt7921, "MediaTek MT7921"),
        (0x14c3, 0x0608) => w(MediaTek, Mt7921, "MediaTek MT7921K (RZ608)"),
        (0x14c3, 0x0616 | 0x7922) => w(MediaTek, Mt7921, "MediaTek MT7922 (RZ616)"),
        (0x168c, 0x003e) => w(Qualcomm, Ath, "Qualcomm Atheros QCA6174"),
        (0x168c, 0x0042) => w(Qualcomm, Ath, "Qualcomm Atheros QCA9377"),
        (0x168c, 0x0034) => w(Qualcomm, Ath, "Qualcomm Atheros AR9462"),
        (0x168c, 0x0036) => w(Qualcomm, Ath, "Qualcomm Atheros QCA9565"),
        (0x168c, 0x002b) => w(Qualcomm, Ath, "Qualcomm Atheros AR9285"),
        (0x17cb, 0x1101) => w(Qualcomm, Ath, "Qualcomm QCA6390"),
        (0x17cb, 0x1103) => w(Qualcomm, Ath, "Qualcomm WCN6855"),
        (0x14e4, 0x43a0) => w(Broadcom, Brcm, "Broadcom BCM4360"),
        (0x14e4, 0x43b1) => w(Broadcom, Brcm, "Broadcom BCM4352"),
        (0x14e4, 0x43ba) => w(Broadcom, Brcm, "Broadcom BCM43602"),
        _ => return None,
    })
}

/// PCI class 02 subclass 80 ("other network") is what most Wi-Fi cards
/// report; 0x0d (wireless controller) is the official one.
pub fn is_wifi_class(class: u8, subclass: u8) -> bool {
    class == 0x0d || (class == 0x02 && subclass == 0x80)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BtChip {
    pub vendor: Vendor,
    pub name: &'static str,
}

/// USB class of Bluetooth controllers: wireless controller, RF, Bluetooth.
pub const BT_CLASS: (u8, u8, u8) = (0xE0, 0x01, 0x01);

/// The Bluetooth controller behind USB `vid:pid`. `class` is the device's
/// (or its first interface's) class triple; unknown ids with the
/// Bluetooth class are still reported, named after their vendor.
pub fn bt_chip(vid: u16, pid: u16, class: (u8, u8, u8)) -> Option<BtChip> {
    use Vendor::*;
    let known = match (vid, pid) {
        (0x8087, 0x0029) => Some((Intel, "Intel AX200 Bluetooth")),
        (0x8087, 0x0026) => Some((Intel, "Intel AX201 Bluetooth")),
        (0x8087, 0x0032) => Some((Intel, "Intel AX210 Bluetooth")),
        (0x8087, 0x0033) => Some((Intel, "Intel AX211 Bluetooth")),
        (0x8087, 0x0025) => Some((Intel, "Intel 9260 Bluetooth")),
        (0x8087, 0x0aaa) => Some((Intel, "Intel 9560 Bluetooth")),
        (0x8087, 0x0a2b) => Some((Intel, "Intel 8265 Bluetooth")),
        (0x8087, 0x0a2a) => Some((Intel, "Intel 7265 Bluetooth")),
        (0x8087, 0x07dc) => Some((Intel, "Intel 7260 Bluetooth")),
        (0x0bda, 0xb023) => Some((Realtek, "Realtek RTL8822BE Bluetooth")),
        _ => None,
    };
    if let Some((vendor, name)) = known {
        return Some(BtChip { vendor, name });
    }
    if class != BT_CLASS {
        return None;
    }
    let (vendor, name) = match vid {
        0x8087 => (Intel, "Intel Bluetooth"),
        0x0bda => (Realtek, "Realtek Bluetooth"),
        0x0e8d | 0x13d3 | 0x0489 => (MediaTek, "MediaTek Bluetooth"),
        0x0cf3 | 0x04ca => (Qualcomm, "Qualcomm Atheros Bluetooth"),
        0x0a5c => (Broadcom, "Broadcom Bluetooth"),
        _ => (Other, "Bluetooth controller"),
    };
    Some(BtChip { vendor, name })
}
