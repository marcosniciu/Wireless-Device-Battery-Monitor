//! Console diagnostic: prints every detected device (and with --dump every HID collection).
fn main() {
    if std::env::args().any(|a| a == "--update") {
        println!("this build: {}", batteryhub::install::VERSION);
        let c = batteryhub::update::check();
        println!("{c:?}");
        if let (batteryhub::update::Check::Available(r), true) = (&c, std::env::args().any(|a| a == "--download")) {
            println!("downloaded and verified: {:?}", batteryhub::update::download(r));
        }
        return;
    }
    if std::env::args().any(|a| a == "--dump") {
        print!("{}", batteryhub::device::dump_hid_collections());
    }
    let t = std::time::Instant::now();
    let devices = batteryhub::poll_all_devices();
    for d in &devices {
        println!("{:<11} {:<40} {:<14} {:<22} {} {}", d.kind.label(), d.name, d.status_text(), d.via, d.key, d.detail);
    }
    println!("-- {} devices in {:?}", devices.len(), t.elapsed());
    if std::env::args().any(|a| a == "--ble") {
        batteryhub::ble::ensure_watcher();
        std::thread::sleep(std::time::Duration::from_secs(4));
        println!("-- BLE adverts in 4 s: {}", batteryhub::ble::received_count());
    }
}
