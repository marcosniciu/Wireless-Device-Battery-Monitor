//! BatteryHub Taskbar setup: install / update / uninstall the taskbar edition.
#[path = "../setup.rs"]
mod setup;

const PAYLOAD: &[u8] = include_bytes!(concat!(env!("BH_PAYLOAD_DIR"), "/batteryhub-taskbar.exe"));

fn main() {
    setup::run(batteryhub::install::TASKBAR, PAYLOAD);
}
