// SPDX-License-Identifier: GPL-2.0-or-later
//! Writes a PC boot partition into a disk image the way the installer's
//! `write-boot-files` step does (GlassineOS/installer `core/src/pc_boot.rs`,
//! `core/src/fat.rs`): the kernel set with its assembled `initrd` (the initramfs and
//! the board archive), the recovery image, the default splash, the loader at both
//! paths and the loader record in both slots, laid out by the installer's FAT32
//! writer. Prints what it wrote as `key=value` lines.
//!
//! mkboot DISK OFFSET SIZE PARTUUID ARCH LOADER KERNEL INITRAMFS BOARD_CONF
//!        RECOVERY_KERNEL RECOVERY_INITRD|- RECOVERY_CMDLINE SPLASH|- SYSTEM_UUID USERDATA_UUID
use glassine_installer::fat;
use glassine_installer::pc_boot::{self, BootRoles, Content, KernelSet, Part};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Seek, SeekFrom, Write};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() != 15 {
        eprintln!("usage: see the source");
        std::process::exit(2);
    }
    let (disk, offset, size, partuuid, arch) = (&a[0], a[1].parse::<u64>().unwrap(), a[2].parse::<u64>().unwrap(), &a[3], &a[4]);
    let read = |p: &str| std::fs::read(p).unwrap_or_else(|e| panic!("{p}: {e}"));
    let opt = |p: &str| (p != "-").then(|| read(p));
    let mut roles: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    roles.insert("loader", read(&a[5]));
    roles.insert("kernel", read(&a[6]));
    roles.insert("initramfs", read(&a[7]));
    let board = read(&a[8]);
    roles.insert("recovery", read(&a[9]));
    if let Some(ri) = opt(&a[10]) {
        roles.insert("recovery-initrd", ri);
    }
    let rcmd = &a[11];
    if let Some(s) = opt(&a[12]) {
        pc_boot::check_splash(&s).unwrap();
        roles.insert("splash", s);
    }
    let (system, userdata) = (&a[13], &a[14]);

    let sum = |b: &[u8]| hex(&Sha256::digest(b));
    let base = pc_boot::board_cmdline(&board).unwrap();
    let br = BootRoles { kernel: "kernel".into(), initramfs: "initramfs".into(), loader: "loader".into(), board: Some("board".into()), ..Default::default() };
    let parts = pc_boot::initrd_parts(&br, &board, &|r| roles[r].len() as u64);
    let mut initrd = Vec::new();
    for p in &parts {
        match p {
            Part::Role(r) => initrd.extend_from_slice(&roles[r.as_str()]),
            Part::Bytes(b) => initrd.extend_from_slice(b),
        }
    }
    let k = &roles["kernel"];
    let set = KernelSet::new((&sum(k), k.len() as u64), Some((&sum(&initrd), initrd.len() as u64)), &base, system, userdata, "test", "").unwrap();
    let rk = &roles["recovery"];
    let ri = roles.get("recovery-initrd");
    let rsum = ri.map(|x| sum(x));
    let rset = KernelSet::new((&sum(rk), rk.len() as u64), ri.map(|x| (rsum.as_deref().unwrap(), x.len() as u64)), rcmd, system, userdata, "test", "").unwrap();
    let recovery = (rset.clone(), "recovery".to_string(), ri.map(|_| "recovery-initrd".to_string()));
    let splash = roles.get("splash").map(|s| ("splash", s.len() as u64));
    let bf = pc_boot::files(arch, ("loader", roles["loader"].len() as u64), &set, "kernel", Content::Parts(parts.clone()), Some(&recovery), splash).unwrap();

    let specs: Vec<fat::FileSpec> = bf.files.iter().map(|(f, _)| f.clone()).collect();
    let guid: Vec<u8> = (0..16).map(|i| u8::from_str_radix(&partuuid.replace('-', "")[i * 2..i * 2 + 2], 16).unwrap()).collect();
    let serial = u32::from_le_bytes(guid[0..4].try_into().unwrap());
    let lay = fat::layout(size, 512, offset / 512, serial, pc_boot::VOLUME_LABEL, &specs).unwrap();
    let mut f = std::fs::OpenOptions::new().write(true).open(disk).unwrap();
    let mut put = |at: u64, b: &[u8]| {
        f.seek(SeekFrom::Start(offset + at)).unwrap();
        f.write_all(b).unwrap();
    };
    for (at, b) in &lay.tables {
        put(*at, b);
    }
    for ((_, content), (_, at, _)) in bf.files.iter().zip(&lay.files) {
        let bytes = match content {
            Content::Artifact(r) => roles[r.as_str()].clone(),
            Content::Bytes(b) => b.clone(),
            Content::Parts(_) => initrd.clone(),
        };
        put(*at, &bytes);
    }
    put(lay.reserved.0, &lay.reserved.1);
    println!("set={}", set.id);
    println!("set_rec_sha256={}", set.record_sha256());
    println!("initrd_sha256={}", sum(&initrd));
    println!("initrd_size={}", initrd.len());
    println!("cmdline={}", set.cmdline);
    println!("recovery={}", rset.id);
    println!("recovery_rec_sha256={}", rset.record_sha256());
    println!("recovery_cmdline={}", rset.cmdline);
}
