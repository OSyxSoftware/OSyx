use crate::ids::{bt_chip, wifi_chip, Family, Vendor, BT_CLASS};
use crate::wpa::*;
use alloc::vec::Vec;

fn hex(s: &str) -> Vec<u8> {
    let s: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    s.chunks(2).map(|c| u8::from_str_radix(core::str::from_utf8(c).unwrap(), 16).unwrap()).collect()
}

#[test]
fn thinkpad_t495_chips() {
    let ax200 = wifi_chip(0x8086, 0x2723).unwrap();
    assert_eq!((ax200.vendor, ax200.family), (Vendor::Intel, Family::Iwl22000));
    assert_eq!(wifi_chip(0x10ec, 0xb822).unwrap().family, Family::Rtw88);
    assert_eq!(bt_chip(0x8087, 0x0029, (0xE0, 1, 1)).unwrap().name, "Intel AX200 Bluetooth");
    assert_eq!(bt_chip(0x0bda, 0xb023, (0, 0, 0)).unwrap().vendor, Vendor::Realtek);
    assert_eq!(bt_chip(0x1234, 0x5678, BT_CLASS).unwrap().vendor, Vendor::Other);
    assert!(bt_chip(0x1234, 0x5678, (3, 1, 1)).is_none());
    assert!(wifi_chip(0x8086, 0x100e).is_none());
}

#[test]
fn pmk_test_vectors() {
    // IEEE 802.11-2016 J.4.2.
    assert_eq!(
        pmk_from_passphrase("password", b"IEEE").to_vec(),
        hex("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e")
    );
    assert_eq!(
        pmk_from_passphrase("ThisIsAPassword", b"ThisIsASSID").to_vec(),
        hex("0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af")
    );
}

#[test]
fn prf_test_vector() {
    // IEEE 802.11-2016 J.3.2, test case 1.
    let mut out = [0u8; 64];
    prf(&[0x0b; 20], b"prefix", b"Hi There", &mut out);
    assert_eq!(
        out.to_vec(),
        hex("bcd4c650b30b9684951829e0d75f9d54b862175ed9f00606e17d8da35402ffee
             75df78c3d31e0f889f012120c0862beb67753e7439ae242edb8373698356cf5a")
    );
}

#[test]
fn aes_key_wrap_rfc3394() {
    let kek: [u8; 16] = hex("000102030405060708090A0B0C0D0E0F").try_into().unwrap();
    let plain = hex("00112233445566778899AABBCCDDEEFF");
    let wrapped = hex("1FA68B0A8112B447AEF34BD8FB5A7B829D3E862371D2CFE5");
    assert_eq!(aes_wrap(&kek, &plain).unwrap(), wrapped);
    assert_eq!(aes_unwrap(&kek, &wrapped).unwrap(), plain);
    let mut bad = wrapped.clone();
    bad[5] ^= 1;
    assert!(aes_unwrap(&kek, &bad).is_none());
}

/// A minimal authenticator, for driving the station through a handshake.
struct Ap {
    pmk: [u8; 32],
    aa: [u8; 6],
    spa: [u8; 6],
    anonce: [u8; 32],
    ptk: Option<Ptk>,
    replay: u64,
}

impl Ap {
    fn msg1(&mut self) -> Vec<u8> {
        self.replay += 1;
        build(info::VERSION_AES | info::PAIRWISE | info::ACK, 16, self.replay, &self.anonce, &[], None)
    }

    fn take_msg2(&mut self, f: &[u8]) {
        let mut snonce = [0u8; 32];
        snonce.copy_from_slice(&f[17..49]);
        let ptk = derive_ptk(&self.pmk, &self.aa, &self.spa, &self.anonce, &snonce);
        assert_eq!(mic(&ptk.kck, f), f[81..97], "message 2 MIC");
        self.ptk = Some(ptk);
    }

    fn key_data(&self, gtk: &[u8], key_id: u8) -> Vec<u8> {
        let mut d = rsn_ie_psk(suite::CCMP);
        d.extend_from_slice(&[0xdd, 6 + gtk.len() as u8, 0x00, 0x0f, 0xac, 0x01, key_id, 0]);
        d.extend_from_slice(gtk);
        d.push(0xdd);
        while d.len() % 8 != 0 || d.len() < 16 {
            d.push(0);
        }
        aes_wrap(&self.ptk.unwrap().kek, &d).unwrap()
    }

    fn msg3(&mut self, gtk: &[u8]) -> Vec<u8> {
        self.replay += 1;
        let data = self.key_data(gtk, 1);
        let i = info::VERSION_AES | info::PAIRWISE | info::INSTALL | info::ACK | info::MIC | info::SECURE | info::ENCRYPTED;
        build(i, 16, self.replay, &self.anonce, &data, Some(&self.ptk.unwrap().kck))
    }

    fn group1(&mut self, gtk: &[u8]) -> Vec<u8> {
        self.replay += 1;
        let data = self.key_data(gtk, 2);
        let i = info::VERSION_AES | info::ACK | info::MIC | info::SECURE | info::ENCRYPTED;
        build(i, 16, self.replay, &[0; 32], &data, Some(&self.ptk.unwrap().kck))
    }
}

fn sent(actions: &[Action]) -> Vec<u8> {
    actions.iter().find_map(|a| if let Action::Send(f) = a { Some(f.clone()) } else { None }).unwrap()
}

#[test]
fn four_way_and_group_handshake() {
    let pmk = pmk_from_passphrase("correct horse battery", b"OSyxNet");
    let (aa, spa) = ([0x02, 0, 0, 0, 0, 1], [0x02, 0, 0, 0, 0, 2]);
    let mut ap = Ap { pmk, aa, spa, anonce: [0x11; 32], ptk: None, replay: 0 };
    let mut sta = Supplicant::new(pmk, aa, spa, rsn_ie_psk(suite::CCMP), [0x22; 32]);

    let a = sta.receive(&ap.msg1()).unwrap();
    ap.take_msg2(&sent(&a));
    assert_eq!(sta.state, State::WaitMsg3);

    let gtk = [0x5a; 16];
    let a = sta.receive(&ap.msg3(&gtk)).unwrap();
    let ptk = ap.ptk.unwrap();
    let msg4 = sent(&a);
    assert_eq!(mic(&ptk.kck, &msg4), msg4[81..97], "message 4 MIC");
    assert!(a.contains(&Action::InstallPairwise(ptk.tk)));
    assert!(a.contains(&Action::InstallGroup(Gtk { key_id: 1, key: gtk.to_vec() })));
    assert_eq!(sta.state, State::Done);

    // The same message 3 again is a replay.
    ap.replay -= 1;
    assert_eq!(sta.receive(&ap.msg3(&gtk)), Err(WpaError::Replay));

    let new_gtk = [0xa5; 16];
    let a = sta.receive(&ap.group1(&new_gtk)).unwrap();
    assert!(a.contains(&Action::InstallGroup(Gtk { key_id: 2, key: new_gtk.to_vec() })));
}

#[test]
fn wrong_passphrase_fails_the_mic() {
    let (aa, spa) = ([0x02, 0, 0, 0, 0, 1], [0x02, 0, 0, 0, 0, 2]);
    let mut ap = Ap { pmk: pmk_from_passphrase("right password", b"Net"), aa, spa, anonce: [1; 32], ptk: None, replay: 0 };
    let mut sta = Supplicant::new(pmk_from_passphrase("wrong password", b"Net"), aa, spa, rsn_ie_psk(suite::CCMP), [2; 32]);
    let a = sta.receive(&ap.msg1()).unwrap();
    // The AP computes its PTK from the right PMK: the station's MIC is wrong.
    let f = sent(&a);
    let mut snonce = [0u8; 32];
    snonce.copy_from_slice(&f[17..49]);
    let ptk = derive_ptk(&ap.pmk, &aa, &spa, &ap.anonce, &snonce);
    assert_ne!(mic(&ptk.kck, &f), f[81..97]);
    ap.ptk = Some(ptk);
    assert_eq!(sta.receive(&ap.msg3(&[7; 16])), Err(WpaError::BadMic));
}
