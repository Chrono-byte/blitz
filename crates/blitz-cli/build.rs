// SPDX-License-Identifier: MPL-2.0
fn main() {
  let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
  let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

  if target_os == "windows" {
    let stack_size = 16_i32 * 1024_i32 * 1024_i32;
    if target_env == "msvc" {
      println!("cargo:rustc-link-arg-bin=blitz=/STACK:{stack_size}");
    } else {
      println!("cargo:rustc-link-arg-bin=blitz=-Wl,--stack,{stack_size}");
    }
  }
}
