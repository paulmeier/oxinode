//! Puts `memory.x` where the linker will find it, and re-exports the
//! application's flash origin to Rust so the two can never drift apart.
//!
//! The origin matters at runtime as well as at link time: because we are linked
//! above a SoftDevice, the application has to point VTOR at its own vector table
//! (see `src/boot.rs`). Hardcoding 0x26000 in two files is exactly the kind of
//! thing that silently rots, so parse it instead. The parser itself lives in
//! `oxinode-core` where it is unit tested.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use oxinode_core::linker_script;

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

    // Which commit this image is. The crate version says which release a
    // board is near; the short hash says which build it is running, which is
    // the question on a bench with two boards and three branches. Printed in
    // the first line of the boot log and shown on the System screen.
    println!("cargo::rustc-env=OXINODE_BUILD={}", build_id());

    let memory_x = fs::read_to_string("memory.x").expect("cannot read memory.x");
    let flash = linker_script::parse_region(&memory_x, "FLASH")
        .expect("could not find `FLASH : ORIGIN = .., LENGTH = ..` in memory.x");

    fs::write(
        out.join("app_flash_origin.rs"),
        format!(
            "/// Address this image is linked at, parsed from `memory.x` at build time.\n\
             pub const APP_FLASH_ORIGIN: u32 = {:#010x};\n\
             /// First address past the application region, also from `memory.x`.\n\
             ///\n\
             /// This is where the 40 KB the bootloader reserves for application\n\
             /// data begins, and therefore where anything that has to survive a\n\
             /// firmware update has to live. See `src/store.rs`.\n\
             pub const APP_FLASH_END: u32 = {:#010x};\n",
            flash.origin,
            flash.end()
        ),
    )
    .expect("cannot write app_flash_origin.rs");

    fs::write(out.join("memory.x"), memory_x.as_bytes()).expect("cannot write memory.x");
    println!("cargo::rustc-link-search={}", out.display());

    println!("cargo::rerun-if-changed=memory.x");
    println!("cargo::rerun-if-changed=build.rs");
}

/// The short hash of `HEAD`, with `-dirty` if the tree had uncommitted
/// changes at build time, or `unknown` outside a git checkout (a source
/// tarball, say). Never fails the build: an image with no build id is
/// better than no image.
fn build_id() -> String {
    // Rebuild when the checked-out commit changes: `HEAD` moves on a checkout,
    // and the file it points at moves on a commit.
    println!("cargo::rerun-if-changed=.git/HEAD");
    if let Ok(head) = fs::read_to_string(".git/HEAD") {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            println!("cargo::rerun-if-changed=.git/{reference}");
        }
    }
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let Some(sha) = git(&["rev-parse", "--short=7", "HEAD"]).filter(|s| !s.is_empty()) else {
        return "unknown".to_string();
    };
    let dirty =
        git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    if dirty {
        format!("{sha}-dirty")
    } else {
        sha
    }
}
