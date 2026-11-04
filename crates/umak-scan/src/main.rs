//! Headless scan and benchmark tool that runs the core without a UI.

fn main() {
    let info = umak_core::app_info();
    println!("{} core {}", info.name, info.version);
}
