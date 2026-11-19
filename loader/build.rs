//! Builds LVGL (pinned in scripts/fetch-lvgl.sh) and the C shim with a freestanding
//! clang for the same LLVM triple and float ABI as Rust's UEFI target of the build
//! (x86_64-unknown-uefi or aarch64-unknown-uefi).
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

const LVGL: &str = "third_party/lvgl-9.6.0";

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|e| e == "c") {
            out.push(p);
        }
    }
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let top = root.parent().unwrap().to_path_buf();
    let lvgl = top.join(LVGL);
    assert!(lvgl.join(".ok").exists(), "run scripts/fetch-lvgl.sh first");
    println!("cargo:rerun-if-changed=c");
    println!("cargo:rerun-if-changed=build.rs");

    let mut srcs = Vec::new();
    collect(&lvgl.join("src"), &mut srcs);
    srcs.push(root.join("c/shim.c"));
    srcs.push(root.join("c/menu_art.c"));
    collect(&root.join("c/fonts"), &mut srcs);

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target_flags: &[&str] = match arch.as_str() {
        // Rust's target spec: llvm-target x86_64-unknown-windows, -mmx,-sse,+soft-float.
        "x86_64" => &["--target=x86_64-unknown-windows-msvc", "-mno-red-zone", "-mno-mmx", "-mno-sse", "-msoft-float"],
        // Rust's target spec: llvm-target aarch64-unknown-windows; UEFI enables the FPU.
        "aarch64" => &["--target=aarch64-unknown-windows-msvc"],
        a => panic!("no UEFI build for {a}"),
    };
    let flags: Vec<String> = target_flags
        .iter()
        .chain([
        "-ffreestanding",
        "-fshort-wchar",
        "-mno-stack-arg-probe",
        "-fno-stack-protector",
        "-fno-exceptions",
        "-Os",
        "-ffunction-sections",
        "-fdata-sections",
        "-Wno-everything",
        "-DLV_CONF_INCLUDE_SIMPLE",
        "-DLV_LVGL_H_INCLUDE_SIMPLE",
    ]
    .iter())
    .map(|s| s.to_string())
    .chain([
        format!("-I{}", root.join("c/libc").display()),
        format!("-I{}", root.join("c").display()),
        format!("-I{}", lvgl.display()),
        format!("-ffile-prefix-map={}=.", top.display()),
    ])
    .collect();

    let objs = Mutex::new(Vec::new());
    let next = Mutex::new(0usize);
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    std::thread::scope(|s| {
        for _ in 0..n {
            s.spawn(|| loop {
                let i = {
                    let mut g = next.lock().unwrap();
                    let i = *g;
                    *g += 1;
                    i
                };
                let Some(src) = srcs.get(i) else { break };
                let rel = src.strip_prefix(&top).unwrap().to_string_lossy().replace('/', "_");
                let obj = out.join(format!("{rel}.obj"));
                let st = Command::new("clang")
                    .args(&flags)
                    .arg("-c")
                    .arg(src)
                    .arg("-o")
                    .arg(&obj)
                    .status()
                    .expect("clang");
                assert!(st.success(), "clang failed on {}", src.display());
                objs.lock().unwrap().push(obj);
            });
        }
    });
    let mut objs = objs.into_inner().unwrap();
    objs.sort();
    let lib = out.join("liblvgl.a");
    let _ = std::fs::remove_file(&lib);
    let st = Command::new("llvm-lib")
        .arg(format!("/out:{}", lib.display()))
        .args(&objs)
        .status()
        .expect("llvm-lib");
    assert!(st.success());
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=lvgl");
}
