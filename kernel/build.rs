use std::path::{Path, PathBuf};
use std::process::Command;

/// OpenBSD driver sources (drivers/bsd/openbsd) and the compatibility layer
/// they run on, compiled with clang into the kernel.
const BSD_SOURCES: &[&str] = &[
    "compat/compat.c",
    "compat/kern.c",
    "compat/wifi.c",
    "compat/audio.c",
    "openbsd/kern/subr_tree.c",
    "openbsd/net/if_media.c",
    "openbsd/dev/mii/mii.c",
    "openbsd/dev/mii/mii_physubr.c",
    "openbsd/dev/mii/rgephy.c",
    "openbsd/dev/mii/rlphy.c",
    "openbsd/dev/mii/ukphy.c",
    "openbsd/dev/mii/ukphy_subr.c",
    "openbsd/dev/ic/re.c",
    "openbsd/dev/ic/rtl81x9.c",
    "openbsd/dev/pci/if_re_pci.c",
    "openbsd/dev/pci/if_rl_pci.c",
    // Wi-Fi: iwm(4) on the net80211 stack and its crypto.
    "openbsd/dev/pci/if_iwm.c",
    "openbsd/net80211/ieee80211.c",
    "openbsd/net80211/ieee80211_amrr.c",
    "openbsd/net80211/ieee80211_crypto.c",
    "openbsd/net80211/ieee80211_crypto_bip.c",
    "openbsd/net80211/ieee80211_crypto_ccmp.c",
    "openbsd/net80211/ieee80211_crypto_tkip.c",
    "openbsd/net80211/ieee80211_crypto_wep.c",
    "openbsd/net80211/ieee80211_input.c",
    "openbsd/net80211/ieee80211_ioctl.c",
    "openbsd/net80211/ieee80211_node.c",
    "openbsd/net80211/ieee80211_output.c",
    "openbsd/net80211/ieee80211_pae_input.c",
    "openbsd/net80211/ieee80211_pae_output.c",
    "openbsd/net80211/ieee80211_proto.c",
    "openbsd/net80211/ieee80211_ra.c",
    "openbsd/net80211/ieee80211_ra_vht.c",
    "openbsd/net80211/ieee80211_regdomain.c",
    "openbsd/net80211/ieee80211_rssadapt.c",
    // Sound: azalia(4), Intel High Definition Audio.
    "openbsd/dev/pci/azalia.c",
    "openbsd/dev/pci/azalia_codec.c",
    "openbsd/crypto/aes.c",
    "openbsd/crypto/arc4.c",
    "openbsd/crypto/cmac.c",
    "openbsd/crypto/hmac.c",
    "openbsd/crypto/key_wrap.c",
    "openbsd/crypto/md5.c",
    "openbsd/crypto/michael.c",
    "openbsd/crypto/sha1.c",
    "openbsd/crypto/sha2.c",
];

fn find_clang() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OSYX_CLANG") {
        return Some(PathBuf::from(p));
    }
    let candidates = ["clang", "C:\\msys64\\ucrt64\\bin\\clang.exe", "C:\\msys64\\clang64\\bin\\clang.exe", "C:\\Program Files\\LLVM\\bin\\clang.exe"];
    candidates.iter().map(PathBuf::from).find(|c| Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()))
}

fn build_bsd(dir: &Path) {
    println!("cargo:rustc-check-cfg=cfg(osyx_bsd)");
    let bsd = dir.join("../drivers/bsd");
    println!("cargo:rerun-if-changed={}", bsd.display());
    println!("cargo:rerun-if-env-changed=OSYX_CLANG");
    if std::env::var("TARGET").as_deref() != Ok("x86_64-unknown-none") {
        return;
    }
    let Some(clang) = find_clang() else {
        println!("cargo:warning=clang not found: OpenBSD network drivers left out (set OSYX_CLANG)");
        return;
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    for src in BSD_SOURCES {
        let obj = out.join(src.replace(['/', '.'], "_") + ".o");
        let status = Command::new(&clang)
            .args([
                "--target=x86_64-unknown-none-elf",
                "-std=gnu11",
                "-O2",
                "-ffreestanding",
                "-nostdlibinc",
                "-fno-builtin",
                "-fno-strict-aliasing",
                "-fwrapv",
                "-fno-common",
                "-mno-red-zone",
                "-mcmodel=kernel",
                "-mgeneral-regs-only",
                "-fno-pic",
                "-fno-pie",
                "-fno-stack-protector",
                "-fno-asynchronous-unwind-tables",
                "-ffunction-sections",
                "-fdata-sections",
                "-D_KERNEL",
                // OSyx additions inside the OpenBSD sources are marked with it.
                "-D__OSYX__",
                "-w",
            ])
            .arg("-I")
            .arg(bsd.join("openbsd"))
            .arg("-I")
            .arg(bsd.join("include"))
            .arg("-c")
            .arg(bsd.join(src))
            .arg("-o")
            .arg(&obj)
            .status()
            .expect("running clang");
        assert!(status.success(), "clang failed on {src}");
        println!("cargo:rustc-link-arg-bins={}", obj.display());
    }
    println!("cargo:rustc-cfg=osyx_bsd");
}

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo:rustc-link-arg-bins=-T{dir}/linker.ld");
    println!("cargo:rustc-link-arg-bins=--no-dynamic-linker");
    println!("cargo:rerun-if-changed=linker.ld");
    build_bsd(Path::new(&dir));
}
