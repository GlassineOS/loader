//! The spike's stand-in for 010's loader record: `key=value` lines.
use alloc::string::String;


#[derive(Default)]
pub struct Record {
    pub kernel: String,
    pub kernel_sha256: String,
    pub initrd: String,
    pub initrd_sha256: String,
    pub cmdline: String,
    /// (id, label) of the previous deployment, from `glassine-deploy list`.
    pub previous_deploy: Option<(String, String)>,
    pub wait_ms: u64,
    /// Open the GOP exclusively, which disconnects the firmware's text console from it.
    pub exclusive_gop: bool,
}

pub fn parse(text: &str) -> Record {
    let mut r = Record { wait_ms: 2000, ..Default::default() };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "kernel" => r.kernel = v.into(),
            "kernel_sha256" => r.kernel_sha256 = v.into(),
            "initrd" => r.initrd = v.into(),
            "initrd_sha256" => r.initrd_sha256 = v.into(),
            "cmdline" => r.cmdline = v.into(),
            "wait_ms" => r.wait_ms = v.parse().unwrap_or(2000),
            "exclusive_gop" => r.exclusive_gop = v == "1",
            "previous_deploy" => {
                let mut it = v.splitn(2, ' ');
                let id = it.next().unwrap_or("");
                let label = it.next().unwrap_or(id);
                if !id.is_empty() {
                    r.previous_deploy = Some((id.into(), label.into()));
                }
            }
            _ => {}
        }
    }
    r
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let d = b"0123456789abcdef";
        s.push(d[(b >> 4) as usize] as char);
        s.push(d[(b & 15) as usize] as char);
    }
    s
}

