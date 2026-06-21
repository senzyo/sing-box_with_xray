use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::ptr::null;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_Status, CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW,
    CM_Locate_DevNodeW, CR_SUCCESS, DN_HAS_PROBLEM, DN_STARTED,
};

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn main() {
    let log_path = r"C:\Users\admin\Desktop\sing-box-with-xray\detect_log.txt";
    let mut log = fs::File::create(log_path).unwrap();

    writeln!(log, "=== detect_wintun started ===").unwrap();

    unsafe {
        let mut size = 0u32;
        let ret = CM_Get_Device_ID_List_SizeW(&mut size, null(), 0);
        writeln!(log, "SizeW ret=0x{:X} size={}", ret, size).unwrap();

        if ret != CR_SUCCESS || size == 0 {
            writeln!(log, "FAIL: ret={:X} or size=0", ret).unwrap();
            return;
        }

        let mut buf: Vec<u16> = vec![0u16; size as usize];
        let ret = CM_Get_Device_ID_ListW(null(), buf.as_mut_ptr(), size, 0);
        writeln!(log, "ListW ret=0x{:X}", ret).unwrap();

        if ret != CR_SUCCESS {
            writeln!(log, "FAIL: ListW ret={:X}", ret).unwrap();
            return;
        }

        let mut total = 0u32;
        let mut win_total = 0u32;
        let mut start = 0usize;
        while start < buf.len() {
            let end = buf[start..].iter().position(|&c| c == 0)
                .map(|p| start + p).unwrap_or(buf.len());
            if end == start { break; }
            let id = String::from_utf16_lossy(&buf[start..end]);
            total += 1;

            if id.to_uppercase().contains("WINTUN") {
                win_total += 1;
                writeln!(log, "WINTUN #{}: {}", win_total, id).unwrap();

                let mut dev_inst = 0u32;
                let wid = wide(&id);
                let lr = CM_Locate_DevNodeW(&mut dev_inst, wid.as_ptr(), 0);
                writeln!(log, "  Locate ret=0x{:X} inst={}", lr, dev_inst).unwrap();

                if lr == 0xD {
                    // CR_NO_SUCH_DEVNODE - device is ghosted, definitely orphaned
                    writeln!(log, "  -> ORPHANED (no devnode)").unwrap();
                } else if lr == CR_SUCCESS {
                    let mut st = 0u32;
                    let mut pr = 0u32;
                    let sr = CM_Get_DevNode_Status(&mut st, &mut pr, dev_inst, 0);
                    writeln!(log, "  Status ret=0x{:X} status=0x{:X} problem=0x{:X}", sr, st, pr).unwrap();
                    writeln!(log, "  STARTED={} HAS_PROBLEM={}",
                        if (st & DN_STARTED) != 0 { "Y" } else { "N" },
                        if (st & DN_HAS_PROBLEM) != 0 { "Y" } else { "N" },
                    ).unwrap();
                }
            }

            start = end + 1;
        }
        writeln!(log, "TOTAL devices={} WINTUN={}", total, win_total).unwrap();
    }
}
