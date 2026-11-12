# DLP NIRscan Nano: USB HID protocol

Reverse engineered from the TI Windows GUI 2.1.0 sources (`gui/Sources/API.cpp`,
`usb.cpp`, `Common/include/NNOCommandDefs.h`) and the Tiva TM4C129 firmware
2.1.0 (`DLPNIRscanNanoSoftware_2.1.0/Sources/App/usbCmdHandler.c`, `cmdProc.c`,
`cmdDict.c`). The Rust implementation lives in `src-tauri/src/nano.rs`.

## Transport

| Item | Value |
|---|---|
| USB VID:PID | `0451:4200` (Texas Instruments) |
| Class | HID, generic (no report IDs; hidapi uses report ID 0) |
| Report size | 64 bytes in, 64 bytes out (interrupt endpoints) |
| Host timeout | 20 s (40 s for flash checksum and self-tests) |

A **message** can be longer than a report. The host sends it in consecutive
64-byte output reports; the firmware gathers reports until it has
`4 + length` bytes (rounded up to 64). Responses come back the same way.

## Message format

```
byte 0     flags
            bit 7    rw      1 = read, 0 = write
            bit 6    reply   1 = host wants a reply (always 1)
            bits 5-4 resp    response code: 0 OK, 1 error/NACK, 2 busy
            bits 3-2 reserved (0)
            bits 1-0 dest    0 = command endpoint
byte 1     sequence number (writes increment it, reads use 0)
byte 2-3   length, little endian = number of bytes after the header
byte 4-5   command, little endian = (CMD2 << 8) | CMD3     [requests only]
byte 6..   parameters (little endian)                       [requests only]
```

* Read request: flags `0xC0`. Write request: flags `0x40`.
* The firmware accepts `2 <= length <= 512`.
* Response: same 4-byte header, then `length` payload bytes (no command echo).
  `resp = 2` means another interface (Bluetooth) currently owns the device.
  A read response with `length = 0` is treated as NACK.
* Write responses usually have `length = 0`; some return a 4-byte value
  (e.g. `SCAN_CFG_APPLY` returns the number of patterns, negative on error).

Example: read Tiva version (command `0x0216`):

```
out report: 00 | C0 00 02 00 16 02 00 00 ... (zero padded to 64)
in  report: C0 00 1C 00 <28 bytes: 7 x uint32 versions>
```

## Commands

Codes are `(CMD2 << 8) | CMD3`. "R" = read (`0xC0`), "W" = write (`0x40`).

### Status and information

| Code | Dir | Name | Params | Response |
|---|---|---|---|---|
| 0x0216 | R | TIVA_VER | – | 7 x u32: Tiva FW, DLPC SW, DLPC flash, spectrum lib, cal ver, ref-cal ver, cfg ver. Tiva/speclib are `MAJOR<<16 | MINOR<<8 | PATCH` |
| 0x0233 | R | SERIAL_NUMBER_READ | – | 8 chars |
| 0x023C | R | MODEL_NAME_READ | – | 16 chars |
| 0x0403 | R | READ_DEVICE_STATUS | – | u32 bits: 0x01 Tiva OK, 0x02 scan in progress, 0x04 SD present, 0x08 SD busy, 0x10 BLE stack open, 0x20 BLE connected, 0x40 scan interpret in progress |
| 0x0404 | R | READ_ERROR_STATUS | – | u32 error bits (0x1 scan, 0x2 ADC, 0x4 SD, 0x8 EEPROM, 0x10 BLE, 0x20 speclib, 0x40 HW, 0x80 TMP006, 0x100 HDC1000, 0x200 battery empty, 0x400 memory, 0x800 UART) + per-subsystem error code bytes |
| 0x0405 | W | RESET_ERROR_STATUS | – | – |
| 0x0406 | R | GET_SPECIFIC_ERR_STATUS | u32 field | u8 |
| 0x0407 | R | GET_SPECIFIC_ERR_CODE | u32 type | u16 |

### Sensors

| Code | Dir | Name | Response |
|---|---|---|---|
| 0x0300 | R | READ_TEMP | i32 ambient, i32 detector (hundredths of °C) |
| 0x0302 | R | READ_HUM | i32 HDC temp (1/100 °C), u32 humidity (1/100 %) |
| 0x030A | R | READ_BATT_VOLT | u32, hundredths of a volt |
| 0x030B | R | READ_TIVA_TEMP | i32, hundredths of °C |
| 0x0402 | R | READ_PHOTODETECTOR | 3 x u32 (R, G, B lamp photodiode) |
| 0x030C | R | GET_DATE_TIME | year-2000, month, day, weekday, hour, min, sec (u8 each) |
| 0x0309 | W | SET_DATE_TIME | same 7 bytes |

### Scanning

| Code | Dir | Name | Params | Notes |
|---|---|---|---|---|
| 0x022E | W | SCAN_NUM_REPEATS | u16 | scans to average |
| 0x0237 | R | READ_SCAN_TIME | – | u32 estimated scan time in ms |
| 0x0234 | W | WRITE_SCAN_NAME_TAG | 14 bytes, NUL terminated | |
| 0x0218 | W | PERFORM_SCAN | u8 store_in_sd | starts the scan, returns immediately |
| 0x0219 | R | SCAN_GET_STATUS | – | u8 (1 = complete) |
| 0x040C | W | SET_FIXED_PGA | u8 is_fixed, u8 gain | gain 1..64; `0,64` = automatic |
| 0x021B / 0x0228 | W / R | SET_PGA / GET_PGA | u8 | |
| 0x0225 | W | SET_DLPC_ONOFF_CTRL | u8 | 1 = scan controls the lamp/DLPC |
| 0x0105 | W | DLPC_ENABLE | u8 enable, u8 lamp | |
| 0x0239 | W | START_SCAN_INTERPRET | – | device-side interpretation of the last scan |

GUI scan sequence: `SET_FIXED_PGA` → `SCAN_NUM_REPEATS` → `READ_SCAN_TIME` →
`PERFORM_SCAN` → poll `READ_DEVICE_STATUS` every 50 ms until bit 0x02 clears
(timeout = 3 x estimate) → read file type 0 (scan data).

### File transfer (device → host)

| Code | Dir | Name | Params | Response |
|---|---|---|---|---|
| 0x002D | R | FILE_GET_READSIZE | u32 file type | u32 size |
| 0x002E | R | FILE_GET_DATA | – | next chunk (≤ 512 bytes); repeat until size reached |

File types: 0 scan data, 1 scan config, 2 reference-cal scan data, 3 reference-cal
matrix, 4 Hadamard SNR data, 7 last scan stored on SD, 8 interpreted scan.

Type 8 (after `START_SCAN_INTERPRET` and waiting for status bit 0x40 to clear)
is a raw packed array: `i32 n` then `n x (f64 wavelength_nm, i32 intensity)`.

### Scan configurations (EEPROM, up to 20)

| Code | Dir | Name | Params | Response |
|---|---|---|---|---|
| 0x0222 | R | SCAN_CFG_NUM | – | u8 count |
| 0x0223 | R | SCAN_GET_ACT_CFG | – | u8 index |
| 0x0224 | W | SCAN_SET_ACT_CFG | u8 index | |
| 0x0220 | R | SCAN_CFG_READ | u32 index | u32 size + serialized config |
| 0x021F | W | SCAN_CFG_SAVE | u8 index, u8 size, serialized config | |
| 0x021E | W | SCAN_CFG_APPLY | serialized config | i32 pattern count (temporary, not stored) |
| 0x0221 | W | SCAN_CFG_ERASEALL | – | |

The TI GUI always erases all configs and writes the full list again.

### Utilities, tests, maintenance

| Code | Dir | Name | Params |
|---|---|---|---|
| 0x0400 | R | GET_NUM_SCAN_FILES_SD | → u32 |
| 0x0238 | W | DEL_LAST_SCAN_FILE_SD | |
| 0x030E / 0x030F | W / R | SET / GET_HIBERNATE | u8 |
| 0x030D | W | HIBERNATE_MODE | no reply |
| 0x021A | W | TIVA_RESET | no reply |
| 0x022A | R | CALIB_STRUCT_READ | → serialized calibCoeffs |
| 0x0101..0x0109 | R | EEPROM/ADC/BQ/SDRAM/TMP/HDC/SDC tests | → u8 (0 = pass); BQ returns 5 x i32 |
| 0x010B | W | LED_TEST | u8 |
| 0x010C / 0x010D | R / W | BUTTON_TEST_RD / WR | u8 |

Not implemented in the Mac app on purpose (factory / risky): calibration writes
(0x0229, 0x022D, 0x0226, 0x0230, 0x040A), serial/model writes, EEPROM wipes,
DLPC flash erase/update and Tiva bootloader (0x002F, 0x0025, 0x002A).

## Data formats (TPL serialization)

The spectrum library serializes structs with [TPL](http://troydhanson.github.io/tpl/):

```
"tpl" | flags u8 | total_len u32 | format string\0 | u32 per '#' (array lengths) | packed data
```

Types: `c` u8, `v` u16, `j` i16, `u` u32, `i` i32, `f` f64, `#` fixed array,
`$( )` nested struct, `S( )` root struct. Everything little endian.

| Blob | Format |
|---|---|
| scan config (legacy) | `S(cvc#c#vvcvv)` |
| scan config (slew) | `S(cvc#c#vc)` followed by `S(ccvvvv)#` (always 5 sections) |
| scan data (legacy) | `S(uc#cccccccjjvvu$(f#f#)c#vccccvc#c#vvcvvi#)` |
| scan data (slew, firmware 2.x) | `S(uc#cccccccjjvvu$(f#f#)c#vccc)` + slew config (2 images) + `i#` (864 ADC samples) |
| reference cal matrix | `S(c#f#v##)` (19 widths, 50 wavelengths, 19x50 u16) |
| calibration coeffs | `S(f#f#)` (3 shift-vector + 3 pixel→nm coefficients) |

Slew section: `scan_type` (0 column, 1 Hadamard), `width_px`, `start_nm`,
`end_nm`, `num_patterns`, `exposure_time` (0..6 = 0.635, 1.27, 2.45, 5.08,
15.24, 30.48, 60.96 ms).

## Turning ADC data into a spectrum

Implemented in `src-tauri/src/dlpspec/` (port of TI DLP Spectrum Library 2.x):

1. A black (all-off) pattern is inserted every `black_pattern_period` patterns.
   Their mean is the DC level; it is subtracted and the black samples removed.
2. Each slew section is processed independently:
   * **Column**: one ADC sample per pattern. Pattern centre columns are spaced
     evenly between the DMD columns of `start_nm` and `end_nm`.
   * **Hadamard**: column groups are split into sets; each set is measured with
     the rows of a Paley S-matrix of prime order p ≡ 3 (mod 4). Intensities are
     recovered with `x = y · (S − ½) / ((p+1)/4)`.
3. Wavelength of each point: `nm = c0 + c1·col + c2·col²` using the device's
   pixel→nm calibration (half a column is subtracted for even pattern widths).
4. Reference: the factory reference scan (file type 2) is interpreted the same
   way, interpolated onto the sample wavelengths, corrected for pattern width
   with the reference-cal matrix (file type 3) and scaled for PGA gain.
5. Absorbance = −log10(sample / reference), reflectance = sample / reference.
