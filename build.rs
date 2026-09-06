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

use oxinode_core::linker_script;

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

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
