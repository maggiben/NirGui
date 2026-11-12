//! Command line tool to exercise the NIRscan Nano protocol without the GUI.
//!
//!   nano-cli info                 device versions, serial, status
//!   nano-cli sensors              temperatures, humidity, battery
//!   nano-cli configs              list stored scan configurations
//!   nano-cli scan [out.dat]       run a scan with the active config, print spectrum
//!   nano-cli fetch-ref <dir>      save factory reference blobs (refcal.dat, refmatrix.dat)
//!   nano-cli interpret <scan.dat> [refcal.dat refmatrix.dat]

use hidapi::HidApi;
use nirscan_nano_lib::dlpspec::{interpret, SlewScanConfig};
use nirscan_nano_lib::nano::{file_type, fmt_dlpc_ver, fmt_ver, Nano};
use std::error::Error;

fn open() -> Result<Nano, Box<dyn Error>> {
    let api = HidApi::new()?;
    Ok(Nano::open(&api)?)
}

fn print_results(r: &nirscan_nano_lib::dlpspec::ScanResults, reference: Option<&nirscan_nano_lib::dlpspec::ScanResults>) {
    println!("scan '{}' cfg '{}' serial {} pga {} points {}", r.head.scan_name, r.cfg.config_name, r.head.serial_number, r.head.pga, r.length);
    println!("wavelength_nm,intensity{}", if reference.is_some() { ",reference,absorbance" } else { "" });
    for i in 0..r.length {
        match reference {
            Some(rf) => println!(
                "{:.3},{},{},{:.5}",
                r.wavelength[i],
                r.intensity[i],
                rf.intensity[i],
                interpret::absorbance(r.intensity[i], rf.intensity[i])
            ),
            None => println!("{:.3},{}", r.wavelength[i], r.intensity[i]),
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("info");
    match cmd {
        "info" => {
            let mut d = open()?;
            let v = d.versions()?;
            println!("path        {}", d.path);
            println!("serial      {}", d.serial_number()?);
            println!("model       {}", d.model_name().unwrap_or_default());
            println!("tiva fw     {}", fmt_ver(v[0]));
            println!("dlpc sw     {}", fmt_dlpc_ver(v[1]));
            println!("dlpc flash  {}", fmt_dlpc_ver(v[2]));
            println!("speclib     {}", fmt_ver(v[3]));
            println!("cal/ref/cfg {} / {} / {}", v[4], v[5], v[6]);
            println!("status      0x{:08X}", d.device_status()?);
            let (e, codes) = d.error_status()?;
            println!("errors      0x{:08X} {:?}", e, codes);
        }
        "sensors" => {
            let mut d = open()?;
            let (a, det) = d.read_temp()?;
            let (h, ht) = d.read_humidity()?;
            println!("ambient {:.2} C, detector {:.2} C", a as f64 / 100.0, det as f64 / 100.0);
            println!("humidity {:.2} %, hdc temp {:.2} C", h as f64 / 100.0, ht as f64 / 100.0);
            println!("battery {:.2} V, tiva {:.2} C", d.read_battery()? as f64 / 100.0, d.read_tiva_temp()? as f64 / 100.0);
            println!("date {:?}", d.get_date_time()?);
        }
        "configs" => {
            let mut d = open()?;
            let n = d.num_configs()?;
            let act = d.active_config()?;
            for i in 0..n {
                let blob = d.read_config_blob(i)?;
                let c = SlewScanConfig::from_blob(&blob)?;
                println!("{}{} {:?}", if i == act { "*" } else { " " }, i, c);
            }
        }
        "scan" => {
            let mut d = open()?;
            // same preparation as the GUI app (and TI GUI): auto PGA, lamp controlled by scan
            d.set_fixed_pga(false, 64)?;
            d.set_lamp(if std::env::var("LAMP").as_deref() == Ok("on") { nirscan_nano_lib::nano::LampMode::On } else { nirscan_nano_lib::nano::LampMode::Auto })?;
            d.set_num_repeats(6)?;
            eprintln!("estimated {} ms", d.estimated_scan_time_ms()?);
            let t0 = std::time::Instant::now();
            let mut last = 0xFFFF_FFFFu32;
            let blob = d.scan(false, |st| {
                if st != last {
                    eprintln!("{:>6} ms status 0x{:02X}", t0.elapsed().as_millis(), st);
                    last = st;
                }
            })?;
            eprintln!("scan blob {} bytes after {} ms", blob.len(), t0.elapsed().as_millis());
            if let Some(p) = args.get(2) {
                std::fs::write(p, &blob)?;
                eprintln!("raw scan saved to {}", p);
            }
            let r = interpret::interpret(&blob)?;
            let cal = d.read_file(file_type::REF_CAL_DATA)?;
            let mat = d.read_file(file_type::REF_CAL_MATRIX)?;
            let rf = interpret::interp_reference(&cal, &mat, &r)?;
            print_results(&r, Some(&rf));
        }
        "test" => {
            use nirscan_nano_lib::nano::cmd;
            let mut d = open()?;
            for (n, c) in [("eeprom", cmd::EEPROM_TEST), ("adc", cmd::ADC_TEST), ("tmp006", cmd::TMP_TEST), ("hdc1000", cmd::HDC_TEST), ("sdram", cmd::SDRAM_TEST), ("sdcard", cmd::SDC_TEST)] {
                println!("{:8} {:?}", n, d.self_test(c));
            }
            println!("bq       {:?}", d.bq_test());
            println!("errors   {:?}", d.error_status());
        }
        "lamp" => {
            let mut d = open()?;
            let on = args.get(2).map(String::as_str) == Some("on");
            d.set_lamp(if on { nirscan_nano_lib::nano::LampMode::On } else { nirscan_nano_lib::nano::LampMode::Auto })?;
            println!("lamp {} ; status 0x{:X} errors {:?}", if on { "on" } else { "auto" }, d.device_status()?, d.error_status()?);
        }
        "reset" => {
            let mut d = open()?;
            d.reset()?;
            println!("reset sent");
        }
        "fetch-ref" => {
            let dir = args.get(2).cloned().unwrap_or_else(|| ".".into());
            let mut d = open()?;
            std::fs::write(format!("{}/refcal.dat", dir), d.read_file(file_type::REF_CAL_DATA)?)?;
            std::fs::write(format!("{}/refmatrix.dat", dir), d.read_file(file_type::REF_CAL_MATRIX)?)?;
            println!("saved refcal.dat and refmatrix.dat in {}", dir);
        }
        "interpret" => {
            let blob = std::fs::read(args.get(2).ok_or("missing scan file")?)?;
            let r = interpret::interpret(&blob)?;
            let rf = match (args.get(3), args.get(4)) {
                (Some(c), Some(m)) => Some(interpret::interp_reference(&std::fs::read(c)?, &std::fs::read(m)?, &r)?),
                _ => None,
            };
            print_results(&r, rf.as_ref());
        }
        _ => eprintln!("usage: nano-cli [info|sensors|configs|scan [out.dat]|fetch-ref <dir>|interpret <scan.dat> [refcal refmatrix]]"),
    }
    Ok(())
}
