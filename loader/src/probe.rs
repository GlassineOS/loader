// SPDX-License-Identifier: GPL-2.0-or-later
//! The probe report (GlassineOS/boot `docs/handback.md`, "The probe area"): what the
//! firmware offers, read without changing anything. The only firmware calls are listing
//! handles and their protocols, opening protocols with `GetProtocol` (never exclusively)
//! and the graphics output's `QueryMode`; the console's mode is read from its fields.
//! No mode is set, nothing is drawn, no hardware is touched.
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use glassine_loader_core::raw::PROBE_BYTES;
use glassine_loader_core::sha256_hex;
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, SearchType};
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};
use uefi::proto::device_path::media::PartitionSignature;
use uefi::proto::device_path::{DevicePath, DevicePathNodeEnum};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{Guid, Handle, Identify, guid};
use uefi_raw::protocol::console::{GraphicsOutputProtocol, SimpleTextOutputProtocol};

/// Names the report gives: protocols of the UEFI specification, and the protocol GUIDs
/// of Qualcomm's public bootloader source (`QcomModulePkg/QcomModulePkg.dec`, ABL tag
/// LA.UM.7.2.c25-12600-sdm660.0).
fn names() -> Vec<(Guid, &'static str)> {
    use uefi::proto::console::{pointer::Pointer, serial::Serial, text::Input, text::Output};
    use uefi::proto::device_path::DevicePath;
    use uefi::proto::loaded_image::LoadedImage;
    use uefi::proto::media::{block::BlockIO, disk::DiskIo, fs::SimpleFileSystem, load_file::LoadFile, load_file::LoadFile2, partition::PartitionInfo};
    alloc::vec![
        (GraphicsOutput::GUID, "graphics-output"),
        (BlockIO::GUID, "block-io"),
        (DiskIo::GUID, "disk-io"),
        (DevicePath::GUID, "device-path"),
        (LoadedImage::GUID, "loaded-image"),
        (Input::GUID, "simple-text-in"),
        (Output::GUID, "simple-text-out"),
        (SimpleFileSystem::GUID, "simple-file-system"),
        (Serial::GUID, "serial-io"),
        (PartitionInfo::GUID, "partition-info"),
        (Pointer::GUID, "simple-pointer"),
        (LoadFile::GUID, "load-file"),
        (LoadFile2::GUID, "load-file2"),
        (uefi_raw::protocol::console::AbsolutePointerProtocol::GUID, "absolute-pointer"),
        (uefi_raw::protocol::console::EdidDiscoveredProtocol::GUID, "edid-discovered"),
        (guid!("bd8c1056-9f36-44ec-92a8-a6337f817986"), "edid-active"),
        (guid!("bc62157e-3e33-4fec-9920-2d3b36d750df"), "loaded-image-device-path"),
        (guid!("dd9e7534-7762-4698-8c14-f58517a625aa"), "simple-text-in-ex"),
        (guid!("a77b2472-e282-4e9f-a245-c2c0e27bbcc1"), "block-io2"),
        (guid!("6a7a5cff-e8d9-4f70-bada-75ab3025ce14"), "component-name2"),
        (guid!("18a031ab-b443-4d1a-a5c0-0c09261e9f71"), "driver-binding"),
        (guid!("3152bca5-eade-433d-862e-c01cdc291f44"), "rng (qcom: gQcomRngProtocolGuid)"),
        (guid!("77ed108d-8524-4b8b-9d2e-34987aecb9c1"), "qcom:gQcomScmProtocolGuid"),
        (guid!("157a5c45-21b2-43c5-ba7c-822fee5fe599"), "qcom:gEfiPlatformInfoProtocolGuid"),
        (guid!("85c1f7d2-bce6-4f31-8f4d-d37e03d05eaa"), "qcom:gEfiMemCardInfoProtocolGuid"),
        (guid!("32d2963a-fe5d-4f30-b633-6e5dc55803cc"), "qcom:gEfiUsbfnIoProtocolGuid"),
        (guid!("b0760469-970c-487a-a4b5-28db7b45cef1"), "qcom:gEfiChipInfoProtocolGuid"),
        (guid!("4684800a-2755-4edc-b443-7f8ceb3239d3"), "qcom:gQcomPmicVersionProtocolGuid"),
        (guid!("d9d9ce48-44b8-4f49-8e3e-2a3b927dc6c1"), "qcom:gEfiUsbDeviceProtocolGuid"),
        (guid!("5172ffb5-4253-7d51-c641-a701f973103c"), "qcom:gEfiRamPartitionProtocolGuid"),
        (guid!("95a9a93e-a86e-4926-aaef-9918e772d987"), "qcom:gEfiEraseBlockProtocolGuid"),
        (guid!("a022155a-4828-4535-a499-11f15240b91b"), "qcom:gEfiResetReasonProtocolGuid"),
        (guid!("6edc8a6d-2663-43cd-90ff-4621ffd10df5"), "qcom:gChargerExProtocolGuid"),
        (guid!("97044b58-fea4-4ad0-9d0b-e417d60f11a1"), "qcom:gQcomPmicPonProtocolGuid"),
        (guid!("79d6c879-725e-489e-a0a9-27efa5dfcb35"), "qcom:gEfiLimitsProtocolGuid"),
        (guid!("8e5eff91-21b6-47d3-af2b-c15a01e020ec"), "qcom:gEfiQcomVerifiedBootProtocolGuid"),
        (guid!("f57f73ed-0afc-4723-9374-2cebc0198ef9"), "qcom:gQcomScmModeSwithProtocolGuid"),
        (guid!("a74862ce-680f-4fe1-a311-df41f4030391"), "qcom:gQcomQseecomProtocolGuid"),
        (guid!("71746e63-65f9-41ec-ac08-cdd1f2d02298"), "qcom:gQcomMdtpProtocolGuid"),
        (guid!("d68edce2-a314-457b-962a-1d99bbfcbbfb"), "qcom:gEfiNandPartiGuidProtocolGuid"),
        (guid!("e3eef434-22c9-e33b-8f5d-0e81686a68cb"), "qcom:gEfiUbiFlasherProtocolGuid"),
        (guid!("408ec123-134e-4b80-9142-b89a0e08f8b3"), "qcom:gEfiQcomASN1X509ProtocolGuid"),
        (guid!("eb7a442a-cef4-40a7-a8ae-5a2e7f48e172"), "qcom:gEfiQcomSecRSAProtocolGuid"),
        (guid!("c0dd69ac-76ba-11e6-ab24-1fc7f5575f19"), "qcom:gQcomDisplayUtilsProtocolGuid"),
        (guid!("d7f58a0e-bed2-4b5a-bb43-8ab23dd0e2b0"), "qcom:gQcomKeypadDeviceGuid"),
    ]
}

fn pixel_format(f: PixelFormat) -> &'static str {
    match f {
        PixelFormat::Rgb => "rgb",
        PixelFormat::Bgr => "bgr",
        PixelFormat::Bitmask => "bitmask",
        PixelFormat::BltOnly => "blt-only",
    }
}

fn guids_of(h: Handle) -> Vec<Guid> {
    boot::protocols_per_handle(h).map(|p| p.iter().map(|g| **g).collect()).unwrap_or_default()
}

/// One line per key, in order; `truncated=1` when lines had to be left out.
struct Report {
    head: Vec<String>,
    protos: Vec<String>,
    gop: Vec<String>,
    /// `gop<n>.mode<m>` lines past each display's current mode: left out first after the
    /// log.
    extra_modes: Vec<String>,
    tail: Vec<String>,
    log: Vec<String>,
}

impl Report {
    fn text(&self, log: usize, extra: usize, protos: usize, truncated: bool) -> String {
        let mut t = String::new();
        let mut push = |l: &String| {
            t.push_str(l);
            t.push('\n');
        };
        self.head.iter().for_each(&mut push);
        self.protos.iter().take(protos).for_each(&mut push);
        self.gop.iter().for_each(&mut push);
        self.extra_modes.iter().take(extra).for_each(&mut push);
        self.tail.iter().for_each(&mut push);
        self.log.iter().take(log).for_each(&mut push);
        if truncated {
            t.push_str("truncated=1\n");
        }
        t
    }

    /// The area's bytes: the report, its check line, newlines up to [`PROBE_BYTES`].
    fn bytes(&self) -> Vec<u8> {
        let fits = |t: &String| t.len() + 7 + 64 <= PROBE_BYTES;
        let (mut l, mut e, mut p) = (self.log.len(), self.extra_modes.len(), self.protos.len());
        let mut cut = false;
        loop {
            let t = self.text(l, e, p, cut);
            if fits(&t) {
                let mut b = t.clone().into_bytes();
                b.extend_from_slice(format!("check={}\n", sha256_hex(t.as_bytes())).as_bytes());
                b.resize(PROBE_BYTES, b'\n');
                return b;
            }
            cut = true;
            if l > 0 {
                l -= 1;
            } else if e > 0 {
                e -= 1;
            } else if p > 0 {
                p -= 1;
            } else {
                let t = String::from("format=1\ntruncated=1\n");
                let mut b = t.clone().into_bytes();
                b.extend_from_slice(format!("check={}\n", sha256_hex(t.as_bytes())).as_bytes());
                b.resize(PROBE_BYTES, b'\n');
                return b;
            }
        }
    }
}

/// The partition's GPT identity and number, from a handle's device path.
fn partition_of(h: Handle) -> (String, String) {
    let params = OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None };
    if let Ok(dp) = unsafe { boot::open_protocol::<DevicePath>(params, OpenProtocolAttributes::GetProtocol) } {
        for node in dp.node_iter() {
            if let Ok(DevicePathNodeEnum::MediaHardDrive(hd)) = node.as_enum() {
                let id = match hd.partition_signature() {
                    PartitionSignature::Guid(g) => format!("{g}"),
                    _ => "-".into(),
                };
                return (id, format!("{}", hd.partition_number()));
            }
        }
    }
    ("-".into(), "-".into())
}

/// The report, as the bytes of the probe report. `boot`: the boot partition's handle
/// (our `DeviceHandle`), when there is one.
pub fn report(boot_part: Option<Handle>) -> Vec<u8> {
    crate::log!("PROBE start");
    let names = names();
    let name = |g: &Guid| names.iter().find(|(n, _)| n == g).map(|(_, s)| *s).unwrap_or("-");
    let all: Vec<Handle> = boot::locate_handle_buffer(SearchType::AllHandles).map(|b| b.iter().copied().collect()).unwrap_or_default();
    let index = |h: Handle| all.iter().position(|x| *x == h);

    let mut counts: BTreeMap<String, (Guid, usize)> = BTreeMap::new();
    for &h in &all {
        for g in guids_of(h) {
            counts.entry(format!("{g}")).or_insert((g, 0)).1 += 1;
        }
    }
    let st = uefi::table::system_table_raw();
    let head = alloc::vec![
        String::from("format=1"),
        format!("loader={}", env!("CARGO_PKG_VERSION")),
        format!("firmware={} 0x{:08x}", uefi::system::firmware_vendor(), uefi::system::firmware_revision()),
        format!("uefi={}.{}", uefi::system::uefi_revision().major(), uefi::system::uefi_revision().minor()),
        format!("handles={}", all.len()),
        format!("protocols={}", counts.len()),
    ];
    // File systems: on the boot partition (which form the loader uses) and on any handle
    // (whether the firmware has a file system driver at all).
    let fss: Vec<Handle> = boot::locate_handle_buffer(SearchType::ByProtocol(&SimpleFileSystem::GUID))
        .map(|b| b.iter().copied().collect())
        .unwrap_or_default();
    let mut head = head;
    head.push(format!("boot_fs={}", boot_part.is_some_and(|b| fss.contains(&b)) as u8));
    head.push(format!("fs_handles={}", fss.len()));
    for (i, &h) in fss.iter().enumerate() {
        let (id, num) = partition_of(h);
        head.push(format!("fs{}=handle={} part={} {}", i + 1, index(h).map_or("-".into(), |x| format!("{x}")), id, num));
    }
    let protos: Vec<String> =
        counts.iter().enumerate().map(|(i, (s, (g, n)))| format!("proto{}={} {} {}", i + 1, s, n, name(g))).collect();

    // The graphics output: modes by QueryMode, the current mode from its fields.
    let mut gop = Vec::new();
    let mut extra_modes = Vec::new();
    let gops: Vec<Handle> =
        boot::locate_handle_buffer(SearchType::ByProtocol(&GraphicsOutput::GUID)).map(|b| b.iter().copied().collect()).unwrap_or_default();
    gop.push(format!("gops={}", gops.len()));
    for (i, &h) in gops.iter().enumerate() {
        let n = i + 1;
        let params = OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None };
        let Ok(g) = (unsafe { boot::open_protocol::<GraphicsOutput>(params, OpenProtocolAttributes::GetProtocol) }) else {
            gop.push(format!("gop{n}=handle={} unopened", index(h).map_or("-".into(), |x| format!("{x}"))));
            continue;
        };
        let raw = unsafe { &*(&*g as *const GraphicsOutput as *const GraphicsOutputProtocol) };
        let m = unsafe { &*raw.mode };
        gop.push(format!(
            "gop{n}=handle={} modes={} current={} fb=0x{:x} size={}",
            index(h).map_or("-".into(), |x| format!("{x}")),
            m.max_mode,
            m.mode,
            m.frame_buffer_base,
            m.frame_buffer_size
        ));
        for (j, mode) in g.modes().enumerate() {
            let info = mode.info();
            let (w, hh) = info.resolution();
            let line = format!("gop{n}.mode{j}={w}x{hh} {} stride={}", pixel_format(info.pixel_format()), info.stride());
            if j as u32 <= m.mode { gop.push(line) } else { extra_modes.push(line) }
        }
    }

    // The console handles of the system table.
    let mut tail = Vec::new();
    if let Some(st) = st {
        let st = unsafe { st.as_ref() };
        for (key, h) in [("conin", st.stdin_handle), ("conout", st.stdout_handle), ("stderr", st.stderr_handle)] {
            let line = match unsafe { Handle::from_ptr(h) } {
                Some(h) => {
                    let gs: Vec<String> = guids_of(h).iter().map(|g| format!("{g}")).collect();
                    format!("{key}=handle={} {}", index(h).map_or("-".into(), |x| format!("{x}")), gs.join(" "))
                }
                None => format!("{key}=handle=none"),
            };
            tail.push(line);
        }
        if !st.stdout.is_null() {
            let out = unsafe { &*(st.stdout as *const SimpleTextOutputProtocol) };
            if !out.mode.is_null() {
                let mode = unsafe { &*out.mode };
                tail.push(format!("conout.modes=max={} current={}", mode.max_mode, mode.mode));
            }
        }
    }
    let log: Vec<String> = crate::log::kept().into_iter().enumerate().map(|(i, l)| format!("log{}={}", i + 1, l)).collect();
    let r = Report { head, protos, gop, extra_modes, tail, log };
    let b = r.bytes();
    crate::log!("PROBE done: {} handles, {} protocols, {} displays", all.len(), counts.len(), gops.len());
    b
}
